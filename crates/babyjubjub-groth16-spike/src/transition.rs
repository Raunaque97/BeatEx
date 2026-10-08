//! Independent queued-order submission circuit.
//!
//! A trader proves one encrypted contingent order and the associated private
//! balance deductions. The instruction creates an independent order account;
//! it does not read or update the encrypted auction aggregate.

use super::*;
use ark_bn254::{G1Affine, G2Affine};
use ark_ec::AffineRepr;
use ark_r1cs_std::prelude::{CondSelectGadget, ToBitsGadget};
use ark_serialize::Compress;
use std::ops::Neg;

const TRANSITION_DOMAIN: &[u8] = b"BeatEx queued order submission v1";
const ORDER_COMMITMENT_DOMAIN: &[u8] = b"BeatEx private contingent order v1";
const BALANCE_BITS: usize = 64;
const FOLD_BATCH_SIZE: usize = 4;
const ORDER_EVENT_DOMAIN: &[u8] = b"BeatEx queue event v2";
const FOLD_DOMAIN: &[u8] = b"BeatEx replace fold v2";

#[derive(Clone, Debug)]
struct TransitionWitness {
    order: OrderWitness,
    order_salt: Fq,
    old_base_balance: u64,
    old_quote_balance: u64,
    balance_randomness: [JubjubScalar; 4],
}

impl TransitionWitness {
    fn deterministic() -> Self {
        let order = OrderWitness::deterministic();
        // Keep this private. It prevents dictionary attacks against the small
        // four-bit order space when the commitment is stored in the order PDA.
        let order_salt = Fq::from_le_bytes_mod_order(&[0x91; 32]);
        let old_base_balance = 100u64;
        let old_quote_balance = 10_000_000u64;
        let balance_randomness = [
            JubjubScalar::from(0xb001u64),
            JubjubScalar::from(0xb002u64),
            JubjubScalar::from(0xb003u64),
            JubjubScalar::from(0xb004u64),
        ];
        Self {
            order,
            order_salt,
            old_base_balance,
            old_quote_balance,
            balance_randomness,
        }
    }

    fn order_commitment(&self) -> Fq {
        let mut payload = Vec::with_capacity(8 + ORDER_CIPHERTEXTS);
        payload.push(Fq::from_le_bytes_mod_order(ORDER_COMMITMENT_DOMAIN));
        payload.extend_from_slice(&self.order.auction_context);
        payload.extend_from_slice(&self.order.owner_context);
        payload.push(Fq::from(self.order.client_order_nonce));
        for index in 0..LEVELS {
            payload.push(Fq::from(self.order.buy[index]));
            payload.push(Fq::from(self.order.sell[index]));
        }
        payload.push(Fq::from(self.order.base_reserve));
        payload.push(Fq::from(self.order.quote_reserve));
        payload.push(self.order_salt);
        transcript_hash_native(&payload)
    }

    fn balance_ciphertexts(&self) -> [Ciphertext; 4] {
        let new_base_balance = self
            .old_base_balance
            .checked_sub(self.order.base_reserve as u64)
            .expect("base balance covers reserve");
        let new_quote_balance = self
            .old_quote_balance
            .checked_sub(self.order.quote_reserve as u64)
            .expect("quote balance covers reserve");
        [
            encrypt(
                self.old_base_balance.into(),
                self.balance_randomness[0],
                self.order.committee_key,
            ),
            encrypt(
                self.old_quote_balance.into(),
                self.balance_randomness[1],
                self.order.committee_key,
            ),
            encrypt(
                new_base_balance.into(),
                self.balance_randomness[2],
                self.order.committee_key,
            ),
            encrypt(
                new_quote_balance.into(),
                self.balance_randomness[3],
                self.order.committee_key,
            ),
        ]
    }

    fn digest(&self) -> Fq {
        let mut payload = Vec::with_capacity(16 + ORDER_CIPHERTEXTS * 4);
        payload.push(Fq::from_le_bytes_mod_order(TRANSITION_DOMAIN));
        payload.push(Fq::from(1u64));
        payload.extend_from_slice(&self.order.auction_context);
        payload.extend_from_slice(&self.order.owner_context);
        payload.push(Fq::from(self.order.client_order_nonce));
        payload.push(self.order_commitment());
        append_wire_point_native(&mut payload, self.order.committee_key.into_affine());
        payload.push(Fq::from(self.order.reference_price));
        payload.push(Fq::from(self.order.tick_size));
        for ciphertext in self.order.ciphertexts().into_iter().take(ORDER_CIPHERTEXTS) {
            append_wire_point_native(&mut payload, ciphertext.ephemeral);
            append_wire_point_native(&mut payload, ciphertext.body);
        }
        for ciphertext in self.balance_ciphertexts() {
            append_wire_point_native(&mut payload, ciphertext.ephemeral);
            append_wire_point_native(&mut payload, ciphertext.body);
        }
        transcript_hash_native(&payload)
    }
}

#[derive(Clone)]
struct TransitionCircuit {
    witness: Option<TransitionWitness>,
    digest: Option<Fq>,
}

impl TransitionCircuit {
    fn blank() -> Self {
        Self {
            witness: None,
            digest: None,
        }
    }

    fn from_witness(witness: TransitionWitness) -> Self {
        let digest = Some(witness.digest());
        Self {
            witness: Some(witness),
            digest,
        }
    }
}

impl ConstraintSynthesizer<Fq> for TransitionCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fq>) -> Result<(), SynthesisError> {
        let witness = self.witness.as_ref();
        let order = witness.map(|witness| &witness.order);
        let buy_values = order.map(|order| order.buy);
        let sell_values = order.map(|order| order.sell);
        let reference_value = order.map(|order| u128::from(order.reference_price));
        let tick_value = order.map(|order| u128::from(order.tick_size));
        let base_reserve_value = order.map(|order| order.base_reserve);
        let quote_reserve_value = order.map(|order| order.quote_reserve);

        let (_, reference) = alloc_uint(cs.clone(), reference_value, PRICE_BITS)?;
        let (_, tick) = alloc_uint(cs.clone(), tick_value, PRICE_BITS)?;
        let lower_price_value = order.and_then(|order| {
            u128::from(order.reference_price)
                .checked_sub((LEVELS / 2) as u128 * u128::from(order.tick_size))
        });
        let upper_price_value = order.and_then(|order| {
            u128::from(order.reference_price)
                .checked_add((LEVELS / 2) as u128 * u128::from(order.tick_size))
        });
        let (_, lower_price) = alloc_uint(cs.clone(), lower_price_value, PRICE_BITS)?;
        let (_, upper_price) = alloc_uint(cs.clone(), upper_price_value, PRICE_BITS)?;
        lower_price.enforce_equal(&(&reference - &tick * Fq::from((LEVELS / 2) as u64)))?;
        upper_price.enforce_equal(&(&reference + &tick * Fq::from((LEVELS / 2) as u64)))?;

