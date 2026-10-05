#![cfg(feature = "sbf-test-fixtures")]

use {
    anchor_lang::{
        prelude::Pubkey, solana_program::program_option::COption, AccountDeserialize,
        AccountSerialize, InstructionData,
    },
    beatex::{
        Auction, AuctionState, ConfidentialTransferArgs, DepositCollateralArgs, DleqProofInput,
        EncryptedPrivateBalance, Groth16ProofInput, KeyEpoch, Market, Order, OrderState,
        PrivateWithdrawalArgs, QueueCircuitKind, QueueVerificationKey, QueuedOrder,
        QueuedOrderState, WithdrawalState,
    },
    litesvm::LiteSVM,
    solana_account::Account,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::Message,
    solana_signer::Signer,
    solana_transaction::Transaction,
    solana_zk_sdk_pod::encryption::{
        elgamal::{PodElGamalCiphertext, PodElGamalPubkey},
        grouped_elgamal::PodGroupedElGamalCiphertext3Handles,
        pedersen::PodPedersenCommitment,
    },
    spl_token_2022_interface::{
        extension::{
            confidential_transfer::{
                instruction::{
                    BatchedGroupedCiphertext3HandlesValidityProofContext, BatchedRangeProofContext,
                    CiphertextCommitmentEqualityProofContext, ProofContextState, ProofType,
                },
                ConfidentialTransferAccount, ConfidentialTransferMint,
            },
            BaseStateWithExtensions, BaseStateWithExtensionsMut, ExtensionType,
            StateWithExtensions, StateWithExtensionsMut,
        },
        state::{Account as TokenAccount, AccountState, Mint},
    },
    std::path::PathBuf,
};

const ZK_ELGAMAL_PROOF_PROGRAM_ID: Pubkey =
    anchor_lang::pubkey!("ZkE1Gama1Proof11111111111111111111111111111");

fn program_path() -> PathBuf {
    std::env::var_os("BEATEX_SBF_SO")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            path.extend(["..", "..", "target", "deploy", "beatex.so"]);
            path
        })
}

fn svm_with_program() -> LiteSVM {
    let path = program_path();
    assert!(path.exists(), "build the test ELF before running this test");
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(beatex::ID, path).unwrap();
    // LiteSVM 0.10 embeds Token-2022 v10 without `zk-ops`; that binary logs
    // the confidential instruction and then returns InvalidInstructionData.
    // Override it with the official v11 release artifact, which re-enables
    // confidential transfers.
    let token_2022_path = std::env::var_os("TOKEN_2022_SBF_SO")
        .expect("set TOKEN_2022_SBF_SO to the official Token-2022 v11 SBF artifact");
    svm.add_program_from_file(spl_token_2022_interface::id(), token_2022_path)
        .unwrap();
    svm
}

fn put_anchor_account<T: AccountSerialize>(svm: &mut LiteSVM, address: Pubkey, value: &T) {
    let mut data = Vec::new();
    value.try_serialize(&mut data).unwrap();
    svm.set_account(
        address,
        Account {
            lamports: 1_000_000_000,
            data,
            owner: beatex::ID,
            ..Account::default()
        },
    )
    .unwrap();
}

fn put_system_account(svm: &mut LiteSVM, address: Pubkey) {
    svm.set_account(
        address,
        Account {
            lamports: 1_000_000,
            owner: anchor_lang::system_program::ID,
            ..Account::default()
        },
    )
    .unwrap();
}

fn confidential_mint_data() -> Vec<u8> {
    let len = ExtensionType::try_calculate_account_len::<Mint>(&[
        ExtensionType::ConfidentialTransferMint,
    ])
    .unwrap();
    let mut data = vec![0; len];
    let mut state = StateWithExtensionsMut::<Mint>::unpack_uninitialized(&mut data).unwrap();
    state.base = Mint {
        is_initialized: true,
        ..Mint::default()
    };
    state.pack_base();
    state
        .init_extension::<ConfidentialTransferMint>(false)
        .unwrap()
        .auto_approve_new_accounts = true.into();
    state.init_account_type().unwrap();
    data
}

