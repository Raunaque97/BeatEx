//! Releases the reserves committed by the latest active order back into the
//! trader's encrypted balance. Replacement composes this proof with the normal
//! order-submission proof in one transaction; cancellation needs only this
//! proof and enqueues an identity replacement event.

use super::*;
use crate::transition::{
    ciphertext_bytes, context_pubkey, decode_wire_point, field_bytes, field_bytes_le,
    field_from_bytes_le, g1_bytes, g2_bytes, pubkey_context, scalar_bytes, scalar_from_bytes,
    wire_point_bytes, OrderProofOutput, OrderProofRequest, OrderProver, PrivateBalanceOpening,
    SolanaVerificationKey,
};
use ark_bn254::Bn254;
use ark_ec::AffineRepr;
use ark_groth16::{Groth16, ProvingKey, VerifyingKey};
use ark_r1cs_std::prelude::ToBitsGadget;
use ark_snark::SNARK;
use rand::{CryptoRng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use std::ops::Neg;

const RELEASE_DOMAIN: &[u8] = b"BeatEx collateral release v2";
const ORDER_COMMITMENT_DOMAIN: &[u8] = b"BeatEx private contingent order v2";
const BALANCE_BITS: usize = 64;

#[derive(Clone, Debug)]
pub struct CollateralReleaseWitness {
    pub market_context: [Fq; 2],
    pub auction_context: [Fq; 2],
    pub active_order_context: [Fq; 2],
    pub owner_context: [Fq; 2],
    pub balance_revision: u64,
    pub client_order_nonce: u64,
    pub buy: [u64; LEVELS],
    pub sell: [u64; LEVELS],
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub order_salt: Fq,
    pub old_base_balance: u64,
    pub old_quote_balance: u64,
    pub committee_key: EdwardsProjective,
    /// old base, old quote, released base, released quote.
    pub balance_randomness: [JubjubScalar; 4],
}

impl CollateralReleaseWitness {
    pub fn deterministic() -> Self {
        let order = OrderWitness::deterministic();
        Self {
            market_context: [Fq::from(0x4d41u64), Fq::from(0x524bu64)],
            auction_context: order.auction_context,
            active_order_context: [Fq::from(0x4143u64), Fq::from(0x5449u64)],
            owner_context: order.owner_context,
            balance_revision: 4,
            client_order_nonce: order.client_order_nonce,
            buy: order.buy,
            sell: order.sell,
            base_reserve: order.base_reserve as u64,
            quote_reserve: order.quote_reserve as u64,
            order_salt: Fq::from_le_bytes_mod_order(&[0x91; 32]),
            old_base_balance: 80,
            old_quote_balance: 8_000_000,
            committee_key: order.committee_key,
            balance_randomness: [
                JubjubScalar::from(0xe001u64),
                JubjubScalar::from(0xe002u64),
                JubjubScalar::from(0xe003u64),
                JubjubScalar::from(0xe004u64),
            ],
        }
    }

    pub fn order_commitment(&self) -> Fq {
        let mut payload = vec![Fq::from_le_bytes_mod_order(ORDER_COMMITMENT_DOMAIN)];
        payload.extend_from_slice(&self.auction_context);
        payload.extend_from_slice(&self.owner_context);
        payload.push(Fq::from(self.client_order_nonce));
        for index in 0..LEVELS {
            payload.push(Fq::from(self.buy[index]));
            payload.push(Fq::from(self.sell[index]));
        }
        payload.push(Fq::from(self.base_reserve));
        payload.push(Fq::from(self.quote_reserve));
        payload.push(self.order_salt);
        transcript_hash_native(&payload)
    }

    pub(crate) fn balance_ciphertexts(&self) -> [Ciphertext; 4] {
        let released_base = self
            .old_base_balance
            .checked_add(self.base_reserve)
            .expect("base release fits u64");
        let released_quote = self
            .old_quote_balance
            .checked_add(self.quote_reserve)
            .expect("quote release fits u64");
        [
            encrypt(
                self.old_base_balance.into(),
                self.balance_randomness[0],
                self.committee_key,
            ),
            encrypt(
                self.old_quote_balance.into(),
                self.balance_randomness[1],
                self.committee_key,
            ),
            encrypt(
                released_base.into(),
                self.balance_randomness[2],
                self.committee_key,
            ),
            encrypt(
                released_quote.into(),
                self.balance_randomness[3],
                self.committee_key,
            ),
        ]
    }

    pub fn digest(&self) -> Fq {
        let mut payload = vec![Fq::from_le_bytes_mod_order(RELEASE_DOMAIN), Fq::from(2u64)];
        payload.extend_from_slice(&self.market_context);
        payload.extend_from_slice(&self.auction_context);
        payload.extend_from_slice(&self.active_order_context);
        payload.extend_from_slice(&self.owner_context);
        payload.push(Fq::from(self.balance_revision));
        payload.push(Fq::from(self.client_order_nonce));
        payload.push(self.order_commitment());
        append_wire_point_native(&mut payload, self.committee_key.into_affine());
        for ciphertext in self.balance_ciphertexts() {
            append_wire_point_native(&mut payload, ciphertext.ephemeral);
            append_wire_point_native(&mut payload, ciphertext.body);
        }
        transcript_hash_native(&payload)
    }
}

#[derive(Clone)]
pub struct CollateralReleaseCircuit {
    pub witness: Option<CollateralReleaseWitness>,
    pub digest: Option<Fq>,
}

impl CollateralReleaseCircuit {
    pub const fn blank() -> Self {
        Self {
            witness: None,
            digest: None,
        }
    }

    pub fn from_witness(witness: CollateralReleaseWitness) -> Self {
        let digest = Some(witness.digest());
        Self {
            witness: Some(witness),
            digest,
        }
    }
}

impl ConstraintSynthesizer<Fq> for CollateralReleaseCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fq>) -> Result<(), SynthesisError> {
        let witness = self.witness.as_ref();
        let market = alloc_context(cs.clone(), witness.map(|value| value.market_context))?;
        let auction = alloc_context(cs.clone(), witness.map(|value| value.auction_context))?;
        let active_order =
            alloc_context(cs.clone(), witness.map(|value| value.active_order_context))?;
        let owner = alloc_context(cs.clone(), witness.map(|value| value.owner_context))?;
        let (_, revision) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.balance_revision)),
            64,
        )?;
        let (_, nonce) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.client_order_nonce)),
            64,
        )?;

        let mut buy = Vec::with_capacity(LEVELS);
        let mut sell = Vec::with_capacity(LEVELS);
        for index in 0..LEVELS {
            let (_, buy_value) = alloc_uint(
                cs.clone(),
                witness.map(|value| u128::from(value.buy[index])),
                QUANTITY_BITS,
            )?;
            let (_, sell_value) = alloc_uint(
                cs.clone(),
                witness.map(|value| u128::from(value.sell[index])),
                QUANTITY_BITS,
            )?;
            buy.push(buy_value);
            sell.push(sell_value);
        }
        let (_, base_reserve) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.base_reserve)),
            BALANCE_BITS,
        )?;
        let (_, quote_reserve) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.quote_reserve)),
            BALANCE_BITS,
        )?;
        let salt = FpVar::new_witness(cs.clone(), || {
            witness
                .map(|value| value.order_salt)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let mut commitment_payload = vec![FpVar::constant(Fq::from_le_bytes_mod_order(
            ORDER_COMMITMENT_DOMAIN,
        ))];
        commitment_payload.extend(auction.iter().cloned());
        commitment_payload.extend(owner.iter().cloned());
        commitment_payload.push(nonce.clone());
        for index in 0..LEVELS {
            commitment_payload.push(buy[index].clone());
            commitment_payload.push(sell[index].clone());
        }
        commitment_payload.push(base_reserve.clone());
        commitment_payload.push(quote_reserve.clone());
        commitment_payload.push(salt);
        let order_commitment = transcript_hash_gadget(&commitment_payload)?;

        let old_base_value = witness.map(|value| u128::from(value.old_base_balance));
        let old_quote_value = witness.map(|value| u128::from(value.old_quote_balance));
        let released_base_value = witness.and_then(|value| {
            value
                .old_base_balance
                .checked_add(value.base_reserve)
                .map(u128::from)
        });
        let released_quote_value = witness.and_then(|value| {
            value
                .old_quote_balance
                .checked_add(value.quote_reserve)
                .map(u128::from)
        });
        let (old_base_bits, old_base) = alloc_uint(cs.clone(), old_base_value, BALANCE_BITS)?;
        let (old_quote_bits, old_quote) = alloc_uint(cs.clone(), old_quote_value, BALANCE_BITS)?;
        let (released_base_bits, released_base) =
            alloc_uint(cs.clone(), released_base_value, BALANCE_BITS)?;
        let (released_quote_bits, released_quote) =
            alloc_uint(cs.clone(), released_quote_value, BALANCE_BITS)?;
        (&old_base + &base_reserve).enforce_equal(&released_base)?;
        (&old_quote + &quote_reserve).enforce_equal(&released_quote)?;

        let committee_key = EdwardsVar::new_witness(cs.clone(), || {
            witness
                .map(|value| value.committee_key)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let randomness = witness.map(|value| &value.balance_randomness);
        let messages = [
            old_base_bits,
            old_quote_bits,
            released_base_bits,
            released_quote_bits,
        ];
        let mut ciphertexts = Vec::with_capacity(4);
        for (index, message) in messages.iter().enumerate() {
            let randomness_bits = randomness.map(|values| {
                let bigint = values[index].into_bigint();
                (0..RANDOMNESS_BITS)
                    .map(|bit| bigint.get_bit(bit))
                    .collect::<Vec<_>>()
            });
            ciphertexts.push(encrypt_gadget(
                cs.clone(),
                message,
                randomness_bits,
                &committee_key,
            )?);
        }

        let mut statement = vec![
            FpVar::constant(Fq::from_le_bytes_mod_order(RELEASE_DOMAIN)),
            FpVar::constant(Fq::from(2u64)),
        ];
        statement.extend(market);
        statement.extend(auction);
        statement.extend(active_order);
        statement.extend(owner);
        statement.push(revision);
        statement.push(nonce);
        statement.push(order_commitment);
        append_wire_point_gadget(&mut statement, &committee_key)?;
        for (ephemeral, body) in ciphertexts {
            append_wire_point_gadget(&mut statement, &ephemeral)?;
            append_wire_point_gadget(&mut statement, &body)?;
        }
        let expected =
            FpVar::new_input(cs, || self.digest.ok_or(SynthesisError::AssignmentMissing))?;
        transcript_hash_gadget(&statement)?.enforce_equal(&expected)
    }
}

fn alloc_context(
    cs: ConstraintSystemRef<Fq>,
    value: Option<[Fq; 2]>,
) -> Result<Vec<FpVar<Fq>>, SynthesisError> {
    (0..2)
        .map(|index| {
            FpVar::new_witness(cs.clone(), || {
                value
                    .map(|value| value[index])
                    .ok_or(SynthesisError::AssignmentMissing)
            })
        })
        .collect()
}

fn append_wire_point_native(payload: &mut Vec<Fq>, point: EdwardsAffine) {
    payload.push(point.y);
    payload.push(Fq::from(u64::from(point.x.into_bigint().get_bit(0))));
}

fn append_wire_point_gadget(
    payload: &mut Vec<FpVar<Fq>>,
    point: &EdwardsVar,
) -> Result<(), SynthesisError> {
    let x_bits = point.x.to_bits_le()?;
    payload.push(point.y.clone());
    payload.push(x_bits[0].clone().into());
    Ok(())
}

#[derive(Clone, Debug)]
pub struct SolanaCollateralReleaseFixture {
    pub vk_alpha_g1: [u8; 64],
    pub vk_beta_g2: [u8; 128],
    pub vk_gamma_g2: [u8; 128],
    pub vk_delta_g2: [u8; 128],
    pub vk_ic: [[u8; 64]; 2],
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub market: [u8; 32],
    pub auction: [u8; 32],
    pub active_order: [u8; 32],
    pub owner: [u8; 32],
    pub balance_revision: u64,
    pub client_order_nonce: u64,
    pub order_commitment: [u8; 32],
    pub committee_key: [u8; 32],
    pub balance_ciphertexts: [[u8; 64]; 4],
}

#[derive(Clone, Debug)]
pub struct CollateralReleaseProofRequest {
    pub market: [u8; 32],
    pub auction: [u8; 32],
    pub active_order: [u8; 32],
    pub owner: [u8; 32],
    pub balance_revision: u64,
    pub client_order_nonce: u64,
    pub buy: [u64; LEVELS],
    pub sell: [u64; LEVELS],
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub order_salt: [u8; 32],
    pub expected_order_commitment: [u8; 32],
    pub committee_key: [u8; 32],
    pub old_balance: PrivateBalanceOpening,
}

#[derive(Clone, Debug)]
pub struct CollateralReleaseProofOutput {
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub released_encrypted_base: [u8; 64],
    pub released_encrypted_quote: [u8; 64],
    pub released_balance: PrivateBalanceOpening,
}

pub struct CollateralReleaseProver {
    proving_key: ProvingKey<Bn254>,
}

pub struct ReplacementProver {
    release: CollateralReleaseProver,
    order: OrderProver,
}

#[derive(Clone, Debug)]
pub struct ReplacementProofOutput {
    pub release: CollateralReleaseProofOutput,
    pub replacement: OrderProofOutput,
}

impl ReplacementProver {
    pub fn deterministic_devnet() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            release: CollateralReleaseProver::deterministic_devnet()?,
            order: OrderProver::deterministic_devnet()?,
        })
    }

    /// Prove the atomic release-and-relock transition. The replacement's old
    /// balance is overwritten with the exact opening produced by the release,
    /// preventing a caller from accidentally creating two unrelated proofs.
    pub fn prove<R: RngCore + CryptoRng>(
        &self,
        release_request: CollateralReleaseProofRequest,
        mut replacement_request: OrderProofRequest,
        rng: &mut R,
    ) -> Result<ReplacementProofOutput, Box<dyn std::error::Error>> {
        if release_request.auction != replacement_request.auction
            || release_request.owner != replacement_request.owner
            || release_request.committee_key != replacement_request.committee_key
        {
            return Err(std::io::Error::other(
                "release and replacement must share auction, owner, and committee key",
            )
            .into());
        }
        let release = self.release.prove(release_request, rng)?;
        replacement_request.old_balance = release.released_balance.clone();
        let replacement = self.order.prove(replacement_request, rng)?;
        Ok(ReplacementProofOutput {
            release,
            replacement,
        })
    }

    pub fn release_verification_key(
        &self,
    ) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
        self.release.verification_key()
    }

    pub fn order_verification_key(
        &self,
    ) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
        self.order.verification_key()
    }
}

