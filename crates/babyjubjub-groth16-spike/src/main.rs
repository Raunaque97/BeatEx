use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use ark_bn254::Bn254;
use ark_ec::{AdditiveGroup, CurveGroup, PrimeGroup};
use ark_ed_on_bn254::{
    constraints::EdwardsVar, EdwardsAffine, EdwardsProjective, Fq, Fr as JubjubScalar,
};
use ark_ff::{BigInteger, PrimeField, UniformRand, Zero};
use ark_groth16::Groth16;
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
    groups::CurveVar,
};
use ark_relations::r1cs::{
    ConstraintSynthesizer, ConstraintSystem, ConstraintSystemRef, SynthesisError,
};
use ark_serialize::CanonicalSerialize;
use ark_snark::SNARK;
use light_poseidon::{parameters::bn254_x5, Poseidon, PoseidonHasher, PoseidonParameters};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

pub mod decryption;
pub mod deposit;
pub mod transition;

const LEVELS: usize = 17;
const QUANTITY_BITS: usize = 4;
const PRICE_BITS: usize = 64;
const QUOTE_COLLATERAL_BITS: usize = PRICE_BITS + QUANTITY_BITS;
const RANDOMNESS_BITS: usize = JubjubScalar::MODULUS_BIT_SIZE as usize;
const ORDER_CIPHERTEXTS: usize = LEVELS * 2;
const CIPHERTEXTS: usize = ORDER_CIPHERTEXTS + 2;
const TRANSCRIPT_WIDTH: u8 = 13;
const TRANSCRIPT_PAYLOAD_PER_FRAME: usize = TRANSCRIPT_WIDTH as usize - 2;

#[derive(Clone, Copy, Debug)]
struct Ciphertext {
    ephemeral: EdwardsAffine,
    body: EdwardsAffine,
}

#[derive(Clone, Debug)]
struct OrderWitness {
    buy: [u64; LEVELS],
    sell: [u64; LEVELS],
    reference_price: u64,
    tick_size: u64,
    base_reserve: u128,
    quote_reserve: u128,
    committee_key: EdwardsProjective,
    auction_context: [Fq; 2],
    owner_context: [Fq; 2],
    client_order_nonce: u64,
    randomness: Vec<JubjubScalar>,
}

impl OrderWitness {
    fn deterministic() -> Self {
        let mut buy = [0u64; LEVELS];
        let mut sell = [0u64; LEVELS];
        for (index, value) in buy.iter_mut().enumerate().take(LEVELS / 2) {
            *value = ((index * 7 + 3) % 15 + 1) as u64;
        }
        for (index, value) in sell.iter_mut().enumerate().skip(LEVELS / 2 + 1) {
            *value = ((index * 11 + 5) % 15 + 1) as u64;
        }

        let reference_price = 100_000u64;
        let tick_size = 500u64;
        let prices = price_grid(reference_price, tick_size).expect("valid deterministic grid");
        let base_reserve = sell.iter().copied().max().unwrap_or_default() as u128;
        let quote_reserve = buy
            .iter()
            .zip(prices)
            .map(|(quantity, price)| u128::from(*quantity) * price)
            .max()
            .unwrap_or_default();

        let mut rng = ChaCha20Rng::from_seed([0x5a; 32]);
        let committee_secret = JubjubScalar::rand(&mut rng);
        let committee_key =
            EdwardsProjective::generator().mul_bigint(committee_secret.into_bigint());
        let randomness = (0..CIPHERTEXTS)
            .map(|_| JubjubScalar::rand(&mut rng))
            .collect();

        Self {
            buy,
            sell,
            reference_price,
            tick_size,
            base_reserve,
            quote_reserve,
            committee_key,
            auction_context: [Fq::from(0x4245_4154u64), Fq::from(0x4155_4354u64)],
            owner_context: [Fq::from(0x4f57_4e45u64), Fq::from(0x5452_4144u64)],
            client_order_nonce: 7,
            randomness,
        }
    }