fn confidential_token_account_data(
    mint: Pubkey,
    owner: Pubkey,
    elgamal_pubkey: [u8; 32],
    pending_credit_counter: u64,
    public_amount: u64,
) -> Vec<u8> {
    let len = ExtensionType::try_calculate_account_len::<TokenAccount>(&[
        ExtensionType::ConfidentialTransferAccount,
    ])
    .unwrap();
    let mut data = vec![0; len];
    let mut state =
        StateWithExtensionsMut::<TokenAccount>::unpack_uninitialized(&mut data).unwrap();
    state.base = TokenAccount {
        mint,
        owner,
        amount: public_amount,
        state: AccountState::Initialized,
        delegate: COption::None,
        is_native: COption::None,
        close_authority: COption::None,
        ..TokenAccount::default()
    };
    state.pack_base();
    let confidential = state
        .init_extension::<ConfidentialTransferAccount>(false)
        .unwrap();
    confidential.approved = true.into();
    confidential.elgamal_pubkey.0 = elgamal_pubkey;
    confidential.allow_confidential_credits = true.into();
    confidential.allow_non_confidential_credits = true.into();
    confidential.pending_balance_credit_counter = pending_credit_counter.into();
    confidential.maximum_pending_balance_credit_counter = 65_536_u64.into();
    state.init_account_type().unwrap();
    data
}

fn fixture_market(
    authority: Pubkey,
    market: Pubkey,
    key_epoch: Pubkey,
    base_mint: Pubkey,
    base_vault: Pubkey,
    quote_mint: Pubkey,
    quote_vault: Pubkey,
) -> (Market, KeyEpoch, Pubkey) {
    let (vault_authority, vault_authority_bump) =
        Pubkey::find_program_address(&[b"vault-authority", market.as_ref()], &beatex::ID);
    let elgamal_pubkey = beatex_threshold_crypto::dealer_setup_3_of_7(b"beatex/sbf/token-2022")
        .public
        .encryption_pubkey
        .to_bytes();
    let market_state = Market {
        version: 1,
        authority,
        base_mint,
        quote_mint,
        base_vault,
        quote_vault,
        base_lot_size: 1,
        quote_lot_size: 1,
        tick_size: 1,
        grid_radius: 1,
        max_order_quantity: beatex::queue::MAX_PRIVATE_ORDER_QUANTITY,
        max_aggregate_quantity: 10_000,
        current_key_epoch: key_epoch,
        market_start_slot: 0,
        last_clearing_price: 1,
        last_clearing_volume: 0,
        last_cleared_auction_sequence: 0,
        minimum_reference_volume: 1,
        oracle_price_account: Pubkey::new_unique(),
        oracle_feed_id: [1; 32],
        oracle_max_age_seconds: 90,
        oracle_max_confidence_bps: 100,
        oracle_exponent_adjustment: 0,
        min_reference_price: 1,
        max_reference_price: 10_000,
        submission_duration_slots: 10,
        aggregation_duration_slots: 10,
        decryption_duration_slots: 10,
        allocation_rule_version: 1,
        order_circuit_version: 1,
        claim_circuit_version: 1,
        paused: false,
        vault_authority_bump,
        bump: 0,
    };
    let proof = DleqProofInput {
        commitment_1: [0; 32],
        commitment_2: [0; 32],
        response: [0; 32],
    };
    let key_epoch_state = KeyEpoch {
        authority,
        market,
        epoch: 1,
        encryption_pubkey: elgamal_pubkey,
        babyjub_encryption_pubkey: {
            let mut identity = [0u8; 32];
            identity[0] = 1;
            identity
        },
        babyjub_participant_public_shares: [[0; 32]; beatex::COMMITTEE_SIZE],
        coefficient_commitments: [[0; 32]; 3],
        participant_public_shares: [[0; 32]; beatex::COMMITTEE_SIZE],
        participant_authorities: [Pubkey::default(); beatex::COMMITTEE_SIZE],
        key_relation_proof: proof,
        threshold: beatex::THRESHOLD,
        proof_transcript_version: 2,
        activation_slot: 1,
        retired: false,
        acknowledgement_bitmap: 0x7f,
        bump: 0,
    };
    (market_state, key_epoch_state, vault_authority)
}

fn put_token_2022_account(svm: &mut LiteSVM, address: Pubkey, data: Vec<u8>) {
    svm.set_account(
        address,
        Account {
            lamports: 1_000_000_000,
            data,
            owner: spl_token_2022_interface::id(),
            ..Account::default()
        },
    )
    .unwrap();
}