impl CollateralReleaseProver {
    /// Load ceremony or environment-specific parameters outside the browser
    /// setup path. Compressed Arkworks serialization is validated strictly.
    pub fn from_compressed_bytes(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            proving_key: deserialize_proving_key(bytes)?,
        })
    }

    pub fn to_compressed_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        serialize_proving_key(&self.proving_key)
    }

    pub fn from_uncompressed_bytes(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            proving_key: deserialize_uncompressed_proving_key(bytes)?,
        })
    }

    /// Loads an uncompressed proving key whose bytes were authenticated by the caller.
    pub fn from_trusted_uncompressed_bytes(
        bytes: &[u8],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            proving_key: deserialize_trusted_uncompressed_proving_key(bytes)?,
        })
    }

    pub fn to_uncompressed_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        serialize_uncompressed_proving_key(&self.proving_key)
    }

    /// Deterministic setup for Devnet testing only. Production must replace
    /// this with ceremony-generated parameters loaded from an artifact.
    pub fn deterministic_devnet() -> Result<Self, Box<dyn std::error::Error>> {
        let mut setup_rng = ChaCha20Rng::from_seed([0xe1; 32]);
        Ok(Self {
            proving_key: setup(&mut setup_rng)?,
        })
    }

    pub fn verification_key(&self) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
        let key = &self.proving_key.vk;
        let ic = key
            .gamma_abc_g1
            .iter()
            .copied()
            .map(g1_bytes)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| std::io::Error::other("release key must have two IC points"))?;
        Ok(SolanaVerificationKey {
            alpha_g1: g1_bytes(key.alpha_g1),
            beta_g2: g2_bytes(key.beta_g2),
            gamma_g2: g2_bytes(key.gamma_g2),
            delta_g2: g2_bytes(key.delta_g2),
            ic,
        })
    }

    pub fn prove<R: RngCore + CryptoRng>(
        &self,
        request: CollateralReleaseProofRequest,
        rng: &mut R,
    ) -> Result<CollateralReleaseProofOutput, Box<dyn std::error::Error>> {
        if request
            .buy
            .iter()
            .chain(&request.sell)
            .any(|quantity| *quantity > MAX_ORDER_QUANTITY)
        {
            return Err(std::io::Error::other("invalid private order quantities").into());
        }
        let released_base_value = request
            .old_balance
            .base_value
            .checked_add(request.base_reserve)
            .ok_or_else(|| std::io::Error::other("released base balance overflow"))?;
        let released_quote_value = request
            .old_balance
            .quote_value
            .checked_add(request.quote_reserve)
            .ok_or_else(|| std::io::Error::other("released quote balance overflow"))?;
        let released_base_randomness = JubjubScalar::rand(rng);
        let released_quote_randomness = JubjubScalar::rand(rng);
        let witness = CollateralReleaseWitness {
            market_context: pubkey_context(request.market),
            auction_context: pubkey_context(request.auction),
            active_order_context: pubkey_context(request.active_order),
            owner_context: pubkey_context(request.owner),
            balance_revision: request.balance_revision,
            client_order_nonce: request.client_order_nonce,
            buy: request.buy,
            sell: request.sell,
            base_reserve: request.base_reserve,
            quote_reserve: request.quote_reserve,
            order_salt: field_from_bytes_le(request.order_salt)?,
            old_base_balance: request.old_balance.base_value,
            old_quote_balance: request.old_balance.quote_value,
            committee_key: decode_wire_point(request.committee_key)?.into_group(),
            balance_randomness: [
                scalar_from_bytes(request.old_balance.base_randomness)?,
                scalar_from_bytes(request.old_balance.quote_randomness)?,
                released_base_randomness,
                released_quote_randomness,
            ],
        };
        if field_bytes_le(witness.order_commitment()) != request.expected_order_commitment {
            return Err(
                std::io::Error::other("private order does not match its commitment").into(),
            );
        }
        let digest = witness.digest();
        let balance_ciphertexts = witness.balance_ciphertexts().map(ciphertext_bytes);
        let proof = Groth16::<Bn254>::prove(
            &self.proving_key,
            CollateralReleaseCircuit::from_witness(witness),
            rng,
        )?;
        let prepared_key = Groth16::<Bn254>::process_vk(&self.proving_key.vk)?;
        if !Groth16::<Bn254>::verify_with_processed_vk(&prepared_key, &[digest], &proof)? {
            return Err(std::io::Error::other("generated release proof did not verify").into());
        }
        Ok(CollateralReleaseProofOutput {
            proof_a: g1_bytes(proof.a.neg()),
            proof_b: g2_bytes(proof.b),
            proof_c: g1_bytes(proof.c),
            public_input: field_bytes(digest),
            released_encrypted_base: balance_ciphertexts[2],
            released_encrypted_quote: balance_ciphertexts[3],
            released_balance: PrivateBalanceOpening {
                base_value: released_base_value,
                quote_value: released_quote_value,
                base_randomness: scalar_bytes(released_base_randomness),
                quote_randomness: scalar_bytes(released_quote_randomness),
            },
        })
    }
}

