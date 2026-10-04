//! Token-2022 confidential-vault CPI helpers.
//!
//! Proof generation stays offchain. These helpers consume already-verified ZK
//! proof-context accounts and sign only with the market vault-authority PDA.

use {
    anchor_lang::{
        prelude::*,
        solana_program::program::{invoke, invoke_signed},
    },
    solana_zk_sdk_pod::encryption::{
        auth_encryption::PodAeCiphertext,
        elgamal::{PodElGamalCiphertext, PodElGamalPubkey},
    },
    spl_token_2022_interface::{
        extension::{
            confidential_transfer::{
                instruction::{
                    deposit as confidential_deposit, inner_apply_pending_balance, inner_transfer,
                    BatchedGroupedCiphertext3HandlesValidityProofData, BatchedRangeProofU128Data,
                    CiphertextCommitmentEqualityProofData,
                },
                ConfidentialTransferAccount,
            },
            BaseStateWithExtensions, StateWithExtensions,
        },
        instruction::transfer_checked,
        state::{Account as TokenAccount, Mint},
    },
    spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation,
};

use crate::BeatExError;

pub const AE_CIPHERTEXT_LEN: usize = 36;

pub fn mint_decimals(mint_data: &[u8]) -> Result<u8> {
    let mint = StateWithExtensions::<Mint>::unpack(mint_data)
        .map_err(|_| error!(BeatExError::InvalidDepositAccount))?;
    require!(mint.base.is_initialized, BeatExError::InvalidDepositAccount);
    Ok(mint.base.decimals)
}

pub fn validate_public_token_account(
    token_account_data: &[u8],
    expected_mint: &Pubkey,
    expected_owner: &Pubkey,
) -> Result<u64> {
    let token_account = StateWithExtensions::<TokenAccount>::unpack(token_account_data)
        .map_err(|_| error!(BeatExError::InvalidDepositAccount))?;
    require_keys_eq!(
        token_account.base.mint,
        *expected_mint,
        BeatExError::InvalidDepositAccount
    );
    require_keys_eq!(
        token_account.base.owner,
        *expected_owner,
        BeatExError::InvalidDepositAccount
    );
    Ok(token_account.base.amount)
}

#[allow(clippy::too_many_arguments)]
pub fn invoke_public_transfer_checked<'info>(
    token_program: &AccountInfo<'info>,
    source: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    owner: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
) -> Result<()> {
    require_keys_eq!(
        *token_program.key,
        spl_token_2022_interface::id(),
        BeatExError::InvalidTokenProgram
    );
    let instruction = transfer_checked(
        token_program.key,
        source.key,
        mint.key,
        destination.key,
        owner.key,
        &[],
        amount,
        decimals,
    )?;
    invoke(
        &instruction,
        &[
            source.clone(),
            mint.clone(),
            destination.clone(),
            owner.clone(),
            token_program.clone(),
        ],
    )?;
    Ok(())
}

pub fn invoke_confidential_deposit<'info>(
    token_program: &AccountInfo<'info>,
    token_account: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require_keys_eq!(
        *token_program.key,
        spl_token_2022_interface::id(),
        BeatExError::InvalidTokenProgram
    );
    let instruction = confidential_deposit(
        token_program.key,
        token_account.key,
        mint.key,
        amount,
        decimals,
        authority.key,
        &[],
    )?;
    invoke_signed(
        &instruction,
        &[
            token_account.clone(),
            mint.clone(),
            authority.clone(),
            token_program.clone(),
        ],
        signer_seeds,
    )?;
    Ok(())
}

