//! Offchain proof-generation primitives for BeatEx's shared Token-2022 vaults.
//!
//! This crate deliberately contains no allocation policy and performs no RPC
//! writes. It accepts typed snapshots, reconstructs the threshold vault key for
//! the shortest possible lifetime, and produces the standard proof bundle that
//! Token-2022 v11 consumes from proof-context accounts.

mod context;
mod error;
mod keys;
mod proof;
mod state;

pub use context::{
    build_withdrawal_context_plan, ProofContextAccountSpec, TransferContextAddresses,
    TransferContextPlan, WithdrawalContextAddresses, WithdrawalContextPlan,
};
pub use error::VaultProofError;
pub use keys::{VaultAeKey, VaultElGamalKeypair};
pub use proof::{
    claim_bridge_witness, derive_private_order_identifiers,
    generate_private_withdrawal_groth16_proof, generate_transfer_proof,
    generate_withdrawal_proof_bundle, prepare_private_withdrawal_witness, ClaimBridgeWitness,
    TransferCommitments, TransferProofBundle, WithdrawalProofBundle,
};
pub use state::{
    ConfidentialDestination, PrivateOrderOpening, TransferRequest, VaultSnapshot,
    WithdrawalStatementContext, MAX_TRANSFER_AMOUNT,
};