pub fn setup<R: RngCore + CryptoRng>(
    rng: &mut R,
) -> Result<ProvingKey<Bn254>, Box<dyn std::error::Error>> {
    let (proving_key, _) =
        Groth16::<Bn254>::circuit_specific_setup(CollateralReleaseCircuit::blank(), rng)?;
    Ok(proving_key)
}

pub fn prove<R: RngCore + CryptoRng>(
    proving_key: &ProvingKey<Bn254>,
    witness: CollateralReleaseWitness,
    rng: &mut R,
) -> Result<SolanaProofBytes, Box<dyn std::error::Error>> {
    let digest = witness.digest();
    let proof = Groth16::<Bn254>::prove(
        proving_key,
        CollateralReleaseCircuit::from_witness(witness),
        rng,
    )?;
    Ok((
        g1_bytes(proof.a.neg()),
        g2_bytes(proof.b),
        g1_bytes(proof.c),
        field_bytes(digest),
    ))
}

pub fn deterministic_solana_fixture(
) -> Result<SolanaCollateralReleaseFixture, Box<dyn std::error::Error>> {
    collateral_release_fixture(CollateralReleaseWitness::deterministic())
}

pub fn deterministic_solana_fixture_for_context(
    market: [u8; 32],
    auction: [u8; 32],
    active_order: [u8; 32],
    owner: [u8; 32],
) -> Result<SolanaCollateralReleaseFixture, Box<dyn std::error::Error>> {
    let mut witness = CollateralReleaseWitness::deterministic();
    witness.market_context = pubkey_context(market);
    witness.auction_context = pubkey_context(auction);
    witness.active_order_context = pubkey_context(active_order);
    witness.owner_context = pubkey_context(owner);
    collateral_release_fixture(witness)
}