fn put_zero_transfer_proof_contexts(
    svm: &mut LiteSVM,
    source_pubkey: [u8; 32],
    destination_pubkey: [u8; 32],
) -> (Pubkey, Pubkey, Pubkey) {
    let context_authority = Pubkey::new_unique();
    let equality_address = Pubkey::new_unique();
    let validity_address = Pubkey::new_unique();
    let range_address = Pubkey::new_unique();

    // These are correctly encoded *preverified context-state accounts*. The
    // test intentionally injects them after verification, so it covers
    // Token-2022 context extraction/consistency and BeatEx CPI wiring, but not
    // native ZK ElGamal proof verification.
    let equality = CiphertextCommitmentEqualityProofContext {
        pubkey: PodElGamalPubkey(source_pubkey),
        ciphertext: PodElGamalCiphertext([0; 64]),
        commitment: PodPedersenCommitment([0; 32]),
    };
    let validity = BatchedGroupedCiphertext3HandlesValidityProofContext {
        first_pubkey: PodElGamalPubkey(source_pubkey),
        second_pubkey: PodElGamalPubkey(destination_pubkey),
        third_pubkey: PodElGamalPubkey([0; 32]),
        grouped_ciphertext_lo: PodGroupedElGamalCiphertext3Handles([0; 128]),
        grouped_ciphertext_hi: PodGroupedElGamalCiphertext3Handles([0; 128]),
    };
    let range = BatchedRangeProofContext {
        commitments: [PodPedersenCommitment([0; 32]); 8],
        bit_lengths: [64, 16, 32, 16, 0, 0, 0, 0],
    };
    for (address, data) in [
        (
            equality_address,
            ProofContextState::encode(
                &context_authority,
                ProofType::CiphertextCommitmentEquality,
                &equality,
            ),
        ),
        (
            validity_address,
            ProofContextState::encode(
                &context_authority,
                ProofType::BatchedGroupedCiphertext3HandlesValidity,
                &validity,
            ),
        ),
        (
            range_address,
            ProofContextState::encode(&context_authority, ProofType::BatchedRangeProofU128, &range),
        ),
    ] {
        svm.set_account(
            address,
            Account {
                lamports: 1_000_000_000,
                data,
                owner: ZK_ELGAMAL_PROOF_PROGRAM_ID,
                ..Account::default()
            },
        )
        .unwrap();
    }
    (equality_address, validity_address, range_address)
}

