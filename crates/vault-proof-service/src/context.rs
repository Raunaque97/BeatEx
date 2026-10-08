//! Transaction-ready proof-context planning for Token-2022 withdrawals.
//!
//! Each proof is deliberately emitted as its own verification instruction.
//! This keeps even the U128 range proof below Solana's transaction-size limit;
//! a coordinator creates the six exact-sized context accounts first, then
//! submits these six instructions and finally relays the BeatEx withdrawal.

use {
    crate::WithdrawalProofBundle,
    solana_instruction::Instruction,
    solana_pubkey::Pubkey,
    solana_zk_elgamal_proof_interface::{
        instruction::{close_context_state, ContextStateInfo, ProofInstruction},
        proof_data::{
            batched_grouped_ciphertext_validity::BatchedGroupedCiphertext3HandlesValidityProofContext,
            batched_range_proof::BatchedRangeProofContext,
            ciphertext_commitment_equality::CiphertextCommitmentEqualityProofContext,
        },
        state::ProofContextState,
    },
    std::mem::size_of,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransferContextAddresses {
    pub equality: Pubkey,
    pub validity: Pubkey,
    pub range: Pubkey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WithdrawalContextAddresses {
    pub base: TransferContextAddresses,
    pub quote: TransferContextAddresses,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProofContextAccountSpec {
    pub address: Pubkey,
    pub owner: Pubkey,
    pub space: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferContextPlan {
    pub accounts: [ProofContextAccountSpec; 3],
    /// System-program allocations. Both payer and each fresh context keypair
    /// sign their respective transaction.
    pub create_instructions: [Instruction; 3],
    /// Equality, grouped-ciphertext validity, then range verification.
    pub verify_instructions: [Instruction; 3],
    /// Close after the atomic BeatEx withdrawal has consumed the contexts.
    pub close_instructions: [Instruction; 3],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WithdrawalContextPlan {
    pub base: TransferContextPlan,
    pub quote: TransferContextPlan,
}

fn transfer_plan(
    bundle: &crate::TransferProofBundle,
    addresses: TransferContextAddresses,
    authority: Pubkey,
    close_destination: Pubkey,
    payer: Pubkey,
    rent_lamports: [u64; 3],
) -> TransferContextPlan {
    let proof_program = solana_zk_elgamal_proof_interface::id();
    let accounts = [
        ProofContextAccountSpec {
            address: addresses.equality,
            owner: proof_program,
            space: size_of::<ProofContextState<CiphertextCommitmentEqualityProofContext>>(),
        },
        ProofContextAccountSpec {
            address: addresses.validity,
            owner: proof_program,
            space: size_of::<ProofContextState<BatchedGroupedCiphertext3HandlesValidityProofContext>>(
            ),
        },
        ProofContextAccountSpec {
            address: addresses.range,
            owner: proof_program,
            space: size_of::<ProofContextState<BatchedRangeProofContext>>(),
        },
    ];
    let equality_context = ContextStateInfo {
        context_state_account: &addresses.equality,
        context_state_authority: &authority,
    };
    let validity_context = ContextStateInfo {
        context_state_account: &addresses.validity,
        context_state_authority: &authority,
    };
    let range_context = ContextStateInfo {
        context_state_account: &addresses.range,
        context_state_authority: &authority,
    };
    let verify_instructions = [
        ProofInstruction::VerifyCiphertextCommitmentEquality
            .encode_verify_proof(Some(equality_context), &bundle.equality_proof),
        ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity
            .encode_verify_proof(Some(validity_context), &bundle.ciphertext_validity_proof),
        ProofInstruction::VerifyBatchedRangeProofU128
            .encode_verify_proof(Some(range_context), &bundle.range_proof),
    ];
    let create_instructions = std::array::from_fn(|index| {
        solana_system_interface::instruction::create_account(
            &payer,
            &accounts[index].address,
            rent_lamports[index],
            accounts[index].space as u64,
            &accounts[index].owner,
        )
    });
    let close_instructions = [
        close_context_state(equality_context, &close_destination),
        close_context_state(validity_context, &close_destination),
        close_context_state(range_context, &close_destination),
    ];
    TransferContextPlan {
        accounts,
        create_instructions,
        verify_instructions,
        close_instructions,
    }
}

/// Build all six proof-program instructions and exact account allocations for
/// one two-asset claim or refund. RPC submission remains intentionally outside
/// this cryptographic crate; the returned plan is directly consumable by a
/// Solana transaction sender.
pub fn build_withdrawal_context_plan(
    bundle: &WithdrawalProofBundle,
    addresses: WithdrawalContextAddresses,
    authority: Pubkey,
    close_destination: Pubkey,
    payer: Pubkey,
    // Rent-exempt lamports for equality, validity and range context accounts.
    // The same context layouts are used for both assets.
    rent_lamports: [u64; 3],
) -> WithdrawalContextPlan {
    WithdrawalContextPlan {
        base: transfer_plan(
            &bundle.base,
            addresses.base,
            authority,
            close_destination,
            payer,
            rent_lamports,
        ),
        quote: transfer_plan(
            &bundle.quote,
            addresses.quote,
            authority,
            close_destination,
            payer,
            rent_lamports,
        ),
    }
}
