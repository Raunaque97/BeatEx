use {
    crate::VaultProofError,
    beatex_threshold_crypto::{
        reconstruct_elgamal_keypair, ParticipantSecretShare, ThresholdPublicConfig,
    },
    solana_zk_sdk_v4::encryption::{
        auth_encryption::AeKey,
        elgamal::{ElGamalKeypair, ElGamalSecretKey},
    },
    zeroize::Zeroizing,
};

/// A short-lived reconstructed Token-2022 source key.
///
/// The wrapped SDK keypair zeroizes its secret on drop and this type redacts it
/// from debug output. It should exist only while generating one proof bundle.
pub struct VaultElGamalKeypair {
    pub(crate) inner: ElGamalKeypair,
}

impl VaultElGamalKeypair {
    /// Reconstruct a prototype 3-of-7 key and verify it against the registered
    /// committee public key before converting it to Token-2022 v11's pinned
    /// proof-generation SDK type.
    pub fn reconstruct_3_of_7(
        config: &ThresholdPublicConfig,
        shares: [&ParticipantSecretShare; 3],
    ) -> Result<Self, VaultProofError> {
        if config.threshold != 3 || config.participant_count != 7 {
            return Err(VaultProofError::InvalidThresholdConfiguration);
        }
        let reconstructed = reconstruct_elgamal_keypair(config, &shares)
            .map_err(|error| VaultProofError::ThresholdReconstruction(error.to_string()))?;

        // Token-2022 v11's proof-generation crate is pinned to solana-zk-sdk
        // v4, while the committee crate uses v7. The canonical scalar encoding
        // is the compatibility boundary. Keep the copied bytes zeroizing.
        let secret_bytes = Zeroizing::new(*reconstructed.secret().as_bytes());
        let secret = ElGamalSecretKey::try_from(secret_bytes.as_slice()).map_err(|_| {
            VaultProofError::ThresholdReconstruction(
                "reconstructed scalar is not canonical in the v4 SDK".into(),
            )
        })?;
        let inner = ElGamalKeypair::new(secret);
        let actual: [u8; 32] = inner.pubkey().into();
        if actual != config.encryption_pubkey.to_bytes() {
            return Err(VaultProofError::SourceKeyMismatch);
        }
        Ok(Self { inner })
    }

    pub fn pubkey_bytes(&self) -> [u8; 32] {
        self.inner.pubkey().into()
    }
}

impl core::fmt::Debug for VaultElGamalKeypair {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VaultElGamalKeypair")
            .field("pubkey", &self.pubkey_bytes())
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

/// The authenticated-encryption key for a vault's decryptable balance.
pub struct VaultAeKey {
    pub(crate) inner: AeKey,
}

impl VaultAeKey {
    pub fn new_rand() -> Self {
        Self {
            inner: AeKey::new_rand(),
        }
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self {
            inner: AeKey::from(bytes),
        }
    }

    pub fn encrypt_balance(&self, balance: u64) -> [u8; 36] {
        self.inner.encrypt(balance).to_bytes()
    }
}

impl core::fmt::Debug for VaultAeKey {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_tuple("VaultAeKey")
            .field(&"[REDACTED]")
            .finish()
    }
}