    fn ciphertexts(&self) -> Vec<Ciphertext> {
        let mut messages = Vec::with_capacity(CIPHERTEXTS);
        for index in 0..LEVELS {
            messages.push(u128::from(self.buy[index]));
            messages.push(u128::from(self.sell[index]));
        }
        messages.push(self.base_reserve);
        messages.push(self.quote_reserve);

        messages
            .into_iter()
            .zip(self.randomness.iter().copied())
            .map(|(message, randomness)| encrypt(message, randomness, self.committee_key))
            .collect()
    }

    fn transcript_digest(&self) -> Fq {
        let ciphertexts = self.ciphertexts();
        let mut payload = Vec::with_capacity(11 + ciphertexts.len() * 4);
        payload.push(domain_separator());
        payload.push(Fq::from(1u64));
        payload.extend_from_slice(&self.auction_context);
        payload.extend_from_slice(&self.owner_context);
        payload.push(Fq::from(self.client_order_nonce));
        let committee_key = self.committee_key.into_affine();
        payload.push(committee_key.x);
        payload.push(committee_key.y);
        payload.push(Fq::from(self.reference_price));
        payload.push(Fq::from(self.tick_size));
        for ciphertext in ciphertexts {
            payload.push(ciphertext.ephemeral.x);
            payload.push(ciphertext.ephemeral.y);
            payload.push(ciphertext.body.x);
            payload.push(ciphertext.body.y);
        }
        transcript_hash_native(&payload)
    }
}

#[derive(Clone, Copy, Debug)]
struct Features {
    encryption: bool,
    transcript: bool,
}

impl Features {
    const ARITHMETIC: Self = Self {
        encryption: false,
        transcript: false,
    };
    const ENCRYPTION: Self = Self {
        encryption: true,
        transcript: false,
    };
    const FULL: Self = Self {
        encryption: true,
        transcript: true,
    };
}

#[derive(Clone, Debug)]
struct OrderCircuit {
    witness: Option<OrderWitness>,
    digest: Option<Fq>,
    features: Features,
}

impl OrderCircuit {
    fn blank(features: Features) -> Self {
        Self {
            witness: None,
            digest: None,
            features,
        }
    }

    fn from_witness(witness: OrderWitness, features: Features) -> Self {
        let digest = features.transcript.then(|| witness.transcript_digest());
        Self {
            witness: Some(witness),
            digest,
            features,
        }
    }
}

impl ConstraintSynthesizer<Fq> for OrderCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fq>) -> Result<(), SynthesisError> {
        let witness = self.witness.as_ref();
        let buy_values = witness.map(|witness| witness.buy);
        let sell_values = witness.map(|witness| witness.sell);
        let reference_value = witness.map(|witness| u128::from(witness.reference_price));
        let tick_value = witness.map(|witness| u128::from(witness.tick_size));
        let base_reserve_value = witness.map(|witness| witness.base_reserve);
        let quote_reserve_value = witness.map(|witness| witness.quote_reserve);

        let (_, reference) = alloc_uint(cs.clone(), reference_value, PRICE_BITS)?;
        let (_, tick) = alloc_uint(cs.clone(), tick_value, PRICE_BITS)?;
        let lower_price_value = witness.and_then(|witness| {
            u128::from(witness.reference_price)
                .checked_sub((LEVELS / 2) as u128 * u128::from(witness.tick_size))
        });
        let upper_price_value = witness.and_then(|witness| {
            u128::from(witness.reference_price)
                .checked_add((LEVELS / 2) as u128 * u128::from(witness.tick_size))
        });
        let (_, lower_price) = alloc_uint(cs.clone(), lower_price_value, PRICE_BITS)?;
        let (_, upper_price) = alloc_uint(cs.clone(), upper_price_value, PRICE_BITS)?;
        lower_price.enforce_equal(&(&reference - &tick * Fq::from((LEVELS / 2) as u64)))?;
        upper_price.enforce_equal(&(&reference + &tick * Fq::from((LEVELS / 2) as u64)))?;