        let (_, base_reserve) = alloc_uint(cs.clone(), base_reserve_value, QUANTITY_BITS)?;
        let (_, quote_reserve) = alloc_uint(cs.clone(), quote_reserve_value, BALANCE_BITS)?;
        let old_base_balance_value = witness.map(|witness| u128::from(witness.old_base_balance));
        let old_quote_balance_value = witness.map(|witness| u128::from(witness.old_quote_balance));
        let new_base_balance_value = witness.and_then(|witness| {
            witness
                .old_base_balance
                .checked_sub(witness.order.base_reserve as u64)
                .map(u128::from)
        });
        let new_quote_balance_value = witness.and_then(|witness| {
            witness
                .old_quote_balance
                .checked_sub(witness.order.quote_reserve as u64)
                .map(u128::from)
        });
        let (old_base_balance_bits, old_base_balance) =
            alloc_uint(cs.clone(), old_base_balance_value, BALANCE_BITS)?;
        let (old_quote_balance_bits, old_quote_balance) =
            alloc_uint(cs.clone(), old_quote_balance_value, BALANCE_BITS)?;
        let (new_base_balance_bits, new_base_balance) =
            alloc_uint(cs.clone(), new_base_balance_value, BALANCE_BITS)?;
        let (new_quote_balance_bits, new_quote_balance) =
            alloc_uint(cs.clone(), new_quote_balance_value, BALANCE_BITS)?;
        (&new_base_balance + &base_reserve).enforce_equal(&old_base_balance)?;
        (&new_quote_balance + &quote_reserve).enforce_equal(&old_quote_balance)?;
        let prices_native = order
            .map(|order| price_grid(order.reference_price, order.tick_size))
            .transpose()
            .map_err(|_| SynthesisError::Unsatisfiable)?;
        let base_max_index = order.map(|order| {
            order
                .sell
                .iter()
                .position(|value| u128::from(*value) == order.base_reserve)
                .unwrap_or(0)
        });
        let quote_max_index = order.zip(prices_native.as_ref()).map(|(order, prices)| {
            order
                .buy
                .iter()
                .zip(prices)
                .position(|(quantity, price)| u128::from(*quantity) * *price == order.quote_reserve)
                .unwrap_or(0)
        });