#[test]
#[ignore = "requires BeatEx SBF and TOKEN_2022_SBF_SO pointing to Token-2022 v11 with zk-ops"]
fn vault_pda_applies_pending_balance_through_token_2022_cpi() {
    let mut svm = svm_with_program();
    let authority = Keypair::new();
    svm.airdrop(&authority.pubkey(), 10_000_000_000).unwrap();

    let market = Pubkey::new_unique();
    let key_epoch = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    let vault = Pubkey::new_unique();
    let (market_state, key_epoch_state, vault_authority) = fixture_market(
        authority.pubkey(),
        market,
        key_epoch,
        mint,
        vault,
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    );
    put_anchor_account(&mut svm, market, &market_state);
    put_anchor_account(&mut svm, key_epoch, &key_epoch_state);
    put_system_account(&mut svm, vault_authority);

    svm.set_account(
        mint,
        Account {
            lamports: 1_000_000_000,
            data: confidential_mint_data(),
            owner: spl_token_2022_interface::id(),
            ..Account::default()
        },
    )
    .unwrap();
    svm.set_account(
        vault,
        Account {
            lamports: 1_000_000_000,
            data: confidential_token_account_data(
                mint,
                vault_authority,
                key_epoch_state.encryption_pubkey,
                1,
                0,
            ),
            owner: spl_token_2022_interface::id(),
            ..Account::default()
        },
    )
    .unwrap();

    let new_decryptable_balance = [7; beatex::token2022::AE_CIPHERTEXT_LEN];
    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(vault_authority, false),
            AccountMeta::new_readonly(spl_token_2022_interface::id(), false),
        ],
        data: beatex::instruction::ApplyVaultPendingBalance {
            is_base_vault: true,
            expected_pending_balance_credit_counter: 1,
            new_decryptable_available_balance: new_decryptable_balance,
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(&[instruction], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    let metadata = svm.send_transaction(transaction).unwrap();
    eprintln!(
        "Token-2022 ApplyPendingBalance CPI consumed {} CU",
        metadata.compute_units_consumed
    );

    let vault_account = svm.get_account(&vault).unwrap();
    let state = StateWithExtensions::<TokenAccount>::unpack(&vault_account.data).unwrap();
    let confidential = state
        .get_extension::<ConfidentialTransferAccount>()
        .unwrap();
    assert_eq!(u64::from(confidential.pending_balance_credit_counter), 0);
    assert_eq!(
        u64::from(confidential.expected_pending_balance_credit_counter),
        1
    );
    assert_eq!(
        u64::from(confidential.actual_pending_balance_credit_counter),
        1
    );
    assert_eq!(
        confidential.decryptable_available_balance.0,
        new_decryptable_balance
    );
}

#[test]
#[ignore = "requires BeatEx SBF and TOKEN_2022_SBF_SO pointing to Token-2022 v11 with zk-ops"]
fn vault_pda_transfers_with_preverified_context_accounts() {
    let mut svm = svm_with_program();
    let authority = Keypair::new();
    svm.airdrop(&authority.pubkey(), 10_000_000_000).unwrap();

    let market = Pubkey::new_unique();
    let key_epoch = Pubkey::new_unique();
    let auction = Pubkey::new_unique();
    let order = Pubkey::new_unique();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let base_vault = Pubkey::new_unique();
    let quote_vault = Pubkey::new_unique();
    let base_destination = Pubkey::new_unique();
    let quote_destination = Pubkey::new_unique();
    let (market_state, key_epoch_state, vault_authority) = fixture_market(
        authority.pubkey(),
        market,
        key_epoch,
        base_mint,
        base_vault,
        quote_mint,
        quote_vault,
    );
    let elgamal_pubkey = key_epoch_state.encryption_pubkey;
    put_anchor_account(&mut svm, market, &market_state);
    put_anchor_account(&mut svm, key_epoch, &key_epoch_state);
    put_system_account(&mut svm, vault_authority);

    let auction_state = Auction {
        version: 1,
        market,
        auction_id: 1,
        key_epoch,
        state: AuctionState::Settled,
        open_slot: 0,
        submission_end_slot: 1,
        aggregation_deadline_slot: 2,
        decryption_deadline_slot: 2,
        reference_price: 1,
        reference_from_oracle: false,
        oracle_publish_time: 0,
        tick_size: 1,
        grid_size: 3,
        allocation_rule_version: 1,
        order_circuit_version: 1,
        claim_circuit_version: 1,
        finalized_order_count: 1,
        aggregate_buy: [[0; 64]; beatex::state::MAX_GRID_SIZE],
        aggregate_sell: [[0; 64]; beatex::state::MAX_GRID_SIZE],
        decrypted_demand: [0; beatex::state::MAX_GRID_SIZE],
        decrypted_supply: [0; beatex::state::MAX_GRID_SIZE],
        decrypted_entry_bitmap: 0,
        clearing_index: 1,
        clearing_price: 1,
        clearing_volume: 0,
        base_residual: 0,
        quote_residual: 0,
        bump: 0,
    };
    let order_state = Order {
        version: 1,
        market,
        auction,
        owner: authority.pubkey(),
        funder: authority.pubkey(),
        client_order_nonce: 1,
        state: OrderState::Finalized,
        grid_size: 3,
        written_entry_bitmap: 0,
        encrypted_buy: [[0; 64]; beatex::state::MAX_GRID_SIZE],
        encrypted_sell: [[0; 64]; beatex::state::MAX_GRID_SIZE],
        base_collateral_commitment: [0; 32],
        quote_collateral_commitment: [0; 32],
        order_circuit_version: 1,
        claim_nullifier_commitment: [0; 32],
        aggregated: true,
        bump: 0,
    };
    put_anchor_account(&mut svm, auction, &auction_state);
    put_anchor_account(&mut svm, order, &order_state);

    for mint in [base_mint, quote_mint] {
        put_token_2022_account(&mut svm, mint, confidential_mint_data());
    }
    for (address, mint) in [
        (base_vault, base_mint),
        (quote_vault, quote_mint),
        (base_destination, base_mint),
        (quote_destination, quote_mint),
    ] {
        let owner = if address == base_vault || address == quote_vault {
            vault_authority
        } else {
            authority.pubkey()
        };
        put_token_2022_account(
            &mut svm,
            address,
            confidential_token_account_data(mint, owner, elgamal_pubkey, 0, 0),
        );
    }

    let (equality, validity, range) =
        put_zero_transfer_proof_contexts(&mut svm, elgamal_pubkey, elgamal_pubkey);
    let transfer = ConfidentialTransferArgs {
        new_source_decryptable_available_balance: [5; beatex::token2022::AE_CIPHERTEXT_LEN],
        auditor_ciphertext_lo: [0; 64],
        auditor_ciphertext_hi: [0; 64],
    };
    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(auction, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new(order, false),
            AccountMeta::new_readonly(base_mint, false),
            AccountMeta::new(base_vault, false),
            AccountMeta::new(base_destination, false),
            AccountMeta::new_readonly(equality, false),
            AccountMeta::new_readonly(validity, false),
            AccountMeta::new_readonly(range, false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new(quote_vault, false),
            AccountMeta::new(quote_destination, false),
            AccountMeta::new_readonly(equality, false),
            AccountMeta::new_readonly(validity, false),
            AccountMeta::new_readonly(range, false),
            AccountMeta::new_readonly(vault_authority, false),
            AccountMeta::new_readonly(spl_token_2022_interface::id(), false),
        ],
        data: beatex::instruction::SettleTestOrder {
            is_refund: false,
            base_transfer: transfer,
            quote_transfer: transfer,
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(&[instruction], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    let metadata = svm.send_transaction(transaction).unwrap();
    eprintln!(
        "two Token-2022 confidential Transfer CPIs consumed {} CU",
        metadata.compute_units_consumed
    );

    let order_account = svm.get_account(&order).unwrap();
    let settled_order = Order::try_deserialize(&mut order_account.data.as_slice()).unwrap();
    assert_eq!(settled_order.state, OrderState::Claimed);
    for destination in [base_destination, quote_destination] {
        let destination_account = svm.get_account(&destination).unwrap();
        let state = StateWithExtensions::<TokenAccount>::unpack(&destination_account.data).unwrap();
        let confidential = state
            .get_extension::<ConfidentialTransferAccount>()
            .unwrap();
        assert_eq!(u64::from(confidential.pending_balance_credit_counter), 1);
    }
}

#[test]
#[ignore = "requires BeatEx and official Token-2022 v11 SBF artifacts"]
fn public_deposit_is_proved_and_moved_into_confidential_pending_balance() {
    let mut svm = svm_with_program();
    let owner = Keypair::new();
    svm.airdrop(&owner.pubkey(), 2_000_000_000).unwrap();

    let market = Pubkey::new_unique();
    let key_epoch = Pubkey::new_unique();
    let private_balance = Pubkey::new_unique();
    let deposit_vk = Pubkey::new_unique();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let base_vault = Pubkey::new_unique();
    let quote_vault = Pubkey::new_unique();
    let base_source = Pubkey::new_unique();
    let quote_source = Pubkey::new_unique();
    let fixture =
        beatex_babyjubjub_groth16_spike::deposit::deterministic_solana_fixture_for_context(
            market.to_bytes(),
            owner.pubkey().to_bytes(),
            key_epoch.to_bytes(),
        )
        .unwrap();
    let (market_state, mut key_epoch_state, vault_authority) = fixture_market(
        owner.pubkey(),
        market,
        key_epoch,
        base_mint,
        base_vault,
        quote_mint,
        quote_vault,
    );
    key_epoch_state.babyjub_encryption_pubkey = fixture.committee_key;
    let balance_state = EncryptedPrivateBalance {
        version: 1,
        market,
        owner: owner.pubkey(),
        key_epoch,
        encrypted_base: fixture.old_base,
        encrypted_quote: fixture.old_quote,
        deposited_base: 0,
        deposited_quote: 0,
        balance_digest: [0; 32],
        revision: fixture.old_revision,
        bump: 0,
    };
    let vk_state = QueueVerificationKey {
        version: 1,
        market,
        circuit_kind: QueueCircuitKind::BalanceDeposit,
        circuit_version: 1,
        alpha_g1: fixture.vk_alpha_g1,
        beta_g2: fixture.vk_beta_g2,
        gamma_g2: fixture.vk_gamma_g2,
        delta_g2: fixture.vk_delta_g2,
        ic: fixture.vk_ic,
        bump: 0,
    };
    put_anchor_account(&mut svm, market, &market_state);
    put_anchor_account(&mut svm, key_epoch, &key_epoch_state);
    put_anchor_account(&mut svm, private_balance, &balance_state);
    put_anchor_account(&mut svm, deposit_vk, &vk_state);
    put_system_account(&mut svm, vault_authority);
    for mint in [base_mint, quote_mint] {
        put_token_2022_account(&mut svm, mint, confidential_mint_data());
    }
    for (address, mint, authority, amount) in [
        (
            base_source,
            base_mint,
            owner.pubkey(),
            fixture.base_amount + 10,
        ),
        (
            quote_source,
            quote_mint,
            owner.pubkey(),
            fixture.quote_amount + 10,
        ),
        (base_vault, base_mint, vault_authority, 0),
        (quote_vault, quote_mint, vault_authority, 0),
    ] {
        put_token_2022_account(
            &mut svm,
            address,
            confidential_token_account_data(
                mint,
                authority,
                key_epoch_state.encryption_pubkey,
                0,
                amount,
            ),
        );
    }

    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(owner.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new_readonly(deposit_vk, false),
            AccountMeta::new(private_balance, false),
            AccountMeta::new_readonly(base_mint, false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new(base_source, false),
            AccountMeta::new(quote_source, false),
            AccountMeta::new(base_vault, false),
            AccountMeta::new(quote_vault, false),
            AccountMeta::new_readonly(vault_authority, false),
            AccountMeta::new_readonly(spl_token_2022_interface::id(), false),
        ],
        data: beatex::instruction::DepositCollateral {
            args: DepositCollateralArgs {
                base_amount: fixture.base_amount,
                quote_amount: fixture.quote_amount,
                new_encrypted_base: fixture.new_base,
                new_encrypted_quote: fixture.new_quote,
                proof: Groth16ProofInput {
                    proof_a: fixture.proof_a,
                    proof_b: fixture.proof_b,
                    proof_c: fixture.proof_c,
                    public_input: fixture.public_input,
                },
            },
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&owner],
        Message::new(&[instruction], Some(&owner.pubkey())),
        svm.latest_blockhash(),
    );
    let metadata = svm.send_transaction(transaction).unwrap();
    let updated: EncryptedPrivateBalance = {
        let account = svm.get_account(&private_balance).unwrap();
        EncryptedPrivateBalance::try_deserialize(&mut account.data.as_slice()).unwrap()
    };
    assert_eq!(updated.encrypted_base, fixture.new_base);
    assert_eq!(updated.encrypted_quote, fixture.new_quote);
    assert_eq!(updated.deposited_base, fixture.base_amount);
    assert_eq!(updated.deposited_quote, fixture.quote_amount);
    assert_eq!(updated.revision, fixture.old_revision + 1);

    for (source, vault, amount) in [
        (base_source, base_vault, fixture.base_amount),
        (quote_source, quote_vault, fixture.quote_amount),
    ] {
        let source_account = svm.get_account(&source).unwrap();
        let source_state =
            StateWithExtensions::<TokenAccount>::unpack(&source_account.data).unwrap();
        assert_eq!(source_state.base.amount, 10);
        let vault_account = svm.get_account(&vault).unwrap();
        let vault_state = StateWithExtensions::<TokenAccount>::unpack(&vault_account.data).unwrap();
        assert_eq!(vault_state.base.amount, 0);
        let confidential = vault_state
            .get_extension::<ConfidentialTransferAccount>()
            .unwrap();
        assert_eq!(u64::from(confidential.pending_balance_credit_counter), 1);
        assert!(amount > 0);
    }
    eprintln!(
        "proved public deposit plus two confidential Deposit CPIs consumed {} CU",
        metadata.compute_units_consumed
    );
}

#[test]
#[ignore = "requires BeatEx and official Token-2022 v11 SBF artifacts"]
fn production_private_claim_reaches_groth16_verification_before_any_token_cpi() {
    let mut svm = svm_with_program();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 2_000_000_000).unwrap();

    let market = Pubkey::new_unique();
    let key_epoch = Pubkey::new_unique();
    let auction = Pubkey::new_unique();
    let order = Pubkey::new_unique();
    let claim_vk = Pubkey::new_unique();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let base_vault = Pubkey::new_unique();
    let quote_vault = Pubkey::new_unique();
    let base_destination = Pubkey::new_unique();
    let quote_destination = Pubkey::new_unique();
    let (market_state, key_epoch_state, vault_authority) = fixture_market(
        payer.pubkey(),
        market,
        key_epoch,
        base_mint,
        base_vault,
        quote_mint,
        quote_vault,
    );
    let mut auction_state = Auction {
        version: 1,
        market,
        auction_id: 1,
        key_epoch,
        state: AuctionState::Settled,
        open_slot: 0,
        submission_end_slot: 1,
        aggregation_deadline_slot: 2,
        decryption_deadline_slot: 3,
        reference_price: 1,
        reference_from_oracle: false,
        oracle_publish_time: 0,
        tick_size: 1,
        grid_size: 3,
        allocation_rule_version: 1,
        order_circuit_version: 1,
        claim_circuit_version: 1,
        finalized_order_count: 1,
        aggregate_buy: [[0; 64]; beatex::state::MAX_GRID_SIZE],
        aggregate_sell: [[0; 64]; beatex::state::MAX_GRID_SIZE],
        decrypted_demand: [0; beatex::state::MAX_GRID_SIZE],
        decrypted_supply: [0; beatex::state::MAX_GRID_SIZE],
        decrypted_entry_bitmap: 0,
        clearing_index: 1,
        clearing_price: 1,
        clearing_volume: 0,
        base_residual: 0,
        quote_residual: 0,
        bump: 0,
    };
    auction_state.decrypted_entry_bitmap = 0b111;
    let mut order_commitment = [0u8; 32];
    order_commitment[0] = 1;
    let order_state = QueuedOrder {
        version: 1,
        market,
        auction,
        owner: payer.pubkey(),
        funder: payer.pubkey(),
        client_order_nonce: 7,
        sequence: 0,
        state: QueuedOrderState::Folded,
        withdrawal_state: WithdrawalState::Pending,
        terminal_nullifier: [0; 32],
        ciphertexts: vec![0; beatex::queue::QUEUED_CIPHERTEXT_COUNT * 64],
        ciphertext_digest: [0; 32],
        order_commitment,
        order_circuit_version: 1,
        bump: 0,
    };
    let valid_fixture =
        beatex_babyjubjub_groth16_spike::deposit::deterministic_solana_fixture().unwrap();
    let vk_state = QueueVerificationKey {
        version: 1,
        market,
        circuit_kind: QueueCircuitKind::PrivateClaim,
        circuit_version: beatex::queue::PRIVATE_CLAIM_CIRCUIT_VERSION,
        alpha_g1: valid_fixture.vk_alpha_g1,
        beta_g2: valid_fixture.vk_beta_g2,
        gamma_g2: valid_fixture.vk_gamma_g2,
        delta_g2: valid_fixture.vk_delta_g2,
        ic: valid_fixture.vk_ic,
        bump: 0,
    };
    put_anchor_account(&mut svm, market, &market_state);
    put_anchor_account(&mut svm, key_epoch, &key_epoch_state);
    put_anchor_account(&mut svm, auction, &auction_state);
    put_anchor_account(&mut svm, order, &order_state);
    put_anchor_account(&mut svm, claim_vk, &vk_state);
    put_system_account(&mut svm, vault_authority);
    for mint in [base_mint, quote_mint] {
        put_token_2022_account(&mut svm, mint, confidential_mint_data());
    }
    for (address, mint, owner) in [
        (base_vault, base_mint, vault_authority),
        (quote_vault, quote_mint, vault_authority),
        (base_destination, base_mint, payer.pubkey()),
        (quote_destination, quote_mint, payer.pubkey()),
    ] {
        put_token_2022_account(
            &mut svm,
            address,
            confidential_token_account_data(mint, owner, key_epoch_state.encryption_pubkey, 0, 0),
        );
    }
    let (base_equality, base_validity, base_range) = put_zero_transfer_proof_contexts(
        &mut svm,
        key_epoch_state.encryption_pubkey,
        key_epoch_state.encryption_pubkey,
    );
    let (quote_equality, quote_validity, quote_range) = put_zero_transfer_proof_contexts(
        &mut svm,
        key_epoch_state.encryption_pubkey,
        key_epoch_state.encryption_pubkey,
    );
    let transfer = ConfidentialTransferArgs {
        new_source_decryptable_available_balance: [0; beatex::token2022::AE_CIPHERTEXT_LEN],
        auditor_ciphertext_lo: [0; 64],
        auditor_ciphertext_hi: [0; 64],
    };
    let mut terminal_nullifier = [0u8; 32];
    terminal_nullifier[0] = 2;
    let binding = beatex::token2022::TransferProofBinding {
        source_pubkey: key_epoch_state.encryption_pubkey,
        destination_pubkey: key_epoch_state.encryption_pubkey,
        auditor_pubkey: [0; 32],
        new_source_ciphertext: [0; 64],
        ciphertexts_lo: [[0; 64]; 3],
        ciphertexts_hi: [[0; 64]; 3],
    };
    let mut public_input = beatex::queue_hash::private_withdrawal_statement_digest(
        &beatex::queue_hash::PrivateWithdrawalStatement {
            kind: beatex::queue::WithdrawalKind::Claim,
            market: &market,
            auction: &auction,
            order: &order,
            owner: &payer.pubkey(),
            key_epoch: &key_epoch,
            client_order_nonce: order_state.client_order_nonce,
            sequence: order_state.sequence,
            allocation_rule_version: auction_state.allocation_rule_version,
            order_circuit_version: auction_state.order_circuit_version,
            claim_circuit_version: auction_state.claim_circuit_version,
            order_commitment,
            clearing_index: auction_state.clearing_index,
            clearing_price: auction_state.clearing_price,
            demand_at_clearing: 0,
            supply_at_clearing: 0,
            clearing_volume: 0,
            terminal_nullifier,
            base_mint: &base_mint,
            quote_mint: &quote_mint,
            base_vault: &base_vault,
            quote_vault: &quote_vault,
            base_destination: &base_destination,
            quote_destination: &quote_destination,
            base_transfer: &binding,
            quote_transfer: &binding,
            base_new_decryptable_balance: &transfer.new_source_decryptable_available_balance,
            quote_new_decryptable_balance: &transfer.new_source_decryptable_available_balance,
            base_amount_commitment: [0; 32],
            quote_amount_commitment: [0; 32],
            batched_amount_commitment: [0; 32],
        },
    )
    .unwrap();
    public_input.reverse();
    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(auction, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new(order, false),
            AccountMeta::new_readonly(claim_vk, false),
            AccountMeta::new_readonly(base_mint, false),
            AccountMeta::new(base_vault, false),
            AccountMeta::new(base_destination, false),
            AccountMeta::new_readonly(base_equality, false),
            AccountMeta::new_readonly(base_validity, false),
            AccountMeta::new_readonly(base_range, false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new(quote_vault, false),
            AccountMeta::new(quote_destination, false),
            AccountMeta::new_readonly(quote_equality, false),
            AccountMeta::new_readonly(quote_validity, false),
            AccountMeta::new_readonly(quote_range, false),
            AccountMeta::new_readonly(vault_authority, false),
            AccountMeta::new_readonly(spl_token_2022_interface::id(), false),
        ],
        data: beatex::instruction::ClaimQueuedOrder {
            args: PrivateWithdrawalArgs {
                terminal_nullifier,
                base_transfer: transfer,
                quote_transfer: transfer,
                proof: Groth16ProofInput {
                    proof_a: [0; 64],
                    proof_b: [0; 128],
                    proof_c: [0; 64],
                    public_input,
                },
            },
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&payer],
        Message::new(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                instruction,
            ],
            Some(&payer.pubkey()),
        ),
        svm.latest_blockhash(),
    );
    let failure = svm.send_transaction(transaction).unwrap_err();
    let debug = format!("{failure:?}");
    assert!(debug.contains("InvalidGroth16Proof"), "{debug}");

    let unchanged: QueuedOrder = {
        let account = svm.get_account(&order).unwrap();
        QueuedOrder::try_deserialize(&mut account.data.as_slice()).unwrap()
    };
    assert_eq!(unchanged.withdrawal_state, WithdrawalState::Pending);
    assert_eq!(unchanged.terminal_nullifier, [0; 32]);
}
