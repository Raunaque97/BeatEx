use {
    anchor_lang::{
        prelude::Pubkey, solana_program::program_option::COption, AccountDeserialize,
        AccountSerialize, InstructionData,
    },
    ark_groth16::Groth16,
    ark_snark::SNARK,
    beatex::{
        ActiveOrder, Auction, AuctionState, DleqProofInput, KeyEpoch, Market,
        PrivateWithdrawalArgs, QueueCircuitKind, QueueVerificationKey, QueuedOrder,
        QueuedOrderState, WithdrawalState,
    },
    beatex_babyjubjub_groth16_spike::claim::{
        export_private_withdrawal_verifying_key, PrivateWithdrawalCircuit, PrivateWithdrawalKind,
    },
    beatex_threshold_crypto::dealer_setup_3_of_7,
    beatex_vault_proof_service::{
        derive_private_order_identifiers, generate_private_withdrawal_groth16_proof,
        generate_withdrawal_proof_bundle, prepare_private_withdrawal_witness,
        ConfidentialDestination, PrivateOrderOpening, TransferRequest, VaultAeKey,
        VaultElGamalKeypair, VaultSnapshot, WithdrawalStatementContext,
    },
    litesvm::LiteSVM,
    rand::SeedableRng,
    rand_chacha::ChaCha20Rng,
    solana_account::Account,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::Message,
    solana_signer::Signer,
    solana_transaction::Transaction,
    solana_zk_elgamal_proof_interface::{proof_data::ProofType, state::ProofContextState},
    solana_zk_sdk::encryption::elgamal::{ElGamalKeypair, ElGamalPubkey},
    solana_zk_sdk_pod::encryption::{
        auth_encryption::PodAeCiphertext,
        elgamal::{PodElGamalCiphertext, PodElGamalPubkey},
    },
    spl_token_2022_interface::{
        extension::{
            confidential_transfer::{ConfidentialTransferAccount, ConfidentialTransferMint},
            BaseStateWithExtensions, BaseStateWithExtensionsMut, ExtensionType,
            StateWithExtensions, StateWithExtensionsMut,
        },
        state::{Account as TokenAccount, AccountState, Mint},
    },
    std::path::PathBuf,
};

fn svm() -> LiteSVM {
    let program = PathBuf::from(std::env::var_os("BEATEX_SBF_SO").unwrap());
    let token = PathBuf::from(std::env::var_os("TOKEN_2022_SBF_SO").unwrap());
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(beatex::ID, program).unwrap();
    svm.add_program_from_file(spl_token_2022_interface::id(), token)
        .unwrap();
    svm
}

fn put_anchor<T: AccountSerialize>(svm: &mut LiteSVM, key: Pubkey, value: &T) {
    let mut data = Vec::new();
    value.try_serialize(&mut data).unwrap();
    svm.set_account(
        key,
        Account {
            lamports: 1_000_000_000,
            data,
            owner: beatex::ID,
            ..Account::default()
        },
    )
    .unwrap();
}

fn put_account(svm: &mut LiteSVM, key: Pubkey, owner: Pubkey, data: Vec<u8>) {
    svm.set_account(
        key,
        Account {
            lamports: 1_000_000_000,
            data,
            owner,
            ..Account::default()
        },
    )
    .unwrap();
}