        let (base_reserve_bits, base_reserve) =
            alloc_uint(cs.clone(), base_reserve_value, QUANTITY_BITS)?;
        let (quote_reserve_bits, quote_reserve) =
            alloc_uint(cs.clone(), quote_reserve_value, QUOTE_COLLATERAL_BITS)?;

        let prices_native = witness
            .map(|witness| price_grid(witness.reference_price, witness.tick_size))
            .transpose()
            .map_err(|_| SynthesisError::Unsatisfiable)?;
        let base_max_index = witness.map(|witness| {
            witness
                .sell
                .iter()
                .position(|value| u128::from(*value) == witness.base_reserve)
                .unwrap_or(0)
        });
        let quote_max_index = witness
            .zip(prices_native.as_ref())
            .map(|(witness, prices)| {
                witness
                    .buy
                    .iter()
                    .zip(prices)
                    .position(|(quantity, price)| {
                        u128::from(*quantity) * *price == witness.quote_reserve
                    })
                    .unwrap_or(0)
            });

        let mut buy_bits_all = Vec::with_capacity(LEVELS);
        let mut sell_bits_all = Vec::with_capacity(LEVELS);
        let mut base_selectors = Vec::with_capacity(LEVELS);
        let mut quote_selectors = Vec::with_capacity(LEVELS);

        for index in 0..LEVELS {
            let buy_value = buy_values.map(|values| u128::from(values[index]));
            let sell_value = sell_values.map(|values| u128::from(values[index]));
            let (buy_bits, buy) = alloc_uint(cs.clone(), buy_value, QUANTITY_BITS)?;
            let (sell_bits, sell) = alloc_uint(cs.clone(), sell_value, QUANTITY_BITS)?;
            (&buy * &sell).enforce_equal(&FpVar::zero())?;

            let signed_offset = index as isize - (LEVELS / 2) as isize;
            let price = if signed_offset < 0 {
                &reference - &tick * Fq::from((-signed_offset) as u64)
            } else {
                &reference + &tick * Fq::from(signed_offset as u64)
            };
            let liability = &price * &buy;
            let liability_value = witness
                .zip(prices_native.as_ref())
                .map(|(witness, prices)| u128::from(witness.buy[index]) * prices[index]);

            let base_slack_value = witness.map(|witness| {
                witness
                    .base_reserve
                    .checked_sub(u128::from(witness.sell[index]))
                    .unwrap_or(u128::MAX)
            });
            let quote_slack_value = witness.zip(liability_value).map(|(witness, liability)| {
                witness
                    .quote_reserve
                    .checked_sub(liability)
                    .unwrap_or(u128::MAX)
            });
            let (_, base_slack) = alloc_uint(cs.clone(), base_slack_value, QUANTITY_BITS)?;
            let (_, quote_slack) =
                alloc_uint(cs.clone(), quote_slack_value, QUOTE_COLLATERAL_BITS)?;
            (&sell + &base_slack).enforce_equal(&base_reserve)?;
            (&liability + &quote_slack).enforce_equal(&quote_reserve)?;

            let base_selector = Boolean::new_witness(cs.clone(), || {
                Ok(base_max_index.ok_or(SynthesisError::AssignmentMissing)? == index)
            })?;
            let quote_selector = Boolean::new_witness(cs.clone(), || {
                Ok(quote_max_index.ok_or(SynthesisError::AssignmentMissing)? == index)
            })?;
            let base_selector_fp: FpVar<Fq> = base_selector.clone().into();
            let quote_selector_fp: FpVar<Fq> = quote_selector.clone().into();
            (&base_selector_fp * (&base_reserve - &sell)).enforce_equal(&FpVar::zero())?;
            (&quote_selector_fp * (&quote_reserve - &liability)).enforce_equal(&FpVar::zero())?;

            buy_bits_all.push(buy_bits);
            sell_bits_all.push(sell_bits);
            base_selectors.push(base_selector_fp);
            quote_selectors.push(quote_selector_fp);
        }