        let mut buy_bits_all = Vec::with_capacity(LEVELS);
        let mut sell_bits_all = Vec::with_capacity(LEVELS);
        let mut buy_vars = Vec::with_capacity(LEVELS);
        let mut sell_vars = Vec::with_capacity(LEVELS);
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
            let liability_value = order
                .zip(prices_native.as_ref())
                .map(|(order, prices)| u128::from(order.buy[index]) * prices[index]);
            let base_slack_value = order.map(|order| {
                order
                    .base_reserve
                    .checked_sub(u128::from(order.sell[index]))
                    .unwrap_or(u128::MAX)
            });
            let quote_slack_value = order.zip(liability_value).map(|(order, liability)| {
                order
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
            let base_selector_fp: FpVar<Fq> = base_selector.into();
            let quote_selector_fp: FpVar<Fq> = quote_selector.into();
            (&base_selector_fp * (&base_reserve - &sell)).enforce_equal(&FpVar::zero())?;
            (&quote_selector_fp * (&quote_reserve - &liability)).enforce_equal(&FpVar::zero())?;

            buy_bits_all.push(buy_bits);
            sell_bits_all.push(sell_bits);
            buy_vars.push(buy);
            sell_vars.push(sell);
            base_selectors.push(base_selector_fp);
            quote_selectors.push(quote_selector_fp);
        }
        sum_vars(&base_selectors).enforce_equal(&FpVar::one())?;
        sum_vars(&quote_selectors).enforce_equal(&FpVar::one())?;

        let committee_key = EdwardsVar::new_witness(cs.clone(), || {
            order
                .map(|order| order.committee_key)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let randomness = order.map(|order| order.randomness.as_slice());
        let mut order_ciphertexts = Vec::with_capacity(ORDER_CIPHERTEXTS);
        let mut random_index = 0usize;
        for index in 0..LEVELS {
            order_ciphertexts.push(encrypt_gadget(
                cs.clone(),
                &buy_bits_all[index],
                scalar_bits_at(randomness, random_index),
                &committee_key,
            )?);
            random_index += 1;
            order_ciphertexts.push(encrypt_gadget(
                cs.clone(),
                &sell_bits_all[index],
                scalar_bits_at(randomness, random_index),
                &committee_key,
            )?);
            random_index += 1;
        }

        let balance_randomness = witness.map(|witness| &witness.balance_randomness);
        let balance_messages = [
            old_base_balance_bits,
            old_quote_balance_bits,
            new_base_balance_bits,
            new_quote_balance_bits,
        ];
        let mut balance_ciphertexts = Vec::with_capacity(4);
        for (index, message_bits) in balance_messages.iter().enumerate() {
            let randomness_bits = balance_randomness.map(|values| {
                let bigint = values[index].into_bigint();
                (0..RANDOMNESS_BITS)
                    .map(|bit| bigint.get_bit(bit))
                    .collect::<Vec<_>>()
            });
            balance_ciphertexts.push(encrypt_gadget(
                cs.clone(),
                message_bits,
                randomness_bits,
                &committee_key,
            )?);
        }

        let auction_context = alloc_context(cs.clone(), order.map(|order| order.auction_context))?;
        let owner_context = alloc_context(cs.clone(), order.map(|order| order.owner_context))?;
        let client_order_nonce = FpVar::new_witness(cs.clone(), || {
            order
                .map(|order| Fq::from(order.client_order_nonce))
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let order_salt = FpVar::new_witness(cs.clone(), || {
            witness
                .map(|witness| witness.order_salt)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let mut order_payload = Vec::with_capacity(8 + ORDER_CIPHERTEXTS);
        order_payload.push(FpVar::constant(Fq::from_le_bytes_mod_order(
            ORDER_COMMITMENT_DOMAIN,
        )));
        order_payload.extend(auction_context.iter().cloned());
        order_payload.extend(owner_context.iter().cloned());
        order_payload.push(client_order_nonce.clone());
        for index in 0..LEVELS {
            order_payload.push(buy_vars[index].clone());
            order_payload.push(sell_vars[index].clone());
        }
        order_payload.push(base_reserve.clone());
        order_payload.push(quote_reserve.clone());
        order_payload.push(order_salt);
        let order_commitment = transcript_hash_gadget(&order_payload)?;

        let mut transition_payload = Vec::with_capacity(16 + ORDER_CIPHERTEXTS * 4);
        transition_payload.push(FpVar::constant(Fq::from_le_bytes_mod_order(
            TRANSITION_DOMAIN,
        )));
        transition_payload.push(FpVar::constant(Fq::from(1u64)));
        transition_payload.extend(auction_context);
        transition_payload.extend(owner_context);
        transition_payload.push(client_order_nonce);
        transition_payload.push(order_commitment);
        append_wire_point_gadget(&mut transition_payload, &committee_key)?;
        transition_payload.push(reference);
        transition_payload.push(tick);

        for (order_ephemeral, order_body) in order_ciphertexts {
            append_wire_point_gadget(&mut transition_payload, &order_ephemeral)?;
            append_wire_point_gadget(&mut transition_payload, &order_body)?;
        }
        for (ephemeral, body) in balance_ciphertexts {
            append_wire_point_gadget(&mut transition_payload, &ephemeral)?;
            append_wire_point_gadget(&mut transition_payload, &body)?;
        }

        let expected_digest =
            FpVar::new_input(cs, || self.digest.ok_or(SynthesisError::AssignmentMissing))?;
        transcript_hash_gadget(&transition_payload)?.enforce_equal(&expected_digest)
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

fn identity_ciphertext() -> Ciphertext {
    Ciphertext {
        ephemeral: EdwardsAffine::zero(),
        body: EdwardsAffine::zero(),
    }
}

fn ciphertext_digest_native(
    auction_context: [Fq; 2],
    order_context: [Fq; 2],
    previous_ciphertexts: &[Ciphertext],
    ciphertexts: &[Ciphertext],
) -> Fq {
    let mut payload = Vec::with_capacity(5 + ciphertexts.len() * 8);
    payload.push(Fq::from_le_bytes_mod_order(ORDER_EVENT_DOMAIN));
    payload.extend_from_slice(&auction_context);
    payload.extend_from_slice(&order_context);
    for order in [previous_ciphertexts, ciphertexts] {
        for ciphertext in order {
            append_wire_point_native(&mut payload, ciphertext.ephemeral);
            append_wire_point_native(&mut payload, ciphertext.body);
        }
    }
    transcript_hash_native(&payload)
}

fn ciphertext_digest_gadget(
    auction_context: &[FpVar<Fq>],
    order_context: &[FpVar<Fq>],
    previous_ciphertexts: &[(EdwardsVar, EdwardsVar)],
    ciphertexts: &[(EdwardsVar, EdwardsVar)],
) -> Result<FpVar<Fq>, SynthesisError> {
    let mut payload = Vec::with_capacity(5 + ciphertexts.len() * 8);
    payload.push(FpVar::constant(Fq::from_le_bytes_mod_order(
        ORDER_EVENT_DOMAIN,
    )));
    payload.extend_from_slice(auction_context);
    payload.extend_from_slice(order_context);
    for order in [previous_ciphertexts, ciphertexts] {
        for (ephemeral, body) in order {
            append_wire_point_gadget(&mut payload, ephemeral)?;
            append_wire_point_gadget(&mut payload, body)?;
        }
    }
    transcript_hash_gadget(&payload)
}

fn alloc_ciphertext_order(
    cs: ConstraintSystemRef<Fq>,
    values: Option<&[Ciphertext]>,
) -> Result<Vec<(EdwardsVar, EdwardsVar)>, SynthesisError> {
    (0..ORDER_CIPHERTEXTS)
        .map(|index| {
            let value = values.map(|values| values[index]);
            let ephemeral = EdwardsVar::new_witness(cs.clone(), || {
                value
                    .map(|value| value.ephemeral.into_group())
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            let body = EdwardsVar::new_witness(cs.clone(), || {
                value
                    .map(|value| value.body.into_group())
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            Ok((ephemeral, body))
        })
        .collect()
}

#[derive(Clone, Debug)]
struct FoldWitness {
    auction_context: [Fq; 2],
    old_folded_count: u64,
    active_count: usize,
    old_aggregate: Vec<Ciphertext>,
    order_contexts: [[Fq; 2]; FOLD_BATCH_SIZE],
    previous_orders: [Vec<Ciphertext>; FOLD_BATCH_SIZE],
    orders: [Vec<Ciphertext>; FOLD_BATCH_SIZE],
}

impl FoldWitness {
    fn deterministic() -> Self {
        let base_order = OrderWitness::deterministic();
        let old_aggregate = (0..ORDER_CIPHERTEXTS)
            .map(|index| {
                encrypt(
                    30 + index as u128,
                    JubjubScalar::from(0xc000u64 + index as u64 * 17),
                    base_order.committee_key,
                )
            })
            .collect();
        let orders = std::array::from_fn(|slot| {
            if slot >= 3 {
                return vec![identity_ciphertext(); ORDER_CIPHERTEXTS];
            }
            let mut order = base_order.clone();
            for (index, randomness) in order.randomness.iter_mut().enumerate() {
                *randomness += JubjubScalar::from((slot * 1_000 + index + 1) as u64);
            }
            order
                .ciphertexts()
                .into_iter()
                .take(ORDER_CIPHERTEXTS)
                .collect()
        });
        let order_contexts = std::array::from_fn(|slot| {
            if slot < 3 {
                [Fq::from(0x1000u64 + slot as u64), Fq::from(slot as u64)]
            } else {
                [Fq::zero(), Fq::zero()]
            }
        });
        let previous_orders = std::array::from_fn(|slot| {
            if slot == 1 {
                orders[0].clone()
            } else {
                vec![identity_ciphertext(); ORDER_CIPHERTEXTS]
            }
        });
        Self {
            auction_context: base_order.auction_context,
            old_folded_count: 9,
            active_count: 3,
            old_aggregate,
            order_contexts,
            previous_orders,
            orders,
        }
    }

    fn new_aggregate(&self) -> Vec<Ciphertext> {
        self.old_aggregate
            .iter()
            .enumerate()
            .map(|(index, old)| {
                self.orders[..self.active_count]
                    .iter()
                    .zip(&self.previous_orders[..self.active_count])
                    .fold(*old, |aggregate, (order, previous)| Ciphertext {
                        ephemeral: (aggregate.ephemeral.into_group() + order[index].ephemeral
                            - previous[index].ephemeral)
                            .into_affine(),
                        body: (aggregate.body.into_group() + order[index].body
                            - previous[index].body)
                            .into_affine(),
                    })
            })
            .collect()
    }

    fn digest(&self) -> Fq {
        let new_aggregate = self.new_aggregate();
        let mut payload = Vec::with_capacity(12 + FOLD_BATCH_SIZE * 3 + ORDER_CIPHERTEXTS * 8);
        payload.push(Fq::from_le_bytes_mod_order(FOLD_DOMAIN));
        payload.push(Fq::from(1u64));
        payload.extend_from_slice(&self.auction_context);
        payload.push(Fq::from(self.old_folded_count));
        payload.push(Fq::from(self.old_folded_count + self.active_count as u64));
        payload.push(Fq::from(self.active_count as u64));
        for slot in 0..FOLD_BATCH_SIZE {
            payload.extend_from_slice(&self.order_contexts[slot]);
            payload.push(ciphertext_digest_native(
                self.auction_context,
                self.order_contexts[slot],
                &self.previous_orders[slot],
                &self.orders[slot],
            ));
        }
        for (old, new) in self.old_aggregate.iter().zip(new_aggregate) {
            append_wire_point_native(&mut payload, old.ephemeral);
            append_wire_point_native(&mut payload, old.body);
            append_wire_point_native(&mut payload, new.ephemeral);
            append_wire_point_native(&mut payload, new.body);
        }
        transcript_hash_native(&payload)
    }
}

#[derive(Clone)]
struct FoldCircuit {
    witness: Option<FoldWitness>,
    digest: Option<Fq>,
}

impl FoldCircuit {
    fn blank() -> Self {
        Self {
            witness: None,
            digest: None,
        }
    }

    fn from_witness(witness: FoldWitness) -> Self {
        let digest = Some(witness.digest());
        Self {
            witness: Some(witness),
            digest,
        }
    }
}

impl ConstraintSynthesizer<Fq> for FoldCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fq>) -> Result<(), SynthesisError> {
        let witness = self.witness.as_ref();
        let auction_context =
            alloc_context(cs.clone(), witness.map(|witness| witness.auction_context))?;
        let old_count_value = witness.map(|witness| u128::from(witness.old_folded_count));
        let new_count_value = witness
            .map(|witness| u128::from(witness.old_folded_count + witness.active_count as u64));
        let (_, old_count) = alloc_uint(cs.clone(), old_count_value, 64)?;
        let (_, new_count) = alloc_uint(cs.clone(), new_count_value, 64)?;
        let active_flags = (0..FOLD_BATCH_SIZE)
            .map(|slot| {
                Boolean::new_witness(cs.clone(), || {
                    witness
                        .map(|witness| slot < witness.active_count)
                        .ok_or(SynthesisError::AssignmentMissing)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // The on-chain fold instruction never accepts an empty batch. Keep
        // the circuit statement equally strict so a proof cannot authorize a
        // no-op accumulator revision.
        active_flags[0].enforce_equal(&Boolean::TRUE)?;
        for slot in 1..FOLD_BATCH_SIZE {
            // Active slots form a strict prefix; this gives one canonical
            // encoding for every batch size.
            (&active_flags[slot] & !&active_flags[slot - 1]).enforce_equal(&Boolean::FALSE)?;
        }
        let active_vars = active_flags
            .iter()
            .cloned()
            .map(FpVar::from)
            .collect::<Vec<_>>();
        let active_count = sum_vars(&active_vars);
        (&old_count + &active_count).enforce_equal(&new_count)?;

        let mut order_context_vars = Vec::with_capacity(FOLD_BATCH_SIZE);
        let mut previous_order_cipher_vars = Vec::with_capacity(FOLD_BATCH_SIZE);
        let mut order_cipher_vars = Vec::with_capacity(FOLD_BATCH_SIZE);
        let mut order_digests = Vec::with_capacity(FOLD_BATCH_SIZE);
        for (slot, active_flag) in active_flags.iter().enumerate() {
            let context = alloc_context(
                cs.clone(),
                witness.map(|witness| witness.order_contexts[slot]),
            )?;
            let previous_ciphertexts = alloc_ciphertext_order(
                cs.clone(),
                witness.map(|witness| witness.previous_orders[slot].as_slice()),
            )?;
            let ciphertexts = alloc_ciphertext_order(
                cs.clone(),
                witness.map(|witness| witness.orders[slot].as_slice()),
            )?;
            let inactive = !active_flag;
            for element in &context {
                element.conditional_enforce_equal(&FpVar::zero(), &inactive)?;
            }
            for order in [&previous_ciphertexts, &ciphertexts] {
                for (ephemeral, body) in order {
                    ephemeral.conditional_enforce_equal(&EdwardsVar::zero(), &inactive)?;
                    body.conditional_enforce_equal(&EdwardsVar::zero(), &inactive)?;
                }
            }
            let digest = ciphertext_digest_gadget(
                &auction_context,
                &context,
                &previous_ciphertexts,
                &ciphertexts,
            )?;
            order_context_vars.push(context);
            previous_order_cipher_vars.push(previous_ciphertexts);
            order_cipher_vars.push(ciphertexts);
            order_digests.push(digest);
        }

        let mut fold_payload = Vec::with_capacity(12 + FOLD_BATCH_SIZE * 3 + ORDER_CIPHERTEXTS * 8);
        fold_payload.push(FpVar::constant(Fq::from_le_bytes_mod_order(FOLD_DOMAIN)));
        fold_payload.push(FpVar::constant(Fq::from(1u64)));
        fold_payload.extend(auction_context);
        fold_payload.push(old_count);
        fold_payload.push(new_count);
        fold_payload.push(active_count);
        for slot in 0..FOLD_BATCH_SIZE {
            fold_payload.extend(order_context_vars[slot].iter().cloned());
            fold_payload.push(order_digests[slot].clone());
        }

        for index in 0..ORDER_CIPHERTEXTS {
            let old_value = witness.map(|witness| witness.old_aggregate[index]);
            let old_ephemeral = EdwardsVar::new_witness(cs.clone(), || {
                old_value
                    .map(|value| value.ephemeral.into_group())
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            let old_body = EdwardsVar::new_witness(cs.clone(), || {
                old_value
                    .map(|value| value.body.into_group())
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            let mut new_ephemeral = old_ephemeral.clone();
            let mut new_body = old_body.clone();
            for slot in 0..FOLD_BATCH_SIZE {
                let ephemeral_candidate = &new_ephemeral + &order_cipher_vars[slot][index].0;
                let body_candidate = &new_body + &order_cipher_vars[slot][index].1;
                let ephemeral_candidate =
                    &ephemeral_candidate - &previous_order_cipher_vars[slot][index].0;
                let body_candidate = &body_candidate - &previous_order_cipher_vars[slot][index].1;
                new_ephemeral = EdwardsVar::conditionally_select(
                    &active_flags[slot],
                    &ephemeral_candidate,
                    &new_ephemeral,
                )?;
                new_body = EdwardsVar::conditionally_select(
                    &active_flags[slot],
                    &body_candidate,
                    &new_body,
                )?;
            }
            append_wire_point_gadget(&mut fold_payload, &old_ephemeral)?;
            append_wire_point_gadget(&mut fold_payload, &old_body)?;
            append_wire_point_gadget(&mut fold_payload, &new_ephemeral)?;
            append_wire_point_gadget(&mut fold_payload, &new_body)?;
        }

        let expected_digest =
            FpVar::new_input(cs, || self.digest.ok_or(SynthesisError::AssignmentMissing))?;
        transcript_hash_gadget(&fold_payload)?.enforce_equal(&expected_digest)
    }
}

#[derive(Clone, Debug)]
pub struct SolanaTransitionFixture {
    pub vk_alpha_g1: [u8; 64],
    pub vk_beta_g2: [u8; 128],
    pub vk_gamma_g2: [u8; 128],
    pub vk_delta_g2: [u8; 128],
    pub vk_ic: [[u8; 64]; 2],
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub auction: [u8; 32],
    pub owner: [u8; 32],
    pub client_order_nonce: u64,
    pub order_commitment: [u8; 32],
    pub committee_key: [u8; 32],
    pub reference_price: u64,
    pub tick_size: u64,
    pub ciphertexts: [[u8; 64]; ORDER_CIPHERTEXTS],
    pub old_base: [u8; 64],
    pub old_quote: [u8; 64],
    pub new_base: [u8; 64],
    pub new_quote: [u8; 64],
}

#[derive(Clone, Debug)]
pub struct SolanaFoldFixture {
    pub vk_alpha_g1: [u8; 64],
    pub vk_beta_g2: [u8; 128],
    pub vk_gamma_g2: [u8; 128],
    pub vk_delta_g2: [u8; 128],
    pub vk_ic: [[u8; 64]; 2],
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub auction: [u8; 32],
    pub old_folded_count: u64,
    pub order_keys: [[u8; 32]; 3],
    pub order_digests: [[u8; 32]; 3],
    pub old_aggregate: [[u8; 64]; ORDER_CIPHERTEXTS],
    pub new_aggregate: [[u8; 64]; ORDER_CIPHERTEXTS],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateBalanceOpening {
    pub base_value: u64,
    pub quote_value: u64,
    pub base_randomness: [u8; 32],
    pub quote_randomness: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct OrderProofRequest {
    pub auction: [u8; 32],
    pub owner: [u8; 32],
    pub client_order_nonce: u64,
    pub reference_price: u64,
    pub tick_size: u64,
    pub committee_key: [u8; 32],
    pub buy: [u64; LEVELS],
    pub sell: [u64; LEVELS],
    pub old_balance: PrivateBalanceOpening,
}

#[derive(Clone, Debug)]
pub struct OrderProofOutput {
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub order_commitment: [u8; 32],
    pub ciphertexts: [[u8; 64]; ORDER_CIPHERTEXTS],
    pub new_encrypted_base: [u8; 64],
    pub new_encrypted_quote: [u8; 64],
    pub new_balance: PrivateBalanceOpening,
    pub order_salt: [u8; 32],
    pub base_reserve: u64,
    pub quote_reserve: u64,
}

#[derive(Clone, Debug)]
pub struct FoldEventInput {
    pub event: [u8; 32],
    /// The ciphertexts referenced by `previous_event`, or encrypted zero for
    /// an initial submission.
    pub previous_ciphertexts: [[u8; 64]; ORDER_CIPHERTEXTS],
    pub ciphertexts: [[u8; 64]; ORDER_CIPHERTEXTS],
}

#[derive(Clone, Debug)]
pub struct FoldProofRequest {
    pub auction: [u8; 32],
    pub old_folded_count: u64,
    pub old_aggregate: [[u8; 64]; ORDER_CIPHERTEXTS],
    /// One to four queue events, in the same public-key order used by the
    /// on-chain instruction's remaining accounts.
    pub events: Vec<FoldEventInput>,
}

#[derive(Clone, Debug)]
pub struct FoldProofOutput {
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub new_aggregate: [[u8; 64]; ORDER_CIPHERTEXTS],
}

#[derive(Clone, Debug)]
pub struct SolanaVerificationKey {
    pub alpha_g1: [u8; 64],
    pub beta_g2: [u8; 128],
    pub gamma_g2: [u8; 128],
    pub delta_g2: [u8; 128],
    pub ic: [[u8; 64]; 2],
}

pub struct OrderProver {
    proving_key: ark_groth16::ProvingKey<Bn254>,
}

pub struct FoldProver {
    proving_key: ark_groth16::ProvingKey<Bn254>,
}

impl FoldProver {
    /// Deterministic setup for Devnet testing only. Production must load the
    /// ceremony-generated fold parameters matching the installed key PDA.
    pub fn deterministic_devnet() -> Result<Self, Box<dyn std::error::Error>> {
        let mut setup_rng = ChaCha20Rng::from_seed([0x73; 32]);
        let (proving_key, _) =
            Groth16::<Bn254>::circuit_specific_setup(FoldCircuit::blank(), &mut setup_rng)?;
        Ok(Self { proving_key })
    }

    pub fn verification_key(&self) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
        verification_key_bytes(&self.proving_key.vk, "fold")
    }

    pub fn prove<R: RngCore + CryptoRng>(
        &self,
        request: FoldProofRequest,
        rng: &mut R,
    ) -> Result<FoldProofOutput, Box<dyn std::error::Error>> {
        if request.events.is_empty() || request.events.len() > FOLD_BATCH_SIZE {
            return Err(std::io::Error::other("fold batch must contain one to four events").into());
        }
        request
            .old_folded_count
            .checked_add(request.events.len() as u64)
            .ok_or_else(|| std::io::Error::other("folded-order count overflow"))?;

        let identity = vec![identity_ciphertext(); ORDER_CIPHERTEXTS];
        let old_aggregate = decode_ciphertext_order(&request.old_aggregate)?;
        let order_contexts = std::array::from_fn(|slot| {
            request
                .events
                .get(slot)
                .map_or([Fq::zero(), Fq::zero()], |event| {
                    pubkey_context(event.event)
                })
        });
        let previous_orders = (0..FOLD_BATCH_SIZE)
            .map(|slot| {
                request.events.get(slot).map_or_else(
                    || Ok(identity.clone()),
                    |event| decode_ciphertext_order(&event.previous_ciphertexts),
                )
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?
            .try_into()
            .map_err(|_| std::io::Error::other("fold requires four witness slots"))?;
        let orders = (0..FOLD_BATCH_SIZE)
            .map(|slot| {
                request.events.get(slot).map_or_else(
                    || Ok(identity.clone()),
                    |event| decode_ciphertext_order(&event.ciphertexts),
                )
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?
            .try_into()
            .map_err(|_| std::io::Error::other("fold requires four witness slots"))?;
        let witness = FoldWitness {
            auction_context: pubkey_context(request.auction),
            old_folded_count: request.old_folded_count,
            active_count: request.events.len(),
            old_aggregate,
            order_contexts,
            previous_orders,
            orders,
        };
        let digest = witness.digest();
        let new_aggregate = witness
            .new_aggregate()
            .into_iter()
            .map(ciphertext_bytes)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| std::io::Error::other("fold aggregate must contain 34 ciphertexts"))?;
        let proof =
            Groth16::<Bn254>::prove(&self.proving_key, FoldCircuit::from_witness(witness), rng)?;
        let prepared_key = Groth16::<Bn254>::process_vk(&self.proving_key.vk)?;
        if !Groth16::<Bn254>::verify_with_processed_vk(&prepared_key, &[digest], &proof)? {
            return Err(std::io::Error::other("generated fold proof did not verify").into());
        }
        Ok(FoldProofOutput {
            proof_a: g1_bytes(proof.a.neg()),
            proof_b: g2_bytes(proof.b),
            proof_c: g1_bytes(proof.c),
            public_input: field_bytes(digest),
            new_aggregate,
        })
    }
}

fn decode_ciphertext_order(
    encoded: &[[u8; 64]; ORDER_CIPHERTEXTS],
) -> Result<Vec<Ciphertext>, Box<dyn std::error::Error>> {
    encoded
        .iter()
        .map(|ciphertext| {
            let ephemeral = decode_wire_point_allow_identity(
                ciphertext[..32]
                    .try_into()
                    .map_err(|_| std::io::Error::other("invalid ciphertext ephemeral"))?,
            )?;
            let body = decode_wire_point_allow_identity(
                ciphertext[32..]
                    .try_into()
                    .map_err(|_| std::io::Error::other("invalid ciphertext body"))?,
            )?;
            Ok(Ciphertext { ephemeral, body })
        })
        .collect()
}

impl OrderProver {
    /// Deterministic setup for Devnet testing only. Production must replace
    /// this with ceremony-generated parameters loaded from an artifact.
    pub fn deterministic_devnet() -> Result<Self, Box<dyn std::error::Error>> {
        let mut setup_rng = ChaCha20Rng::from_seed([0x32; 32]);
        let (proving_key, _) =
            Groth16::<Bn254>::circuit_specific_setup(TransitionCircuit::blank(), &mut setup_rng)?;
        Ok(Self { proving_key })
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
            .map_err(|_| std::io::Error::other("order key must have two IC points"))?;
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
        request: OrderProofRequest,
        rng: &mut R,
    ) -> Result<OrderProofOutput, Box<dyn std::error::Error>> {
        if request.tick_size == 0 {
            return Err(std::io::Error::other("tick size must be positive").into());
        }
        if request
            .buy
            .iter()
            .chain(&request.sell)
            .any(|quantity| *quantity > 15)
            || request
                .buy
                .iter()
                .zip(request.sell)
                .any(|(buy, sell)| *buy != 0 && sell != 0)
        {
            return Err(std::io::Error::other("invalid private order quantities").into());
        }
        let prices = price_grid(request.reference_price, request.tick_size)
            .map_err(std::io::Error::other)?;
        let base_reserve = request.sell.iter().copied().max().unwrap_or_default();
        let quote_reserve = request
            .buy
            .iter()
            .zip(prices)
            .map(|(quantity, price)| u128::from(*quantity) * price)
            .max()
            .unwrap_or_default();
        let quote_reserve = u64::try_from(quote_reserve)
            .map_err(|_| std::io::Error::other("quote reserve does not fit u64"))?;
        let new_base_value = request
            .old_balance
            .base_value
            .checked_sub(base_reserve)
            .ok_or_else(|| std::io::Error::other("insufficient private base balance"))?;
        let new_quote_value = request
            .old_balance
            .quote_value
            .checked_sub(quote_reserve)
            .ok_or_else(|| std::io::Error::other("insufficient private quote balance"))?;
        let committee_key = decode_wire_point(request.committee_key)?;
        let order_salt = Fq::rand(rng);
        let order = OrderWitness {
            buy: request.buy,
            sell: request.sell,
            reference_price: request.reference_price,
            tick_size: request.tick_size,
            base_reserve: u128::from(base_reserve),
            quote_reserve: u128::from(quote_reserve),
            committee_key: committee_key.into_group(),
            auction_context: pubkey_context(request.auction),
            owner_context: pubkey_context(request.owner),
            client_order_nonce: request.client_order_nonce,
            randomness: (0..CIPHERTEXTS).map(|_| JubjubScalar::rand(rng)).collect(),
        };
        let new_base_randomness = JubjubScalar::rand(rng);
        let new_quote_randomness = JubjubScalar::rand(rng);
        let witness = TransitionWitness {
            order,
            order_salt,
            old_base_balance: request.old_balance.base_value,
            old_quote_balance: request.old_balance.quote_value,
            balance_randomness: [
                scalar_from_bytes(request.old_balance.base_randomness)?,
                scalar_from_bytes(request.old_balance.quote_randomness)?,
                new_base_randomness,
                new_quote_randomness,
            ],
        };
        let digest = witness.digest();
        let order_commitment = field_bytes_le(witness.order_commitment());
        let ciphertexts = witness
            .order
            .ciphertexts()
            .into_iter()
            .take(ORDER_CIPHERTEXTS)
            .map(ciphertext_bytes)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| std::io::Error::other("order must contain 34 ciphertexts"))?;
        let balance_ciphertexts = witness.balance_ciphertexts().map(ciphertext_bytes);
        let proof = Groth16::<Bn254>::prove(
            &self.proving_key,
            TransitionCircuit::from_witness(witness),
            rng,
        )?;
        let prepared_key = Groth16::<Bn254>::process_vk(&self.proving_key.vk)?;
        if !Groth16::<Bn254>::verify_with_processed_vk(&prepared_key, &[digest], &proof)? {
            return Err(std::io::Error::other("generated order proof did not verify").into());
        }
        Ok(OrderProofOutput {
            proof_a: g1_bytes(proof.a.neg()),
            proof_b: g2_bytes(proof.b),
            proof_c: g1_bytes(proof.c),
            public_input: field_bytes(digest),
            order_commitment,
            ciphertexts,
            new_encrypted_base: balance_ciphertexts[2],
            new_encrypted_quote: balance_ciphertexts[3],
            new_balance: PrivateBalanceOpening {
                base_value: new_base_value,
                quote_value: new_quote_value,
                base_randomness: scalar_bytes(new_base_randomness),
                quote_randomness: scalar_bytes(new_quote_randomness),
            },
            order_salt: field_bytes_le(order_salt),
            base_reserve,
            quote_reserve,
        })
    }
}

/// Deterministic development fixture generated from the exact transition
/// circuit. The setup seed is explicitly non-production.
pub fn deterministic_solana_fixture() -> Result<SolanaTransitionFixture, Box<dyn std::error::Error>>
{
    transition_solana_fixture(TransitionWitness::deterministic())
}

/// Generate the deterministic non-production order fixture while binding it
/// to caller-selected Solana account addresses. This is used by instruction-
/// level SBF tests, where the owner must be a real transaction signer.
pub fn deterministic_solana_fixture_for_context(
    auction: [u8; 32],
    owner: [u8; 32],
) -> Result<SolanaTransitionFixture, Box<dyn std::error::Error>> {
    let mut witness = TransitionWitness::deterministic();
    witness.order.auction_context = pubkey_context(auction);
    witness.order.owner_context = pubkey_context(owner);
    transition_solana_fixture(witness)
}

fn transition_solana_fixture(
    witness: TransitionWitness,
) -> Result<SolanaTransitionFixture, Box<dyn std::error::Error>> {
    let digest = witness.digest();
    let order_ciphertexts: [[u8; 64]; ORDER_CIPHERTEXTS] = witness
        .order
        .ciphertexts()
        .into_iter()
        .take(ORDER_CIPHERTEXTS)
        .map(ciphertext_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "queued order must contain 34 ciphertexts")?;
    let balance_ciphertexts = witness.balance_ciphertexts().map(ciphertext_bytes);
    let mut setup_rng = ChaCha20Rng::from_seed([0x32; 32]);
    let (proving_key, verifying_key) =
        Groth16::<Bn254>::circuit_specific_setup(TransitionCircuit::blank(), &mut setup_rng)?;
    let mut proof_rng = ChaCha20Rng::from_seed([0xa0; 32]);
    let proof = Groth16::<Bn254>::prove(
        &proving_key,
        TransitionCircuit::from_witness(witness.clone()),
        &mut proof_rng,
    )?;
    let ic = verifying_key
        .gamma_abc_g1
        .iter()
        .copied()
        .map(g1_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "transition verifying key must have two IC points")?;
    Ok(SolanaTransitionFixture {
        vk_alpha_g1: g1_bytes(verifying_key.alpha_g1),
        vk_beta_g2: g2_bytes(verifying_key.beta_g2),
        vk_gamma_g2: g2_bytes(verifying_key.gamma_g2),
        vk_delta_g2: g2_bytes(verifying_key.delta_g2),
        vk_ic: ic,
        proof_a: g1_bytes(proof.a.neg()),
        proof_b: g2_bytes(proof.b),
        proof_c: g1_bytes(proof.c),
        public_input: field_bytes(digest),
        auction: context_pubkey(witness.order.auction_context),
        owner: context_pubkey(witness.order.owner_context),
        client_order_nonce: witness.order.client_order_nonce,
        order_commitment: field_bytes_le(witness.order_commitment()),
        committee_key: wire_point_bytes(witness.order.committee_key.into_affine()),
        reference_price: witness.order.reference_price,
        tick_size: witness.order.tick_size,
        ciphertexts: order_ciphertexts,
        old_base: balance_ciphertexts[0],
        old_quote: balance_ciphertexts[1],
        new_base: balance_ciphertexts[2],
        new_quote: balance_ciphertexts[3],
    })
}

pub fn deterministic_fold_solana_fixture() -> Result<SolanaFoldFixture, Box<dyn std::error::Error>>
{
    let witness = FoldWitness::deterministic();
    let digest = witness.digest();
    let old_aggregate = witness
        .old_aggregate
        .iter()
        .copied()
        .map(ciphertext_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "fold aggregate must contain 34 ciphertexts")?;
    let new_aggregate = witness
        .new_aggregate()
        .into_iter()
        .map(ciphertext_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "fold aggregate must contain 34 ciphertexts")?;
    let order_keys = std::array::from_fn(|slot| context_pubkey(witness.order_contexts[slot]));
    let order_digests = std::array::from_fn(|slot| {
        field_bytes_le(ciphertext_digest_native(
            witness.auction_context,
            witness.order_contexts[slot],
            &witness.previous_orders[slot],
            &witness.orders[slot],
        ))
    });

    let mut setup_rng = ChaCha20Rng::from_seed([0x73; 32]);
    let (proving_key, verifying_key) =
        Groth16::<Bn254>::circuit_specific_setup(FoldCircuit::blank(), &mut setup_rng)?;
    let mut proof_rng = ChaCha20Rng::from_seed([0xb4; 32]);
    let proof = Groth16::<Bn254>::prove(
        &proving_key,
        FoldCircuit::from_witness(witness.clone()),
        &mut proof_rng,
    )?;
    let ic = verifying_key
        .gamma_abc_g1
        .iter()
        .copied()
        .map(g1_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "fold verifying key must have two IC points")?;
    Ok(SolanaFoldFixture {
        vk_alpha_g1: g1_bytes(verifying_key.alpha_g1),
        vk_beta_g2: g2_bytes(verifying_key.beta_g2),
        vk_gamma_g2: g2_bytes(verifying_key.gamma_g2),
        vk_delta_g2: g2_bytes(verifying_key.delta_g2),
        vk_ic: ic,
        proof_a: g1_bytes(proof.a.neg()),
        proof_b: g2_bytes(proof.b),
        proof_c: g1_bytes(proof.c),
        public_input: field_bytes(digest),
        auction: context_pubkey(witness.auction_context),
        old_folded_count: witness.old_folded_count,
        order_keys,
        order_digests,
        old_aggregate,
        new_aggregate,
    })
}

/// Export the deterministic Devnet fold verification key without generating a
/// proof. Production deployments must replace this setup with ceremony output.
pub fn deterministic_fold_verification_key(
) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
    let mut setup_rng = ChaCha20Rng::from_seed([0x73; 32]);
    let (_, key) = Groth16::<Bn254>::circuit_specific_setup(FoldCircuit::blank(), &mut setup_rng)?;
    verification_key_bytes(&key, "fold")
}

pub(super) fn verification_key_bytes(
    key: &ark_groth16::VerifyingKey<Bn254>,
    label: &str,
) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
    let ic = key
        .gamma_abc_g1
        .iter()
        .copied()
        .map(g1_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| std::io::Error::other(format!("{label} key must have two IC points")))?;
    Ok(SolanaVerificationKey {
        alpha_g1: g1_bytes(key.alpha_g1),
        beta_g2: g2_bytes(key.beta_g2),
        gamma_g2: g2_bytes(key.gamma_g2),
        delta_g2: g2_bytes(key.delta_g2),
        ic,
    })
}

pub(super) fn field_bytes_le(value: Fq) -> [u8; 32] {
    let bytes = value.into_bigint().to_bytes_le();
    let mut result = [0u8; 32];
    result[..bytes.len()].copy_from_slice(&bytes);
    result
}

pub(super) fn wire_point_bytes(point: EdwardsAffine) -> [u8; 32] {
    let mut result = field_bytes_le(point.y);
    if point.x.into_bigint().get_bit(0) {
        result[31] |= 0x80;
    }
    result
}

pub(super) fn decode_wire_point(
    encoded: [u8; 32],
) -> Result<EdwardsAffine, Box<dyn std::error::Error>> {
    let point = decode_wire_point_allow_identity(encoded)?;
    if point.is_zero() {
        return Err(std::io::Error::other("identity is not a valid Baby Jubjub public key").into());
    }
    Ok(point)
}

pub(super) fn decode_wire_point_allow_identity(
    mut encoded: [u8; 32],
) -> Result<EdwardsAffine, Box<dyn std::error::Error>> {
    let expected_x_parity = encoded[31] >> 7;
    encoded[31] &= 0x7f;
    let y = Fq::from_le_bytes_mod_order(&encoded);
    if field_bytes_le(y) != encoded {
        return Err(std::io::Error::other("non-canonical Baby Jubjub y-coordinate").into());
    }
    let mut point = EdwardsAffine::get_point_from_y_unchecked(y, false)
        .ok_or_else(|| std::io::Error::other("invalid Baby Jubjub point"))?;
    if u8::from(point.x.into_bigint().get_bit(0)) != expected_x_parity {
        point = EdwardsAffine::new_unchecked(-point.x, point.y);
    }
    if !point.is_on_curve() || !point.is_in_correct_subgroup_assuming_on_curve() {
        return Err(std::io::Error::other("invalid Baby Jubjub subgroup point").into());
    }
    Ok(point)
}

pub(super) fn scalar_bytes(value: JubjubScalar) -> [u8; 32] {
    let bytes = value.into_bigint().to_bytes_le();
    let mut result = [0u8; 32];
    result[..bytes.len()].copy_from_slice(&bytes);
    result
}

pub(super) fn scalar_from_bytes(
    encoded: [u8; 32],
) -> Result<JubjubScalar, Box<dyn std::error::Error>> {
    let scalar = JubjubScalar::from_le_bytes_mod_order(&encoded);
    if scalar_bytes(scalar) != encoded {
        return Err(std::io::Error::other("non-canonical Baby Jubjub scalar").into());
    }
    Ok(scalar)
}

pub(super) fn field_from_bytes_le(encoded: [u8; 32]) -> Result<Fq, Box<dyn std::error::Error>> {
    let value = Fq::from_le_bytes_mod_order(&encoded);
    if field_bytes_le(value) != encoded {
        return Err(std::io::Error::other("non-canonical BN254 field element").into());
    }
    Ok(value)
}

pub fn deterministic_committee_key_bytes() -> [u8; 32] {
    wire_point_bytes(OrderWitness::deterministic().committee_key.into_affine())
}

pub(super) fn ciphertext_bytes(ciphertext: Ciphertext) -> [u8; 64] {
    let mut result = [0u8; 64];
    result[..32].copy_from_slice(&wire_point_bytes(ciphertext.ephemeral));
    result[32..].copy_from_slice(&wire_point_bytes(ciphertext.body));
    result
}

pub(super) fn context_pubkey(context: [Fq; 2]) -> [u8; 32] {
    let low = field_bytes_le(context[0]);
    let high = field_bytes_le(context[1]);
    assert!(low[16..].iter().all(|byte| *byte == 0));
    assert!(high[16..].iter().all(|byte| *byte == 0));
    let mut result = [0u8; 32];
    result[..16].copy_from_slice(&low[..16]);
    result[16..].copy_from_slice(&high[..16]);
    result
}

pub(super) fn pubkey_context(bytes: [u8; 32]) -> [Fq; 2] {
    [
        Fq::from_le_bytes_mod_order(&bytes[..16]),
        Fq::from_le_bytes_mod_order(&bytes[16..]),
    ]
}

fn reverse_chunks<const CHUNK_SIZE: usize, const ARRAY_SIZE: usize>(
    bytes: &[u8; ARRAY_SIZE],
) -> [u8; ARRAY_SIZE] {
    let mut result = [0_u8; ARRAY_SIZE];
    for (source, destination) in bytes
        .chunks_exact(CHUNK_SIZE)
        .zip(result.chunks_exact_mut(CHUNK_SIZE))
    {
        for (left, right) in source.iter().zip(destination.iter_mut().rev()) {
            *right = *left;
        }
    }
    result
}

pub(crate) fn g1_bytes(point: G1Affine) -> [u8; 64] {
    let mut bytes = [0_u8; 64];
    point
        .x
        .serialize_with_mode(&mut bytes[..32], Compress::No)
        .expect("serialize G1 x");
    point
        .y
        .serialize_with_mode(&mut bytes[32..], Compress::No)
        .expect("serialize G1 y");
    reverse_chunks::<32, 64>(&bytes)
}

pub(crate) fn g2_bytes(point: G2Affine) -> [u8; 128] {
    let mut bytes = [0_u8; 128];
    point
        .serialize_with_mode(&mut bytes[..], Compress::No)
        .expect("serialize G2");
    reverse_chunks::<64, 128>(&bytes)
}

pub(crate) fn field_bytes(value: Fq) -> [u8; 32] {
    let encoded = value.into_bigint().to_bytes_be();
    let mut result = [0_u8; 32];
    result[32 - encoded.len()..].copy_from_slice(&encoded);
    result
}

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cs = ConstraintSystem::<Fq>::new_ref();
    cs.set_mode(ark_relations::r1cs::SynthesisMode::Setup);
    TransitionCircuit::blank().generate_constraints(cs.clone())?;
    cs.finalize();
    println!("BeatEx independent queued-order submission circuit");
    println!(
        "queued_order_constraints={} instance_variables={} witness_variables={}",
        cs.num_constraints(),
        cs.num_instance_variables(),
        cs.num_witness_variables()
    );

    let witness = TransitionWitness::deterministic();
    let digest = witness.digest();
    let setup_started = Instant::now();
    let mut setup_rng = ChaCha20Rng::from_seed([0x32; 32]);
    let (proving_key, verifying_key) =
        Groth16::<Bn254>::circuit_specific_setup(TransitionCircuit::blank(), &mut setup_rng)?;
    let setup_time = setup_started.elapsed();
    let mut proving_key_bytes = Vec::new();
    proving_key.serialize_compressed(&mut proving_key_bytes)?;
    let prepared_key = Groth16::<Bn254>::process_vk(&verifying_key)?;

    let mut prove_times = Vec::new();
    let mut verify_times = Vec::new();
    for round in 0..3u8 {
        let mut proof_rng = ChaCha20Rng::from_seed([0xa0 + round; 32]);
        let prove_started = Instant::now();
        let proof = Groth16::<Bn254>::prove(
            &proving_key,
            TransitionCircuit::from_witness(witness.clone()),
            &mut proof_rng,
        )?;
        prove_times.push(prove_started.elapsed());
        let verify_started = Instant::now();
        assert!(Groth16::<Bn254>::verify_with_processed_vk(
            &prepared_key,
            &[digest],
            &proof,
        )?);
        verify_times.push(verify_started.elapsed());
    }
    println!(
        "queued_order_setup_ms={:.3}, queued_order_prove_median_ms={:.3}, queued_order_verify_median_ms={:.3}, queued_order_pk_bytes={}",
        setup_time.as_secs_f64() * 1_000.0,
        median(prove_times).as_secs_f64() * 1_000.0,
        median(verify_times).as_secs_f64() * 1_000.0,
        proving_key_bytes.len(),
    );
    println!(
        "queued_order_wire_bytes={} (order ciphertexts + two new balance ciphertexts + proof + digest + order commitment)",
        ORDER_CIPHERTEXTS * 64 + 2 * 64 + 256 + 32 + 32
    );
    run_fold_benchmark()?;
    Ok(())
}

fn run_fold_benchmark() -> Result<(), Box<dyn std::error::Error>> {
    let cs = ConstraintSystem::<Fq>::new_ref();
    cs.set_mode(ark_relations::r1cs::SynthesisMode::Setup);
    FoldCircuit::blank().generate_constraints(cs.clone())?;
    cs.finalize();
    println!("BeatEx permissionless four-order fold circuit");
    println!(
        "fold_constraints={} instance_variables={} witness_variables={}",
        cs.num_constraints(),
        cs.num_instance_variables(),
        cs.num_witness_variables()
    );

    let witness = FoldWitness::deterministic();
    let digest = witness.digest();
    let setup_started = Instant::now();
    let mut setup_rng = ChaCha20Rng::from_seed([0x52; 32]);
    let (proving_key, verifying_key) =
        Groth16::<Bn254>::circuit_specific_setup(FoldCircuit::blank(), &mut setup_rng)?;
    let setup_time = setup_started.elapsed();
    let mut proving_key_bytes = Vec::new();
    proving_key.serialize_compressed(&mut proving_key_bytes)?;
    let prepared_key = Groth16::<Bn254>::process_vk(&verifying_key)?;
    let mut prove_times = Vec::new();
    let mut verify_times = Vec::new();
    for round in 0..3u8 {
        let mut proof_rng = ChaCha20Rng::from_seed([0xc0 + round; 32]);
        let prove_started = Instant::now();
        let proof = Groth16::<Bn254>::prove(
            &proving_key,
            FoldCircuit::from_witness(witness.clone()),
            &mut proof_rng,
        )?;
        prove_times.push(prove_started.elapsed());
        let verify_started = Instant::now();
        assert!(Groth16::<Bn254>::verify_with_processed_vk(
            &prepared_key,
            &[digest],
            &proof,
        )?);
        verify_times.push(verify_started.elapsed());
    }
    println!(
        "fold_setup_ms={:.3}, fold_prove_median_ms={:.3}, fold_verify_median_ms={:.3}, fold_pk_bytes={}",
        setup_time.as_secs_f64() * 1_000.0,
        median(prove_times).as_secs_f64() * 1_000.0,
        median(verify_times).as_secs_f64() * 1_000.0,
        proving_key_bytes.len(),
    );
    println!(
        "fold_wire_bytes={} (new aggregate ciphertexts + proof + statement digest)",
        ORDER_CIPHERTEXTS * 64 + 256 + 32
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_queued_order_is_satisfied() {
        let witness = TransitionWitness::deterministic();
        let circuit = TransitionCircuit::from_witness(witness);
        let cs = ConstraintSystem::<Fq>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    #[ignore = "generates a full 234k-constraint Groth16 proof; run in release mode"]
    fn arbitrary_order_prover_generates_a_verified_transition() {
        let prover = OrderProver::deterministic_devnet().unwrap();
        let mut buy = [0u64; LEVELS];
        let mut sell = [0u64; LEVELS];
        buy[3] = 7;
        sell[12] = 5;
        let request = OrderProofRequest {
            auction: [0xa1; 32],
            owner: [0xb2; 32],
            client_order_nonce: 19,
            reference_price: 100_000,
            tick_size: 500,
            committee_key: deterministic_committee_key_bytes(),
            buy,
            sell,
            old_balance: PrivateBalanceOpening {
                base_value: 100,
                quote_value: 10_000_000,
                base_randomness: [0; 32],
                quote_randomness: [0; 32],
            },
        };
        let mut rng = ChaCha20Rng::from_seed([0xd7; 32]);
        let output = prover.prove(request, &mut rng).unwrap();
        assert_eq!(output.base_reserve, 5);
        assert_eq!(output.quote_reserve, 7 * 97_500);
        assert_eq!(output.new_balance.base_value, 95);
        assert_eq!(output.new_balance.quote_value, 10_000_000 - 7 * 97_500);
        assert_ne!(output.order_commitment, [0; 32]);
        assert_ne!(output.public_input, [0; 32]);
    }

    #[test]
    fn wire_decoders_reject_identity_and_noncanonical_values() {
        let identity = wire_point_bytes(EdwardsAffine::zero());
        assert!(decode_wire_point(identity).is_err());
        assert_eq!(
            decode_wire_point_allow_identity(identity).unwrap(),
            EdwardsAffine::zero()
        );
        assert!(decode_wire_point([0; 32]).is_err());
        assert!(scalar_from_bytes([0xff; 32]).is_err());
        let point = OrderWitness::deterministic().committee_key.into_affine();
        assert_eq!(decode_wire_point(wire_point_bytes(point)).unwrap(), point);
    }

    #[test]
    fn mutated_order_ciphertext_is_rejected() {
        let witness = TransitionWitness::deterministic();
        let digest = witness.digest();
        let mut changed = witness.clone();
        changed.order.randomness[0] += JubjubScalar::from(1u64);
        let circuit = TransitionCircuit {
            witness: Some(changed),
            digest: Some(digest),
        };
        let cs = ConstraintSystem::<Fq>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn mutated_balance_ciphertext_is_rejected() {
        let witness = TransitionWitness::deterministic();
        let digest = witness.digest();
        let mut changed = witness.clone();
        changed.balance_randomness[2] += JubjubScalar::from(1u64);
        let circuit = TransitionCircuit {
            witness: Some(changed),
            digest: Some(digest),
        };
        let cs = ConstraintSystem::<Fq>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn insufficient_available_balance_has_no_witness() {
        let witness = TransitionWitness::deterministic();
        let digest = witness.digest();
        let mut changed = witness;
        changed.old_base_balance = 0;
        let circuit = TransitionCircuit {
            witness: Some(changed),
            digest: Some(digest),
        };
        let cs = ConstraintSystem::<Fq>::new_ref();
        assert!(circuit.generate_constraints(cs).is_err());
    }

    #[test]
    fn valid_four_slot_fold_is_satisfied() {
        let witness = FoldWitness::deterministic();
        let circuit = FoldCircuit::from_witness(witness);
        let cs = ConstraintSystem::<Fq>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    #[ignore = "generates a full fold Groth16 proof; run in release mode"]
    fn arbitrary_fold_prover_matches_the_replacement_aggregate() {
        let witness = FoldWitness::deterministic();
        let expected: [[u8; 64]; ORDER_CIPHERTEXTS] = witness
            .new_aggregate()
            .into_iter()
            .map(ciphertext_bytes)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let events = (0..witness.active_count)
            .map(|slot| FoldEventInput {
                event: context_pubkey(witness.order_contexts[slot]),
                previous_ciphertexts: witness.previous_orders[slot]
                    .iter()
                    .copied()
                    .map(ciphertext_bytes)
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
                ciphertexts: witness.orders[slot]
                    .iter()
                    .copied()
                    .map(ciphertext_bytes)
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
            })
            .collect();
        let request = FoldProofRequest {
            auction: context_pubkey(witness.auction_context),
            old_folded_count: witness.old_folded_count,
            old_aggregate: witness
                .old_aggregate
                .iter()
                .copied()
                .map(ciphertext_bytes)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
            events,
        };
        let prover = FoldProver::deterministic_devnet().unwrap();
        let mut rng = ChaCha20Rng::from_seed([0x4f; 32]);
        let output = prover.prove(request, &mut rng).unwrap();
        assert_eq!(output.new_aggregate, expected);
        assert_ne!(output.public_input, [0; 32]);
    }

    #[test]
    fn mutated_fold_order_is_rejected() {
        let witness = FoldWitness::deterministic();
        let digest = witness.digest();
        let mut changed = witness;
        changed.orders[0][0].ephemeral = (changed.orders[0][0].ephemeral.into_group()
            + EdwardsProjective::generator())
        .into_affine();
        let circuit = FoldCircuit {
            witness: Some(changed),
            digest: Some(digest),
        };
        let cs = ConstraintSystem::<Fq>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }
}
