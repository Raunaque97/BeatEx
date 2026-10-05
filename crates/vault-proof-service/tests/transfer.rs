use {
    beatex_threshold_crypto::dealer_setup_3_of_7,
    beatex_vault_proof_service::{
        build_withdrawal_context_plan, claim_bridge_witness, derive_private_order_identifiers,
        generate_transfer_proof, generate_withdrawal_proof_bundle,
        prepare_private_withdrawal_witness, ConfidentialDestination, PrivateOrderOpening,
        TransferContextAddresses, TransferRequest, VaultAeKey, VaultElGamalKeypair,
        VaultProofError, VaultSnapshot, WithdrawalContextAddresses, WithdrawalStatementContext,
        MAX_TRANSFER_AMOUNT,
    },
    curve25519_dalek::scalar::Scalar,
    solana_pubkey::Pubkey,
    solana_zk_sdk_v4::encryption::elgamal::ElGamalKeypair,
    solana_zk_sdk_v4::encryption::pedersen::{Pedersen, PedersenOpening},
};

fn fixture_request(
    source_key: &VaultElGamalKeypair,
    source_ae_key: &VaultAeKey,
    current_balance: u64,
    amount: u64,
) -> TransferRequest {
    let destination_keypair = ElGamalKeypair::new_rand();
    let source_pubkey = source_key.pubkey_bytes();

    // Use the reconstructed v4 public key so the fixture exercises the same
    // byte compatibility boundary as production proof generation.
    let source_pubkey_typed =
        solana_zk_sdk_v4::encryption::elgamal::ElGamalPubkey::try_from(source_pubkey.as_slice())
            .unwrap();
    let available_balance = source_pubkey_typed.encrypt_u64(current_balance).to_bytes();
    let decryptable_available_balance = source_ae_key.encrypt_balance(current_balance);

    TransferRequest {
        source: VaultSnapshot {
            token_account: [1; 32],
            mint: [2; 32],
            elgamal_pubkey: source_pubkey,
            available_balance,
            decryptable_available_balance,
            pending_balance_credit_counter: 0,
            expected_pending_balance_credit_counter: 0,
            actual_pending_balance_credit_counter: 0,
        },
        destination: ConfidentialDestination {
            token_account: [3; 32],
            mint: [2; 32],
            owner: [4; 32],
            elgamal_pubkey: destination_keypair.pubkey().into(),
        },
        amount,
        auditor_elgamal_pubkey: None,
    }
}

#[test]
fn reconstructs_threshold_key_and_host_verifies_real_transfer_bundle() {
    let setup = dealer_setup_3_of_7(b"beatex/vault-proof-service/test");
    let source_key = VaultElGamalKeypair::reconstruct_3_of_7(
        &setup.public,
        [
            &setup.secret_shares[0],
            &setup.secret_shares[3],
            &setup.secret_shares[6],
        ],
    )
    .unwrap();
    let ae_key = VaultAeKey::new_rand();
    let request = fixture_request(&source_key, &ae_key, 1_000_000, 123_456);

    let bundle = generate_transfer_proof(&request, &source_key, &ae_key).unwrap();
    bundle.verify_host().unwrap();
    assert_ne!(bundle.commitments().unwrap().amount_lo, [0; 32]);
    assert_ne!(bundle.commitments().unwrap().amount_hi, [0; 32]);
    assert_ne!(bundle.new_source_decryptable_available_balance, [0; 36]);
}

#[test]
fn rejects_wrong_ae_key_and_oversized_or_insolvent_transfers() {
    let setup = dealer_setup_3_of_7(b"beatex/vault-proof-service/rejection-test");
    let source_key = VaultElGamalKeypair::reconstruct_3_of_7(
        &setup.public,
        [
            &setup.secret_shares[1],
            &setup.secret_shares[2],
            &setup.secret_shares[5],
        ],
    )
    .unwrap();
    let ae_key = VaultAeKey::new_rand();
    let wrong_ae_key = VaultAeKey::new_rand();

    let request = fixture_request(&source_key, &ae_key, 100, 10);
    assert_eq!(
        generate_transfer_proof(&request, &source_key, &wrong_ae_key).unwrap_err(),
        VaultProofError::InvalidDecryptableBalance
    );

    let insolvent = fixture_request(&source_key, &ae_key, 100, 101);
    assert_eq!(
        generate_transfer_proof(&insolvent, &source_key, &ae_key).unwrap_err(),
        VaultProofError::InsufficientFunds
    );

    let oversized = fixture_request(&source_key, &ae_key, MAX_TRANSFER_AMOUNT, 0);
    let mut oversized = oversized;
    oversized.amount = MAX_TRANSFER_AMOUNT + 1;
    assert_eq!(
        generate_transfer_proof(&oversized, &source_key, &ae_key).unwrap_err(),
        VaultProofError::TransferAmountTooLarge
    );
}