fn mint_data() -> Vec<u8> {
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

fn token_data(
    mint: Pubkey,
    owner: Pubkey,
    elgamal_pubkey: [u8; 32],
    available: [u8; 64],
    decryptable: [u8; 36],
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
    confidential.elgamal_pubkey = PodElGamalPubkey(elgamal_pubkey);
    confidential.available_balance = PodElGamalCiphertext(available);
    confidential.decryptable_available_balance = PodAeCiphertext(decryptable);
    confidential.allow_confidential_credits = true.into();
    confidential.allow_non_confidential_credits = true.into();
    confidential.maximum_pending_balance_credit_counter = 65_536_u64.into();
    state.init_account_type().unwrap();
    data
}

#[allow(clippy::too_many_arguments)]
fn transfer_request(
    source_key: &VaultElGamalKeypair,
    ae_key: &VaultAeKey,
    vault: Pubkey,
    mint: Pubkey,
    destination: Pubkey,
    destination_key: &ElGamalKeypair,
    balance: u64,
    amount: u64,
) -> TransferRequest {
    let public_key = ElGamalPubkey::try_from(source_key.pubkey_bytes().as_slice()).unwrap();
    TransferRequest {
        source: VaultSnapshot {
            token_account: vault.to_bytes(),
            mint: mint.to_bytes(),
            elgamal_pubkey: source_key.pubkey_bytes(),
            available_balance: public_key.encrypt_u64(balance).to_bytes(),
            decryptable_available_balance: ae_key.encrypt_balance(balance),
            pending_balance_credit_counter: 0,
            expected_pending_balance_credit_counter: 0,
            actual_pending_balance_credit_counter: 0,
        },
        destination: ConfidentialDestination {
            token_account: destination.to_bytes(),
            mint: mint.to_bytes(),
            owner: [9; 32],
            elgamal_pubkey: destination_key.pubkey().into(),
        },
        amount,
        auditor_elgamal_pubkey: None,
    }
}

fn put_contexts(
    svm: &mut LiteSVM,
    bundle: &beatex_vault_proof_service::TransferProofBundle,
) -> [Pubkey; 3] {
    let authority = Pubkey::new_unique();
    let addresses = [
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    ];
    let data = [
        ProofContextState::encode(
            &authority,
            ProofType::CiphertextCommitmentEquality,
            &bundle.equality_proof.context,
        ),
        ProofContextState::encode(
            &authority,
            ProofType::BatchedGroupedCiphertext3HandlesValidity,
            &bundle.ciphertext_validity_proof.context,
        ),
        ProofContextState::encode(
            &authority,
            ProofType::BatchedRangeProofU128,
            &bundle.range_proof.context,
        ),
    ];
    for (address, bytes) in addresses.into_iter().zip(data) {
        put_account(svm, address, solana_zk_elgamal_proof_interface::id(), bytes);
    }
    addresses
}

#[test]
#[ignore = "generates and executes the multi-million-constraint production claim"]
fn exact_private_claim_executes_atomically_in_sbf() {
    let mut svm = svm();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
    let market = Pubkey::new_unique();
    let auction = Pubkey::new_unique();
    let order = Pubkey::new_unique();
    let active_order = Pubkey::new_unique();
    let key_epoch = Pubkey::new_unique();
    let claim_vk = Pubkey::new_unique();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let base_vault = Pubkey::new_unique();
    let quote_vault = Pubkey::new_unique();
    let base_destination = Pubkey::new_unique();
    let quote_destination = Pubkey::new_unique();
    let (vault_authority, vault_authority_bump) =
        Pubkey::find_program_address(&[b"vault-authority", market.as_ref()], &beatex::ID);

    let threshold = dealer_setup_3_of_7(b"beatex/full-withdrawal-sbf");
    let source_key = VaultElGamalKeypair::reconstruct_3_of_7(
        &threshold.public,
        [
            &threshold.secret_shares[0],
            &threshold.secret_shares[3],
            &threshold.secret_shares[6],
        ],
    )
    .unwrap();
    let base_ae = VaultAeKey::new_rand();
    let quote_ae = VaultAeKey::new_rand();
    let base_destination_key = ElGamalKeypair::new_rand();
    let quote_destination_key = ElGamalKeypair::new_rand();
    let base_request = transfer_request(
        &source_key,
        &base_ae,
        base_vault,
        base_mint,
        base_destination,
        &base_destination_key,
        10_000,
        1,
    );
    let quote_request = transfer_request(
        &source_key,
        &quote_ae,
        quote_vault,
        quote_mint,
        quote_destination,
        &quote_destination_key,
        1_000_000,
        120,
    );
    let bundle = generate_withdrawal_proof_bundle(
        &base_request,
        &quote_request,
        &source_key,
        &source_key,
        &base_ae,
        &quote_ae,
    )
    .unwrap();
    let mut buy = [0u64; beatex::queue::QUEUE_PRICE_LEVELS];
    buy[beatex::queue::QUEUE_PRICE_LEVELS / 2] = 3;
    let opening = PrivateOrderOpening {
        buy,
        sell: [0; beatex::queue::QUEUE_PRICE_LEVELS],
        base_reserve: 0,
        quote_reserve: 300,
        order_salt: {
            let mut value = [0u8; 32];
            value[..8].copy_from_slice(&0x9137u64.to_le_bytes());
            value
        },
    };
    let nonce = 7;
    let (order_commitment, terminal_nullifier) =
        derive_private_order_identifiers(&opening, &auction, &payer.pubkey(), nonce);
    let context = WithdrawalStatementContext {
        kind: beatex::queue::WithdrawalKind::Claim,
        market,
        auction,
        order,
        owner: payer.pubkey(),
        key_epoch,
        client_order_nonce: nonce,
        sequence: 0,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        order_commitment,
        clearing_index: (beatex::queue::QUEUE_PRICE_LEVELS / 2) as u8,
        clearing_price: 100,
        demand_at_clearing: 5,
        supply_at_clearing: 3,
        clearing_volume: 3,
        terminal_nullifier,
        base_mint,
        quote_mint,
        base_vault,
        quote_vault,
        base_destination,
        quote_destination,
    };
    let witness = prepare_private_withdrawal_witness(&opening, &context, &bundle).unwrap();
    let mut setup_rng = ChaCha20Rng::from_seed([0xc1; 32]);
    let (proving_key, verifying_key) = Groth16::<ark_bn254::Bn254>::circuit_specific_setup(
        PrivateWithdrawalCircuit::blank(PrivateWithdrawalKind::Claim),
        &mut setup_rng,
    )
    .unwrap();
    let mut proof_rng = ChaCha20Rng::from_seed([0xd1; 32]);
    let proof =
        generate_private_withdrawal_groth16_proof(&proving_key, witness, &mut proof_rng).unwrap();
    let vk = export_private_withdrawal_verifying_key(&verifying_key).unwrap();

    let market_state = Market {
        version: beatex::state::PROTOCOL_VERSION,
        authority: payer.pubkey(),
        base_mint,
        quote_mint,
        base_vault,
        quote_vault,
        base_unit_size: beatex::state::BASE_UNIT_SIZE,
        quote_unit_size: beatex::state::QUOTE_UNIT_SIZE,
        grid_step_bps: beatex::state::DEFAULT_GRID_STEP_BPS,
        grid_radius: 8,
        max_order_quantity: 1_000_000,
        max_aggregate_quantity: 10_000,
        current_key_epoch: key_epoch,
        market_start_slot: 0,
        last_clearing_price: 100,
        last_clearing_volume: 3,
        last_cleared_auction_sequence: 0,
        minimum_reference_volume: 1,
        oracle_price_account: Pubkey::new_unique(),
        oracle_feed_id: [1; 32],
        oracle_max_age_seconds: 90,
        oracle_max_confidence_bps: 100,
        oracle_exponent_adjustment: 0,
        min_reference_price: 10,
        max_reference_price: 1_000,
        submission_duration_slots: 10,
        aggregation_duration_slots: 10,
        decryption_duration_slots: 10,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        paused: false,
        vault_authority_bump,
        bump: 0,
    };
    let key_epoch_state = KeyEpoch {
        authority: payer.pubkey(),
        market,
        epoch: 1,
        encryption_pubkey: source_key.pubkey_bytes(),
        babyjub_encryption_pubkey: {
            let mut value = [0u8; 32];
            value[0] = 2;
            value
        },
        babyjub_participant_public_shares: [[0; 32]; beatex::COMMITTEE_SIZE],
        coefficient_commitments: [[0; 32]; 3],
        participant_public_shares: [[0; 32]; beatex::COMMITTEE_SIZE],
        participant_authorities: [Pubkey::default(); beatex::COMMITTEE_SIZE],
        key_relation_proof: DleqProofInput {
            commitment_1: [0; 32],
            commitment_2: [0; 32],
            response: [0; 32],
        },
        threshold: 3,
        proof_transcript_version: 2,
        activation_slot: 1,
        retired: false,
        acknowledgement_bitmap: 0x7f,
        bump: 0,
    };
    let mut demand = [0u64; beatex::state::MAX_GRID_SIZE];
    let mut supply = [0u64; beatex::state::MAX_GRID_SIZE];
    demand[8] = 5;
    supply[8] = 3;
    let auction_state = Auction {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction_id: 1,
        key_epoch,
        state: AuctionState::Settled,
        open_slot: 0,
        submission_end_slot: 1,
        aggregation_deadline_slot: 2,
        decryption_deadline_slot: 3,
        reference_price: 100,
        reference_from_oracle: false,
        oracle_publish_time: 0,
        grid_step_bps: beatex::state::DEFAULT_GRID_STEP_BPS,
        grid_size: 17,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        finalized_order_count: 1,
        aggregate_buy: [[0; 64]; beatex::state::MAX_GRID_SIZE],
        aggregate_sell: [[0; 64]; beatex::state::MAX_GRID_SIZE],
        decrypted_demand: demand,
        decrypted_supply: supply,
        decrypted_entry_bitmap: (1u128 << 17) - 1,
        clearing_index: 8,
        clearing_price: 100,
        clearing_volume: 3,
        base_residual: 0,
        quote_residual: 0,
        bump: 0,
    };
    let order_state = QueuedOrder {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction,
        owner: payer.pubkey(),
        funder: payer.pubkey(),
        client_order_nonce: nonce,
        sequence: 0,
        state: QueuedOrderState::Folded,
        withdrawal_state: WithdrawalState::Pending,
        terminal_nullifier: [0; 32],
        previous_event: Pubkey::default(),
        ciphertexts: vec![0; beatex::queue::QUEUED_CIPHERTEXT_COUNT * 64],
        ciphertext_digest: [0; 32],
        order_commitment,
        order_circuit_version: 2,
        bump: 0,
    };
    let active_order_state = ActiveOrder {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction,
        owner: payer.pubkey(),
        latest_event: order,
        revision: 0,
        active: true,
        bump: 0,
    };
    let vk_state = QueueVerificationKey {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        circuit_kind: QueueCircuitKind::PrivateClaim,
        circuit_version: 2,
        alpha_g1: vk.alpha_g1,
        beta_g2: vk.beta_g2,
        gamma_g2: vk.gamma_g2,
        delta_g2: vk.delta_g2,
        ic: vk.ic,
        bump: 0,
    };
    put_anchor(&mut svm, market, &market_state);
    put_anchor(&mut svm, key_epoch, &key_epoch_state);
    put_anchor(&mut svm, auction, &auction_state);
    put_anchor(&mut svm, order, &order_state);
    put_anchor(&mut svm, active_order, &active_order_state);
    put_anchor(&mut svm, claim_vk, &vk_state);
    put_account(
        &mut svm,
        vault_authority,
        anchor_lang::system_program::ID,
        Vec::new(),
    );
    for mint in [base_mint, quote_mint] {
        put_account(&mut svm, mint, spl_token_2022_interface::id(), mint_data());
    }
    put_account(
        &mut svm,
        base_vault,
        spl_token_2022_interface::id(),
        token_data(
            base_mint,
            vault_authority,
            source_key.pubkey_bytes(),
            base_request.source.available_balance,
            base_request.source.decryptable_available_balance,
        ),
    );
    put_account(
        &mut svm,
        quote_vault,
        spl_token_2022_interface::id(),
        token_data(
            quote_mint,
            vault_authority,
            source_key.pubkey_bytes(),
            quote_request.source.available_balance,
            quote_request.source.decryptable_available_balance,
        ),
    );
    put_account(
        &mut svm,
        base_destination,
        spl_token_2022_interface::id(),
        token_data(
            base_mint,
            payer.pubkey(),
            base_destination_key.pubkey().into(),
            [0; 64],
            [0; 36],
        ),
    );
    put_account(
        &mut svm,
        quote_destination,
        spl_token_2022_interface::id(),
        token_data(
            quote_mint,
            payer.pubkey(),
            quote_destination_key.pubkey().into(),
            [0; 64],
            [0; 36],
        ),
    );
    let base_contexts = put_contexts(&mut svm, &bundle.base);
    let quote_contexts = put_contexts(&mut svm, &bundle.quote);
    let (base_transfer, quote_transfer) = bundle.transfer_args();
    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(auction, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new(order, false),
            AccountMeta::new(active_order, false),
            AccountMeta::new_readonly(claim_vk, false),
            AccountMeta::new_readonly(base_mint, false),
            AccountMeta::new(base_vault, false),
            AccountMeta::new(base_destination, false),
            AccountMeta::new_readonly(base_contexts[0], false),
            AccountMeta::new_readonly(base_contexts[1], false),
            AccountMeta::new_readonly(base_contexts[2], false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new(quote_vault, false),
            AccountMeta::new(quote_destination, false),
            AccountMeta::new_readonly(quote_contexts[0], false),
            AccountMeta::new_readonly(quote_contexts[1], false),
            AccountMeta::new_readonly(quote_contexts[2], false),
            AccountMeta::new_readonly(vault_authority, false),
            AccountMeta::new_readonly(spl_token_2022_interface::id(), false),
        ],
        data: beatex::instruction::ClaimQueuedOrder {
            args: PrivateWithdrawalArgs {
                terminal_nullifier,
                base_transfer,
                quote_transfer,
                proof,
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
    let metadata = svm.send_transaction(transaction).unwrap();
    eprintln!(
        "exact production private claim consumed {} CU",
        metadata.compute_units_consumed
    );
    let order_account = svm.get_account(&order).unwrap();
    let claimed = QueuedOrder::try_deserialize(&mut order_account.data.as_slice()).unwrap();
    assert_eq!(claimed.withdrawal_state, WithdrawalState::Claimed);
    assert_eq!(claimed.terminal_nullifier, terminal_nullifier);
    for destination in [base_destination, quote_destination] {
        let account = svm.get_account(&destination).unwrap();
        let state = StateWithExtensions::<TokenAccount>::unpack(&account.data).unwrap();
        let confidential = state
            .get_extension::<ConfidentialTransferAccount>()
            .unwrap();
        assert_eq!(u64::from(confidential.pending_balance_credit_counter), 1);
    }
}