        sum_vars(&base_selectors).enforce_equal(&FpVar::one())?;
        sum_vars(&quote_selectors).enforce_equal(&FpVar::one())?;

        if !self.features.encryption {
            return Ok(());
        }

        let committee_key = EdwardsVar::new_witness(cs.clone(), || {
            witness
                .map(|witness| witness.committee_key)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let randomness = witness.map(|witness| witness.randomness.as_slice());
        let mut ciphertexts = Vec::with_capacity(CIPHERTEXTS);
        let mut random_index = 0usize;
        for index in 0..LEVELS {
            ciphertexts.push(encrypt_gadget(
                cs.clone(),
                &buy_bits_all[index],
                scalar_bits_at(randomness, random_index),
                &committee_key,
            )?);
            random_index += 1;
            ciphertexts.push(encrypt_gadget(
                cs.clone(),
                &sell_bits_all[index],
                scalar_bits_at(randomness, random_index),
                &committee_key,
            )?);
            random_index += 1;
        }
        ciphertexts.push(encrypt_gadget(
            cs.clone(),
            &base_reserve_bits,
            scalar_bits_at(randomness, random_index),
            &committee_key,
        )?);
        random_index += 1;
        ciphertexts.push(encrypt_gadget(
            cs.clone(),
            &quote_reserve_bits,
            scalar_bits_at(randomness, random_index),
            &committee_key,
        )?);

        if !self.features.transcript {
            return Ok(());
        }

        let expected_digest = FpVar::new_input(cs.clone(), || {
            self.digest.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let auction_context = (0..2)
            .map(|index| {
                FpVar::new_witness(cs.clone(), || {
                    witness
                        .map(|witness| witness.auction_context[index])
                        .ok_or(SynthesisError::AssignmentMissing)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let owner_context = (0..2)
            .map(|index| {
                FpVar::new_witness(cs.clone(), || {
                    witness
                        .map(|witness| witness.owner_context[index])
                        .ok_or(SynthesisError::AssignmentMissing)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let client_order_nonce = FpVar::new_witness(cs.clone(), || {
            witness
                .map(|witness| Fq::from(witness.client_order_nonce))
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let mut payload = Vec::with_capacity(11 + ciphertexts.len() * 4);
        payload.push(FpVar::constant(domain_separator()));
        payload.push(FpVar::constant(Fq::from(1u64)));
        payload.extend(auction_context);
        payload.extend(owner_context);
        payload.push(client_order_nonce);
        payload.push(committee_key.x.clone());
        payload.push(committee_key.y.clone());
        payload.push(reference);
        payload.push(tick);
        for (ephemeral, body) in ciphertexts {
            payload.push(ephemeral.x);
            payload.push(ephemeral.y);
            payload.push(body.x);
            payload.push(body.y);
        }
        transcript_hash_gadget(&payload)?.enforce_equal(&expected_digest)?;
        Ok(())
    }
}

fn alloc_uint(
    cs: ConstraintSystemRef<Fq>,
    value: Option<u128>,
    bit_count: usize,
) -> Result<(Vec<Boolean<Fq>>, FpVar<Fq>), SynthesisError> {
    let bits = (0..bit_count)
        .map(|index| {
            Boolean::new_witness(cs.clone(), || {
                let value = value.ok_or(SynthesisError::AssignmentMissing)?;
                Ok(((value >> index) & 1) == 1)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let field = Boolean::le_bits_to_fp(&bits)?;
    Ok((bits, field))
}

fn scalar_bits_at(randomness: Option<&[JubjubScalar]>, index: usize) -> Option<Vec<bool>> {
    randomness.map(|values| {
        let bigint = values[index].into_bigint();
        (0..RANDOMNESS_BITS)
            .map(|bit| bigint.get_bit(bit))
            .collect()
    })
}

fn encrypt_gadget(
    cs: ConstraintSystemRef<Fq>,
    message_bits: &[Boolean<Fq>],
    randomness_bits: Option<Vec<bool>>,
    committee_key: &EdwardsVar,
) -> Result<(EdwardsVar, EdwardsVar), SynthesisError> {
    let randomness_bits = (0..RANDOMNESS_BITS)
        .map(|index| {
            Boolean::new_witness(cs.clone(), || {
                randomness_bits
                    .as_ref()
                    .map(|bits| bits[index])
                    .ok_or(SynthesisError::AssignmentMissing)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let generator = EdwardsProjective::generator();
    let ephemeral = fixed_base_mul(&randomness_bits, generator)?;
    let message = fixed_base_mul(message_bits, generator)?;
    let shared = committee_key.scalar_mul_le(randomness_bits.iter())?;
    let body = shared + &message;
    Ok((ephemeral, body))
}

fn fixed_base_mul(
    bits: &[Boolean<Fq>],
    base: EdwardsProjective,
) -> Result<EdwardsVar, SynthesisError> {
    let mut multiples = Vec::with_capacity(bits.len());
    let mut multiple = base;
    for _ in bits {
        multiples.push(multiple);
        multiple.double_in_place();
    }
    let mut result = EdwardsVar::zero();
    result.precomputed_base_scalar_mul_le(bits.iter().zip(multiples.iter()))?;
    Ok(result)
}

fn sum_vars(values: &[FpVar<Fq>]) -> FpVar<Fq> {
    values.iter().fold(FpVar::zero(), |sum, value| sum + value)
}

fn encrypt(message: u128, randomness: JubjubScalar, key: EdwardsProjective) -> Ciphertext {
    let generator = EdwardsProjective::generator();
    let ephemeral = generator.mul_bigint(randomness.into_bigint());
    let message_scalar = JubjubScalar::from_le_bytes_mod_order(&message.to_le_bytes());
    let body = key.mul_bigint(randomness.into_bigint())
        + generator.mul_bigint(message_scalar.into_bigint());
    Ciphertext {
        ephemeral: ephemeral.into_affine(),
        body: body.into_affine(),
    }
}

fn price_grid(reference: u64, tick: u64) -> Result<[u128; LEVELS], &'static str> {
    let center = LEVELS / 2;
    let mut prices = [0u128; LEVELS];
    for (index, price) in prices.iter_mut().enumerate() {
        *price = if index < center {
            u128::from(reference)
                .checked_sub((center - index) as u128 * u128::from(tick))
                .ok_or("price-grid underflow")?
        } else {
            u128::from(reference)
                .checked_add((index - center) as u128 * u128::from(tick))
                .ok_or("price-grid overflow")?
        };
        if *price >= (1u128 << PRICE_BITS) {
            return Err("price exceeds configured bit width");
        }
    }
    Ok(prices)
}

fn domain_separator() -> Fq {
    Fq::from_le_bytes_mod_order(b"BeatEx order ciphertext transcript v1")
}

fn poseidon_parameters() -> PoseidonParameters<Fq> {
    bn254_x5::get_poseidon_parameters::<Fq>(TRANSCRIPT_WIDTH)
        .expect("light-poseidon supports width 13")
}

fn transcript_hash_native(payload: &[Fq]) -> Fq {
    let params = poseidon_parameters();
    let mut digest = Fq::zero();
    for chunk in payload.chunks(TRANSCRIPT_PAYLOAD_PER_FRAME) {
        let mut inputs = vec![Fq::zero(); TRANSCRIPT_WIDTH as usize - 1];
        inputs[0] = digest;
        inputs[1..1 + chunk.len()].copy_from_slice(chunk);
        digest = Poseidon::new(PoseidonParameters::new(
            params.ark.clone(),
            params.mds.clone(),
            params.full_rounds,
            params.partial_rounds,
            params.width,
            params.alpha,
        ))
        .hash(&inputs)
        .expect("fixed-width transcript frame");
    }
    digest
}

fn transcript_hash_gadget(payload: &[FpVar<Fq>]) -> Result<FpVar<Fq>, SynthesisError> {
    let params = poseidon_parameters();
    let mut digest = FpVar::zero();
    for chunk in payload.chunks(TRANSCRIPT_PAYLOAD_PER_FRAME) {
        let mut inputs = vec![FpVar::zero(); TRANSCRIPT_WIDTH as usize - 1];
        inputs[0] = digest;
        inputs[1..1 + chunk.len()].clone_from_slice(chunk);
        digest = poseidon_compress_gadget(&inputs, &params)?;
    }
    Ok(digest)
}

fn poseidon_compress_gadget(
    inputs: &[FpVar<Fq>],
    params: &PoseidonParameters<Fq>,
) -> Result<FpVar<Fq>, SynthesisError> {
    debug_assert_eq!(inputs.len(), params.width - 1);
    let mut state = Vec::with_capacity(params.width);
    state.push(FpVar::zero());
    state.extend_from_slice(inputs);
    let full_half = params.full_rounds / 2;
    let rounds = params.full_rounds + params.partial_rounds;
    for round in 0..rounds {
        for (index, element) in state.iter_mut().enumerate() {
            *element += params.ark[round * params.width + index];
        }
        let full = round < full_half || round >= full_half + params.partial_rounds;
        if full {
            for element in &mut state {
                *element = fifth_power(element)?;
            }
        } else {
            state[0] = fifth_power(&state[0])?;
        }
        let previous = state;
        state = (0..params.width)
            .map(|row| {
                previous
                    .iter()
                    .enumerate()
                    .fold(FpVar::zero(), |sum, (column, value)| {
                        sum + value * params.mds[row][column]
                    })
            })
            .collect();
    }
    Ok(state[0].clone())
}

fn fifth_power(value: &FpVar<Fq>) -> Result<FpVar<Fq>, SynthesisError> {
    let square = value.square()?;
    let fourth = square.square()?;
    Ok(fourth * value)
}

#[derive(Clone, Copy, Debug)]
struct Shape {
    constraints: usize,
    instance_variables: usize,
    witness_variables: usize,
}

fn circuit_shape(features: Features) -> Result<Shape, SynthesisError> {
    let cs = ConstraintSystem::<Fq>::new_ref();
    cs.set_mode(ark_relations::r1cs::SynthesisMode::Setup);
    OrderCircuit::blank(features).generate_constraints(cs.clone())?;
    cs.finalize();
    Ok(Shape {
        constraints: cs.num_constraints(),
        instance_variables: cs.num_instance_variables(),
        witness_variables: cs.num_witness_variables(),
    })
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arithmetic = circuit_shape(Features::ARITHMETIC)?;
    let encryption = circuit_shape(Features::ENCRYPTION)?;
    let full = circuit_shape(Features::FULL)?;
    println!("BeatEx Baby Jubjub / BN254 Groth16 host spike");
    println!(
        "levels={LEVELS}, ciphertexts={CIPHERTEXTS}, public_inputs={}",
        full.instance_variables - 1
    );
    println!(
        "constraints: arithmetic={} encryption_delta={} transcript_delta={} total={}",
        arithmetic.constraints,
        encryption.constraints - arithmetic.constraints,
        full.constraints - encryption.constraints,
        full.constraints
    );
    println!(
        "variables: instance={} witness={}",
        full.instance_variables, full.witness_variables
    );

    let witness = OrderWitness::deterministic();
    let digest = witness.transcript_digest();
    let mut encrypt_times = Vec::new();
    let mut encrypt_and_hash_times = Vec::new();
    for _ in 0..21 {
        let encrypt_started = Instant::now();
        black_box(witness.ciphertexts());
        encrypt_times.push(encrypt_started.elapsed());
        let hash_started = Instant::now();
        black_box(witness.transcript_digest());
        encrypt_and_hash_times.push(hash_started.elapsed());
    }
    let setup_started = Instant::now();
    let mut setup_rng = ChaCha20Rng::from_seed([0x42; 32]);
    let (proving_key, verifying_key) = Groth16::<Bn254>::circuit_specific_setup(
        OrderCircuit::blank(Features::FULL),
        &mut setup_rng,
    )?;
    let setup_time = setup_started.elapsed();

    let mut proving_key_bytes = Vec::new();
    proving_key.serialize_compressed(&mut proving_key_bytes)?;
    let prepared_key = Groth16::<Bn254>::process_vk(&verifying_key)?;
    let mut prove_times = Vec::new();
    let mut verify_times = Vec::new();
    let mut proof_bytes = 0usize;
    for round in 0..3u8 {
        let mut proof_rng = ChaCha20Rng::from_seed([0x70 + round; 32]);
        let circuit = OrderCircuit::from_witness(witness.clone(), Features::FULL);
        let prove_started = Instant::now();
        let proof = Groth16::<Bn254>::prove(&proving_key, circuit, &mut proof_rng)?;
        prove_times.push(prove_started.elapsed());
        let mut bytes = Vec::new();
        proof.serialize_compressed(&mut bytes)?;
        proof_bytes = bytes.len();
        let verify_started = Instant::now();
        let verified =
            Groth16::<Bn254>::verify_with_processed_vk(&prepared_key, &[digest], &proof)?;
        verify_times.push(verify_started.elapsed());
        assert!(verified);
    }
    let ciphertext_bytes =
        witness
            .ciphertexts()
            .iter()
            .try_fold(0usize, |length, ciphertext| {
                let mut bytes = Vec::new();
                ciphertext.ephemeral.serialize_compressed(&mut bytes)?;
                ciphertext.body.serialize_compressed(&mut bytes)?;
                Ok::<_, ark_serialize::SerializationError>(length + bytes.len())
            })?;
    println!(
        "encrypt_median_ms={:.3}, encrypt_and_hash_median_ms={:.3}",
        median(encrypt_times).as_secs_f64() * 1_000.0,
        median(encrypt_and_hash_times).as_secs_f64() * 1_000.0,
    );
    println!(
        "setup_ms={:.3}, prove_median_ms={:.3}, verify_median_ms={:.3}",
        setup_time.as_secs_f64() * 1_000.0,
        median(prove_times).as_secs_f64() * 1_000.0,
        median(verify_times).as_secs_f64() * 1_000.0,
    );
    println!(
        "proving_key_compressed_bytes={}, proof_compressed_bytes={}, ciphertext_bytes={}, raw_order_payload_bytes={}",
        proving_key_bytes.len(),
        proof_bytes,
        ciphertext_bytes,
        ciphertext_bytes + 256 + 32
    );
    transition::run()?;
    decryption::run()?;
    deposit::run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn satisfied(witness: OrderWitness) -> bool {
        let digest = witness.transcript_digest();
        let circuit = OrderCircuit {
            witness: Some(witness),
            digest: Some(digest),
            features: Features::FULL,
        };
        let cs = ConstraintSystem::<Fq>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        cs.is_satisfied().unwrap()
    }

    #[test]
    fn valid_order_is_satisfied() {
        assert!(satisfied(OrderWitness::deterministic()));
    }

    #[test]
    fn rejects_two_sides_at_one_level() {
        let mut witness = OrderWitness::deterministic();
        witness.sell[0] = 1;
        assert!(!satisfied(witness));
    }

    #[test]
    fn rejects_insufficient_quote_collateral() {
        let mut witness = OrderWitness::deterministic();
        witness.quote_reserve -= 1;
        assert!(!satisfied(witness));
    }

    #[test]
    fn transcript_digest_binds_the_ciphertexts() {
        let witness = OrderWitness::deterministic();
        let digest = witness.transcript_digest();
        let mut changed = witness.clone();
        changed.randomness[0] += JubjubScalar::from(1u64);
        let circuit = OrderCircuit {
            witness: Some(changed),
            digest: Some(digest),
            features: Features::FULL,
        };
        let cs = ConstraintSystem::<Fq>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }
}