#[test]
fn retained_openings_match_the_programs_batched_commitment() {
    let setup = dealer_setup_3_of_7(b"beatex/vault-proof-service/bridge-test");
    let source_key = VaultElGamalKeypair::reconstruct_3_of_7(
        &setup.public,
        [
            &setup.secret_shares[0],
            &setup.secret_shares[2],
            &setup.secret_shares[4],
        ],
    )
    .unwrap();
    let ae_key = VaultAeKey::new_rand();
    let base_amount = 321u64;
    let quote_amount = 45_678u64;
    let base = generate_transfer_proof(
        &fixture_request(&source_key, &ae_key, 1_000_000, base_amount),
        &source_key,
        &ae_key,
    )
    .unwrap();
    let quote = generate_transfer_proof(
        &fixture_request(&source_key, &ae_key, 1_000_000, quote_amount),
        &source_key,
        &ae_key,
    )
    .unwrap();
    let bridge = claim_bridge_witness(&base, &quote).unwrap();
    assert_eq!(
        bridge.batched_amount_commitment,
        beatex::token2022::batch_transfer_amount_commitments(
            bridge.base_amount_commitment,
            bridge.quote_amount_commitment,
        )
        .unwrap()
    );
    let rho_bytes = beatex::queue_hash::ristretto_batching_scalar(
        bridge.base_amount_commitment,
        bridge.quote_amount_commitment,
    )
    .unwrap();
    let rho = Scalar::from_canonical_bytes(rho_bytes).unwrap();
    let amount = Scalar::from(base_amount) + rho * Scalar::from(quote_amount);
    let opening = PedersenOpening::from_bytes(&bridge.batched_opening).unwrap();
    assert_eq!(
        Pedersen::with(amount, &opening).to_bytes(),
        bridge.batched_amount_commitment
    );
}

#[test]
fn coordinator_generates_both_transfer_proofs_and_bridge_atomically() {
    let setup = dealer_setup_3_of_7(b"beatex/vault-proof-service/coordinator-test");
    let source_key = VaultElGamalKeypair::reconstruct_3_of_7(
        &setup.public,
        [
            &setup.secret_shares[1],
            &setup.secret_shares[3],
            &setup.secret_shares[6],
        ],
    )
    .unwrap();
    let base_ae = VaultAeKey::new_rand();
    let quote_ae = VaultAeKey::new_rand();
    let bundle = generate_withdrawal_proof_bundle(
        &fixture_request(&source_key, &base_ae, 10_000, 25),
        &fixture_request(&source_key, &quote_ae, 1_000_000, 2_500),
        &source_key,
        &source_key,
        &base_ae,
        &quote_ae,
    )
    .unwrap();
    bundle.base.verify_host().unwrap();
    bundle.quote.verify_host().unwrap();
    let (base_args, quote_args) = bundle.transfer_args();
    assert_eq!(
        base_args.new_source_decryptable_available_balance,
        bundle.base.new_source_decryptable_available_balance
    );
    assert_eq!(
        quote_args.new_source_decryptable_available_balance,
        bundle.quote.new_source_decryptable_available_balance
    );
    assert_eq!(
        bundle
            .base
            .proof_binding()
            .unwrap()
            .combined_amount_commitment()
            .unwrap(),
        bundle.bridge.base_amount_commitment
    );
    assert_eq!(
        bundle
            .quote
            .proof_binding()
            .unwrap()
            .combined_amount_commitment()
            .unwrap(),
        bundle.bridge.quote_amount_commitment
    );
    assert_eq!(
        bundle.bridge.batched_amount_commitment,
        beatex::token2022::batch_transfer_amount_commitments(
            bundle.bridge.base_amount_commitment,
            bundle.bridge.quote_amount_commitment,
        )
        .unwrap()
    );
}

