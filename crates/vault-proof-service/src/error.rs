use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum VaultProofError {
    #[error("the threshold configuration is not the prototype 3-of-7 configuration")]
    InvalidThresholdConfiguration,
    #[error("threshold key reconstruction failed: {0}")]
    ThresholdReconstruction(String),
    #[error("the reconstructed key does not match the vault ElGamal public key")]
    SourceKeyMismatch,
    #[error("the source and destination token accounts must be distinct")]
    SelfTransferNotSupported,
    #[error("the source and destination mints differ")]
    MintMismatch,
    #[error("the transfer amount exceeds Token-2022's 48-bit confidential-transfer limit")]
    TransferAmountTooLarge,
    #[error("the source available-balance ciphertext is malformed")]
    InvalidAvailableBalance,
    #[error("the source decryptable available balance is malformed or uses the wrong AE key")]
    InvalidDecryptableBalance,
    #[error("the destination ElGamal public key is malformed")]
    InvalidDestinationKey,
    #[error("the auditor ElGamal public key is malformed")]
    InvalidAuditorKey,
    #[error("the source vault does not have enough available balance")]
    InsufficientFunds,
    #[error("Token-2022 proof generation failed: {0}")]
    ProofGeneration(String),
    #[error("private withdrawal Groth16 proof generation failed: {0}")]
    PrivateWithdrawalProofGeneration(String),
    #[error("host verification failed for {0}")]
    ProofVerification(&'static str),
    #[error("the three Token-2022 proof contexts are not mutually consistent")]
    InconsistentProofContexts,
    #[error("a generated Token-2022 proof context contains malformed ciphertext data")]
    InvalidGeneratedContext,
    #[error("the private order opening does not match the queued order commitment")]
    OrderCommitmentMismatch,
    #[error("the private withdrawal opening does not match the submitted terminal nullifier")]
    NullifierMismatch,
    #[error("the circuit and on-chain private-withdrawal transcripts differ")]
    TranscriptMismatch,
    #[error("the withdrawal account addresses do not match the generated Token-2022 proofs")]
    StatementAccountMismatch,
}