pub fn validate_confidential_deposit_deltas(
    source_before: u64,
    source_after: u64,
    vault_before: u64,
    vault_after: u64,
    amount: u64,
) -> Result<()> {
    // The first CPI moves `amount` into the vault's public balance and the
    // second immediately deposits the same amount into confidential pending.
    // Therefore the vault's net public balance must remain unchanged.
    require!(
        source_before.checked_sub(source_after) == Some(amount) && vault_after == vault_before,
        BeatExError::NonExactDeposit
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn invoke_confidential_transfer<'info>(
    token_program: &AccountInfo<'info>,
    source: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    equality_proof_context: &AccountInfo<'info>,
    ciphertext_validity_proof_context: &AccountInfo<'info>,
    range_proof_context: &AccountInfo<'info>,
    new_source_decryptable_available_balance: [u8; AE_CIPHERTEXT_LEN],
    auditor_ciphertext_lo: [u8; 64],
    auditor_ciphertext_hi: [u8; 64],
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require_keys_eq!(
        *token_program.key,
        spl_token_2022_interface::id(),
        BeatExError::InvalidTokenProgram
    );
    let instruction = inner_transfer(
        token_program.key,
        source.key,
        mint.key,
        destination.key,
        &PodAeCiphertext(new_source_decryptable_available_balance),
        &PodElGamalCiphertext(auditor_ciphertext_lo),
        &PodElGamalCiphertext(auditor_ciphertext_hi),
        authority.key,
        &[],
        ProofLocation::<CiphertextCommitmentEqualityProofData>::ContextStateAccount(
            equality_proof_context.key,
        ),
        ProofLocation::<BatchedGroupedCiphertext3HandlesValidityProofData>::ContextStateAccount(
            ciphertext_validity_proof_context.key,
        ),
        ProofLocation::<BatchedRangeProofU128Data>::ContextStateAccount(range_proof_context.key),
    )?;

    invoke_signed(
        &instruction,
        &[
            source.clone(),
            mint.clone(),
            destination.clone(),
            equality_proof_context.clone(),
            ciphertext_validity_proof_context.clone(),
            range_proof_context.clone(),
            authority.clone(),
            token_program.clone(),
        ],
        signer_seeds,
    )?;
    Ok(())
}

pub fn invoke_apply_pending_balance<'info>(
    token_program: &AccountInfo<'info>,
    token_account: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    expected_pending_balance_credit_counter: u64,
    new_decryptable_available_balance: [u8; AE_CIPHERTEXT_LEN],
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require_keys_eq!(
        *token_program.key,
        spl_token_2022_interface::id(),
        BeatExError::InvalidTokenProgram
    );
    let instruction = inner_apply_pending_balance(
        token_program.key,
        token_account.key,
        expected_pending_balance_credit_counter,
        &PodAeCiphertext(new_decryptable_available_balance),
        authority.key,
        &[],
    )?;
    invoke_signed(
        &instruction,
        &[
            token_account.clone(),
            authority.clone(),
            token_program.clone(),
        ],
        signer_seeds,
    )?;
    Ok(())
}

pub fn validate_confidential_vault(
    token_account_data: &[u8],
    expected_mint: &Pubkey,
    expected_authority: &Pubkey,
    expected_elgamal_pubkey: &[u8; 32],
) -> Result<()> {
    let token_account = StateWithExtensions::<TokenAccount>::unpack(token_account_data)
        .map_err(|_| error!(BeatExError::InvalidConfidentialVault))?;
    require_keys_eq!(
        token_account.base.mint,
        *expected_mint,
        BeatExError::InvalidConfidentialVault
    );
    require_keys_eq!(
        token_account.base.owner,
        *expected_authority,
        BeatExError::InvalidConfidentialVault
    );
    let confidential = token_account
        .get_extension::<ConfidentialTransferAccount>()
        .map_err(|_| error!(BeatExError::InvalidConfidentialVault))?;
    confidential
        .approved()
        .map_err(|_| error!(BeatExError::InvalidConfidentialVault))?;
    require!(
        confidential.elgamal_pubkey == PodElGamalPubkey(*expected_elgamal_pubkey),
        BeatExError::InvalidConfidentialVault
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::{program_option::COption, program_pack::Pack};
    use spl_token_2022_interface::state::AccountState;

    fn token_account_data(mint: Pubkey, owner: Pubkey, amount: u64) -> Vec<u8> {
        let account = TokenAccount {
            mint,
            owner,
            amount,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };
        let mut data = vec![0; TokenAccount::LEN];
        TokenAccount::pack(account, &mut data).unwrap();
        data
    }

    #[test]
    fn public_deposit_account_validation_binds_mint_owner_and_amount() {
        let mint = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let data = token_account_data(mint, owner, 123);
        assert_eq!(
            validate_public_token_account(&data, &mint, &owner).unwrap(),
            123
        );
        assert!(validate_public_token_account(&data, &Pubkey::new_unique(), &owner).is_err());
        assert!(validate_public_token_account(&data, &mint, &Pubkey::new_unique()).is_err());
    }

    #[test]
    fn confidential_deposit_requires_exact_source_debit_and_zero_public_vault_delta() {
        validate_confidential_deposit_deltas(100, 93, 20, 20, 7).unwrap();
        assert!(validate_confidential_deposit_deltas(100, 94, 20, 20, 7).is_err());
        assert!(validate_confidential_deposit_deltas(100, 93, 20, 27, 7).is_err());
        assert!(validate_confidential_deposit_deltas(100, 93, 20, 19, 7).is_err());
    }
}