#[test]
fn coordinator_builds_six_exact_proof_context_transactions() {
    let setup = dealer_setup_3_of_7(b"beatex/vault-proof-service/context-plan-test");
    let source_key = VaultElGamalKeypair::reconstruct_3_of_7(
        &setup.public,
        [
            &setup.secret_shares[0],
            &setup.secret_shares[1],
            &setup.secret_shares[5],
        ],
    )
    .unwrap();
    let base_ae = VaultAeKey::new_rand();
    let quote_ae = VaultAeKey::new_rand();
    let bundle = generate_withdrawal_proof_bundle(
        &fixture_request(&source_key, &base_ae, 10_000, 25),
        &fixture_request(&source_key, &quote_ae, 1_000_000, 2_500),
        &source_key,
        &source_key,
        &base_ae,
        &quote_ae,
    )
    .unwrap();
    let addresses = WithdrawalContextAddresses {
        base: TransferContextAddresses {
            equality: Pubkey::new_unique(),
            validity: Pubkey::new_unique(),
            range: Pubkey::new_unique(),
        },
        quote: TransferContextAddresses {
            equality: Pubkey::new_unique(),
            validity: Pubkey::new_unique(),
            range: Pubkey::new_unique(),
        },
    };
    let authority = Pubkey::new_unique();
    let close_destination = Pubkey::new_unique();
    let payer = Pubkey::new_unique();
    let rent_lamports = [1_000_001, 1_000_002, 1_000_003];
    let plan = build_withdrawal_context_plan(
        &bundle,
        addresses,
        authority,
        close_destination,
        payer,
        rent_lamports,
    );

    let all_accounts = plan
        .base
        .accounts
        .iter()
        .chain(plan.quote.accounts.iter())
        .collect::<Vec<_>>();
    assert_eq!(all_accounts.len(), 6);
    assert!(all_accounts.iter().all(|account| {
        account.owner == solana_zk_sdk_v4::zk_elgamal_proof_program::id() && account.space > 32
    }));
    for (transfer, expected) in [(&plan.base, addresses.base), (&plan.quote, addresses.quote)] {
        assert_eq!(transfer.accounts[0].address, expected.equality);
        assert_eq!(transfer.accounts[1].address, expected.validity);
        assert_eq!(transfer.accounts[2].address, expected.range);
        for (index, instruction) in transfer.create_instructions.iter().enumerate() {
            assert_eq!(instruction.accounts[0].pubkey, payer);
            assert!(instruction.accounts[0].is_signer);
            assert_eq!(
                instruction.accounts[1].pubkey,
                transfer.accounts[index].address
            );
            assert!(instruction.accounts[1].is_signer);
        }
        for (instruction, account) in transfer
            .verify_instructions
            .iter()
            .zip(transfer.accounts.iter())
        {
            assert_eq!(instruction.program_id, account.owner);
            assert_eq!(instruction.accounts[0].pubkey, account.address);
            assert_eq!(instruction.accounts[1].pubkey, authority);
            assert!(instruction.data.len() < 1_100);
        }
        for instruction in &transfer.close_instructions {
            assert_eq!(instruction.accounts[1].pubkey, close_destination);
            assert_eq!(instruction.accounts[2].pubkey, authority);
            assert!(instruction.accounts[2].is_signer);
        }
    }
}

#[test]
fn coordinator_builds_the_exact_program_bound_claim_witness() {
    let setup = dealer_setup_3_of_7(b"beatex/vault-proof-service/claim-witness-test");
    let source_key = VaultElGamalKeypair::reconstruct_3_of_7(
        &setup.public,
        [
            &setup.secret_shares[0],
            &setup.secret_shares[3],
            &setup.secret_shares[6],
        ],
    )
    .unwrap();
    let base_ae = VaultAeKey::new_rand();
    let quote_ae = VaultAeKey::new_rand();
    let mut base_request = fixture_request(&source_key, &base_ae, 10_000, 1);
    base_request.source.token_account = [10; 32];
    base_request.source.mint = [11; 32];
    base_request.destination.token_account = [12; 32];
    base_request.destination.mint = [11; 32];
    let mut quote_request = fixture_request(&source_key, &quote_ae, 1_000_000, 120);
    quote_request.source.token_account = [20; 32];
    quote_request.source.mint = [21; 32];
    quote_request.destination.token_account = [22; 32];
    quote_request.destination.mint = [21; 32];
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
            let mut salt = [0u8; 32];
            salt[..8].copy_from_slice(&0x9137u64.to_le_bytes());
            salt
        },
    };
    let auction = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let nonce = 7;
    let (order_commitment, terminal_nullifier) =
        derive_private_order_identifiers(&opening, &auction, &owner, nonce);
    let context = WithdrawalStatementContext {
        kind: beatex::queue::WithdrawalKind::Claim,
        market: Pubkey::new_unique(),
        auction,
        order: Pubkey::new_unique(),
        owner,
        key_epoch: Pubkey::new_unique(),
        client_order_nonce: nonce,
        sequence: 3,
        allocation_rule_version: 1,
        order_circuit_version: 1,
        claim_circuit_version: 1,
        order_commitment,
        clearing_index: (beatex::queue::QUEUE_PRICE_LEVELS / 2) as u8,
        clearing_price: 100,
        demand_at_clearing: 5,
        supply_at_clearing: 3,
        clearing_volume: 3,
        terminal_nullifier,
        base_mint: Pubkey::new_from_array(base_request.source.mint),
        quote_mint: Pubkey::new_from_array(quote_request.source.mint),
        base_vault: Pubkey::new_from_array(base_request.source.token_account),
        quote_vault: Pubkey::new_from_array(quote_request.source.token_account),
        base_destination: Pubkey::new_from_array(base_request.destination.token_account),
        quote_destination: Pubkey::new_from_array(quote_request.destination.token_account),
    };
    let witness = prepare_private_withdrawal_witness(&opening, &context, &bundle).unwrap();
    assert_eq!(witness.bridge.base_amount, 1);
    assert_eq!(witness.bridge.quote_amount, 120);

    let mut wrong_account = context.clone();
    wrong_account.base_destination = Pubkey::new_unique();
    assert_eq!(
        prepare_private_withdrawal_witness(&opening, &wrong_account, &bundle).unwrap_err(),
        VaultProofError::StatementAccountMismatch
    );
    let mut wrong_commitment = context;
    wrong_commitment.order_commitment[0] ^= 1;
    assert_eq!(
        prepare_private_withdrawal_witness(&opening, &wrong_commitment, &bundle).unwrap_err(),
        VaultProofError::OrderCommitmentMismatch
    );
}