fn collateral_release_fixture(
    witness: CollateralReleaseWitness,
) -> Result<SolanaCollateralReleaseFixture, Box<dyn std::error::Error>> {
    let commitment = field_bytes_le(witness.order_commitment());
    let balance_ciphertexts = witness.balance_ciphertexts().map(ciphertext_bytes);
    let market = context_pubkey(witness.market_context);
    let auction = context_pubkey(witness.auction_context);
    let active_order = context_pubkey(witness.active_order_context);
    let owner = context_pubkey(witness.owner_context);
    let balance_revision = witness.balance_revision;
    let client_order_nonce = witness.client_order_nonce;
    let committee_key = wire_point_bytes(witness.committee_key.into_affine());
    let mut setup_rng = ChaCha20Rng::from_seed([0xe1; 32]);
    let proving_key = setup(&mut setup_rng)?;
    let mut proof_rng = ChaCha20Rng::from_seed([0xe2; 32]);
    let (proof_a, proof_b, proof_c, public_input) = prove(&proving_key, witness, &mut proof_rng)?;
    let VerifyingKey {
        alpha_g1,
        beta_g2,
        gamma_g2,
        delta_g2,
        gamma_abc_g1,
    } = proving_key.vk;
    let vk_ic = gamma_abc_g1
        .into_iter()
        .map(g1_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "release key must have two IC points")?;
    Ok(SolanaCollateralReleaseFixture {
        vk_alpha_g1: g1_bytes(alpha_g1),
        vk_beta_g2: g2_bytes(beta_g2),
        vk_gamma_g2: g2_bytes(gamma_g2),
        vk_delta_g2: g2_bytes(delta_g2),
        vk_ic,
        proof_a,
        proof_b,
        proof_c,
        public_input,
        market,
        auction,
        active_order,
        owner,
        balance_revision,
        client_order_nonce,
        order_commitment: commitment,
        committee_key,
        balance_ciphertexts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn valid_release_is_satisfied() {
        let witness = CollateralReleaseWitness::deterministic();
        let cs = ConstraintSystem::<Fq>::new_ref();
        CollateralReleaseCircuit::from_witness(witness)
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    #[ignore = "generates a full collateral-release Groth16 proof; run in release mode"]
    fn arbitrary_release_prover_generates_a_verified_transition() {
        let witness = CollateralReleaseWitness::deterministic();
        let request = CollateralReleaseProofRequest {
            market: context_pubkey(witness.market_context),
            auction: context_pubkey(witness.auction_context),
            active_order: context_pubkey(witness.active_order_context),
            owner: context_pubkey(witness.owner_context),
            balance_revision: witness.balance_revision,
            client_order_nonce: witness.client_order_nonce,
            buy: witness.buy,
            sell: witness.sell,
            base_reserve: witness.base_reserve,
            quote_reserve: witness.quote_reserve,
            order_salt: field_bytes_le(witness.order_salt),
            expected_order_commitment: field_bytes_le(witness.order_commitment()),
            committee_key: wire_point_bytes(witness.committee_key.into_affine()),
            old_balance: PrivateBalanceOpening {
                base_value: witness.old_base_balance,
                quote_value: witness.old_quote_balance,
                base_randomness: scalar_bytes(witness.balance_randomness[0]),
                quote_randomness: scalar_bytes(witness.balance_randomness[1]),
            },
        };
        let prover = CollateralReleaseProver::deterministic_devnet().unwrap();
        let mut rng = ChaCha20Rng::from_seed([0xee; 32]);
        let output = prover.prove(request, &mut rng).unwrap();
        assert_eq!(
            output.released_balance.base_value,
            witness.old_base_balance + witness.base_reserve
        );
        assert_eq!(
            output.released_balance.quote_value,
            witness.old_quote_balance + witness.quote_reserve
        );
        assert_ne!(output.public_input, [0; 32]);
    }

    #[test]
    fn wrong_reserve_cannot_release_extra_balance() {
        let mut witness = CollateralReleaseWitness::deterministic();
        witness.base_reserve += 1;
        let original_digest = CollateralReleaseWitness::deterministic().digest();
        let cs = ConstraintSystem::<Fq>::new_ref();
        CollateralReleaseCircuit {
            witness: Some(witness),
            digest: Some(original_digest),
        }
        .generate_constraints(cs.clone())
        .unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    #[ignore = "generates a circuit-specific setup and measures release proving"]
    fn benchmark_recurring_release_proof() {
        let witness = CollateralReleaseWitness::deterministic();
        let cs = ConstraintSystem::<Fq>::new_ref();
        CollateralReleaseCircuit::from_witness(witness.clone())
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(cs.is_satisfied().unwrap());
        let constraints = cs.num_constraints();

        let mut setup_rng = ChaCha20Rng::from_seed([0xea; 32]);
        let setup_started = Instant::now();
        let proving_key = setup(&mut setup_rng).unwrap();
        let setup_elapsed = setup_started.elapsed();
        let mut proof_rng = ChaCha20Rng::from_seed([0xeb; 32]);
        let proof_started = Instant::now();
        let _proof = prove(&proving_key, witness, &mut proof_rng).unwrap();
        let proof_elapsed = proof_started.elapsed();
        eprintln!(
            "release constraints={constraints}, setup_ms={}, recurring_proof_ms={}",
            setup_elapsed.as_millis(),
            proof_elapsed.as_millis()
        );
    }
}
