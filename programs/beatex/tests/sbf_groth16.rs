#![cfg(feature = "sbf-test-fixtures")]

use {
    anchor_lang::{
        prelude::{Clock, Pubkey},
        AccountDeserialize, AccountSerialize, InstructionData, Space,
    },
    beatex::{
        groth16, queue_hash, AggregateAccumulator, Auction, AuctionState, DecryptionShareInput,
        DleqProofInput, EncryptedOrderEntry, Groth16ProofInput, InitializeKeyEpochArgs,
        InitializeMarketArgs, InitializeOrderArgs, KeyEpoch, Market, Order, OrderState,
        PublishAggregateCurvesArgs, QueueCircuitKind, QueueVerificationKey, SubmissionCounter,
        VerifyThresholdDecryptionArgs, COMMITTEE_SIZE,
    },
    beatex_threshold_crypto::{
        create_decryption_share, dealer_setup_3_of_7, DealerSetup, DecryptionProofContext,
        KeyRelationProofContext,
    },
    litesvm::LiteSVM,
    solana_account::Account,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::Message,
    solana_signer::Signer,
    solana_transaction::Transaction,
    std::path::PathBuf,
};

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
    let program_path = program_path();
    assert!(
        program_path.exists(),
        "build the test ELF before running this ignored test"
    );
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(beatex::ID, program_path).unwrap();
    svm
}

fn execute_pipeline_proof(proof: Groth16ProofInput) -> u64 {
    let mut svm = svm_with_program();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![],
        data: beatex::instruction::VerifyGroth16Pipeline {
            circuit_version: groth16::PIPELINE_CIRCUIT_VERSION,
            proof,
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&payer],
        Message::new(&[instruction], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction)
        .unwrap()
        .compute_units_consumed
}

fn dleq_input(proof: &beatex_threshold_crypto::DleqProof) -> DleqProofInput {
    DleqProofInput {
        commitment_1: proof.commitment_1_bytes(),
        commitment_2: proof.commitment_2_bytes(),
        response: proof.response_bytes(),
    }
}

fn put_system_account(svm: &mut LiteSVM, address: Pubkey) {
    svm.set_account(
        address,
        Account {
            lamports: 1_000_000,
            data: vec![],
            owner: anchor_lang::system_program::ID,
            ..Account::default()
        },
    )
    .unwrap();
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

fn deserialize_account<T: AccountDeserialize>(svm: &LiteSVM, address: &Pubkey) -> T {
    let account = svm.get_account(address).unwrap();
    T::try_deserialize(&mut account.data.as_slice()).unwrap()
}

fn submit_test_encrypted_order(
    svm: &mut LiteSVM,
    authority: &Keypair,
    owner: &Keypair,
    market: Pubkey,
    auction: Pubkey,
    nonce: u64,
    entries: Vec<EncryptedOrderEntry>,
) -> Pubkey {
    svm.airdrop(&owner.pubkey(), 1_000_000_000).unwrap();
    let (order, _) = Pubkey::find_program_address(
        &[
            b"order",
            auction.as_ref(),
            owner.pubkey().as_ref(),
            &nonce.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let initialize = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(owner.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(auction, false),
            AccountMeta::new(order, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data: beatex::instruction::InitializeOrder {
            args: InitializeOrderArgs {
                client_order_nonce: nonce,
                base_collateral_commitment: [nonce as u8; 32],
                quote_collateral_commitment: [nonce as u8 + 1; 32],
                claim_nullifier_commitment: [nonce as u8 + 2; 32],
            },
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[owner],
        Message::new(&[initialize], Some(&owner.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();

    for chunk in entries.chunks(4) {
        let write = Instruction {
            program_id: beatex::ID,
            accounts: vec![
                AccountMeta::new_readonly(owner.pubkey(), true),
                AccountMeta::new_readonly(market, false),
                AccountMeta::new_readonly(auction, false),
                AccountMeta::new(order, false),
            ],
            data: beatex::instruction::WriteOrderChunk {
                entries: chunk.to_vec(),
            }
            .data(),
        };
        let transaction = Transaction::new(
            &[owner],
            Message::new(&[write], Some(&owner.pubkey())),
            svm.latest_blockhash(),
        );
        svm.send_transaction(transaction).unwrap();
    }

    let finalize = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(auction, false),
            AccountMeta::new(order, false),
        ],
        data: beatex::instruction::FinalizeTestOrder {}.data(),
    };
    let transaction = Transaction::new(
        &[authority],
        Message::new(&[finalize], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();
    let finalized: Order = deserialize_account(svm, &order);
    assert_eq!(finalized.state, OrderState::Finalized);
    order
}

#[allow(clippy::too_many_arguments)]
fn threshold_args(
    setup: &DealerSetup,
    market: Pubkey,
    key_epoch_address: Pubkey,
    epoch: u64,
    auction_id: u64,
    side: u8,
    position: u16,
    ciphertext_bytes: [u8; 64],
    claimed_amount: u64,
) -> VerifyThresholdDecryptionArgs {
    let ciphertext =
        solana_zk_sdk::encryption::elgamal::ElGamalCiphertext::from_bytes(&ciphertext_bytes)
            .unwrap();
    let commitment = ciphertext_bytes[..32].try_into().unwrap();
    let shares = [0_usize, 2, 6].map(|selected_position| {
        let participant_index = setup.secret_shares[selected_position].index() as u8;
        let context = DecryptionProofContext {
            program_id: beatex::ID.to_bytes(),
            market: market.to_bytes(),
            key_epoch_address: key_epoch_address.to_bytes(),
            epoch,
            auction_id,
            side,
            position,
            participant_index,
            commitment,
        };
        let share = create_decryption_share(
            &setup.secret_shares[selected_position],
            &ciphertext,
            &context.to_bytes(),
        );
        DecryptionShareInput {
            participant_index,
            point: share.point_bytes(),
            proof: dleq_input(&share.proof),
        }
    });
    VerifyThresholdDecryptionArgs {
        auction_id,
        side,
        position,
        ciphertext: ciphertext_bytes,
        claimed_amount,
        shares,
    }
}

#[test]
#[ignore = "requires an SBF build with --sbf-stack-size 32768 --features sbf-test-fixtures"]
fn groth16_fixture_executes_inside_sbf() {
    let compute_units = execute_pipeline_proof(groth16::pipeline_fixture());
    eprintln!("Groth16 pipeline consumed {compute_units} CU");
    assert!(compute_units > 0);
}

#[test]
#[ignore = "requires an SBF v0 build with --features sbf-test-fixtures"]
fn three_of_seven_threshold_decryption_executes_inside_sbf() {
    let market = anchor_lang::prelude::Pubkey::new_unique();
    let key_epoch_address = anchor_lang::prelude::Pubkey::new_unique();
    let epoch = 1_u64;
    let setup_context = KeyRelationProofContext {
        program_id: beatex::ID.to_bytes(),
        market: market.to_bytes(),
        key_epoch_address: key_epoch_address.to_bytes(),
        epoch,
    };
    let setup = dealer_setup_3_of_7(&setup_context.to_bytes());
    let ciphertext = setup.public.encryption_pubkey.encrypt_u64(47);
    let ciphertext_bytes = ciphertext.to_bytes();
    let commitment = ciphertext_bytes[..32].try_into().unwrap();
    let auction_id = 42_u64;
    let side = 0_u8;
    let position = 2_u16;
    let selected = [0_usize, 2, 6];
    let shares = selected.map(|selected_position| {
        let participant_index = setup.secret_shares[selected_position].index() as u8;
        let context = DecryptionProofContext {
            program_id: beatex::ID.to_bytes(),
            market: market.to_bytes(),
            key_epoch_address: key_epoch_address.to_bytes(),
            epoch,
            auction_id,
            side,
            position,
            participant_index,
            commitment,
        };
        let share = create_decryption_share(
            &setup.secret_shares[selected_position],
            &ciphertext,
            &context.to_bytes(),
        );
        DecryptionShareInput {
            participant_index,
            point: share.point_bytes(),
            proof: dleq_input(&share.proof),
        }
    });

    let participant_public_shares = std::array::from_fn(|index| {
        setup.public.participant_shares[index]
            .point
            .compress()
            .to_bytes()
    });
    let participant_authorities =
        std::array::from_fn(|_| anchor_lang::prelude::Pubkey::new_unique());
    let coefficient_commitments = std::array::from_fn(|index| {
        setup.public.coefficient_commitments[index]
            .compress()
            .to_bytes()
    });
    let babyjub_encryption_pubkey =
        beatex_babyjubjub_groth16_spike::transition::deterministic_committee_key_bytes();
    let key_epoch = KeyEpoch {
        authority: anchor_lang::prelude::Pubkey::new_unique(),
        market,
        epoch,
        encryption_pubkey: setup.public.encryption_pubkey.to_bytes(),
        babyjub_encryption_pubkey,
        babyjub_participant_public_shares: [babyjub_encryption_pubkey; COMMITTEE_SIZE],
        coefficient_commitments,
        participant_public_shares,
        participant_authorities,
        key_relation_proof: dleq_input(&setup.public.key_relation_proof),
        threshold: 3,
        proof_transcript_version: 2,
        activation_slot: 1,
        retired: false,
        acknowledgement_bitmap: 0x7f,
        bump: 255,
    };
    let mut key_epoch_data = Vec::with_capacity(8 + KeyEpoch::INIT_SPACE);
    key_epoch.try_serialize(&mut key_epoch_data).unwrap();

    let mut svm = svm_with_program();
    svm.set_account(
        key_epoch_address,
        Account {
            lamports: 1_000_000_000,
            data: key_epoch_data,
            owner: beatex::ID,
            ..Account::default()
        },
    )
    .unwrap();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![AccountMeta::new_readonly(key_epoch_address, false)],
        data: beatex::instruction::VerifyThresholdDecryption {
            args: VerifyThresholdDecryptionArgs {
                auction_id,
                side,
                position,
                ciphertext: ciphertext_bytes,
                claimed_amount: 47,
                shares,
            },
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&payer],
        Message::new(&[instruction], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    let metadata = svm.send_transaction(transaction).unwrap();
    eprintln!(
        "3-of-7 threshold verification consumed {} CU",
        metadata.compute_units_consumed
    );
    assert!(metadata.compute_units_consumed > 0);
    assert_eq!(participant_public_shares.len(), COMMITTEE_SIZE);
}

#[test]
#[ignore = "requires an SBF v0 build with --features sbf-test-fixtures"]
fn plaintext_lifecycle_executes_inside_sbf() {
    let mut svm = svm_with_program();
    let authority = Keypair::new();
    svm.airdrop(&authority.pubkey(), 10_000_000_000).unwrap();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let base_vault = Pubkey::new_unique();
    let quote_vault = Pubkey::new_unique();
    for address in [base_mint, quote_mint, base_vault, quote_vault] {
        put_system_account(&mut svm, address);
    }

    let market_nonce = 7_u64;
    let (market, _) = Pubkey::find_program_address(
        &[
            b"market",
            base_mint.as_ref(),
            quote_mint.as_ref(),
            &market_nonce.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let (vault_authority, _) =
        Pubkey::find_program_address(&[b"vault-authority", market.as_ref()], &beatex::ID);
    let initialize_market = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(authority.pubkey(), true),
            AccountMeta::new_readonly(base_mint, false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new_readonly(base_vault, false),
            AccountMeta::new_readonly(quote_vault, false),
            AccountMeta::new_readonly(vault_authority, false),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data: beatex::instruction::InitializeMarket {
            args: InitializeMarketArgs {
                market_nonce,
                base_lot_size: 1,
                quote_lot_size: 1,
                tick_size: 10,
                grid_radius: 2,
                max_order_quantity: 1_000,
                max_aggregate_quantity: 10_000,
                initial_reference_price: 100,
                submission_duration_slots: 10,
                aggregation_duration_slots: 10,
                decryption_duration_slots: 10,
                claim_duration_slots: 10,
                allocation_rule_version: 1,
                order_circuit_version: 1,
                claim_circuit_version: 1,
            },
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(&[initialize_market], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();
    let initialized_market: Market = deserialize_account(&svm, &market);
    assert_eq!(initialized_market.last_clearing_price, 100);

    let epoch = 1_u64;
    let (key_epoch_address, _) = Pubkey::find_program_address(
        &[b"key-epoch", market.as_ref(), &epoch.to_le_bytes()],
        &beatex::ID,
    );
    let setup_context = KeyRelationProofContext {
        program_id: beatex::ID.to_bytes(),
        market: market.to_bytes(),
        key_epoch_address: key_epoch_address.to_bytes(),
        epoch,
    };
    let setup = dealer_setup_3_of_7(&setup_context.to_bytes());
    let babyjub_encryption_pubkey =
        beatex_babyjubjub_groth16_spike::transition::deterministic_committee_key_bytes();
    let participant_keypairs: [Keypair; COMMITTEE_SIZE] = std::array::from_fn(|_| Keypair::new());
    let participant_authorities = std::array::from_fn(|index| participant_keypairs[index].pubkey());
    let initialize_key_epoch = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(authority.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(key_epoch_address, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data: beatex::instruction::InitializeKeyEpoch {
            args: InitializeKeyEpochArgs {
                epoch,
                encryption_pubkey: setup.public.encryption_pubkey.to_bytes(),
                babyjub_encryption_pubkey,
                babyjub_participant_public_shares: [babyjub_encryption_pubkey; COMMITTEE_SIZE],
                coefficient_commitments: std::array::from_fn(|index| {
                    setup.public.coefficient_commitments[index]
                        .compress()
                        .to_bytes()
                }),
                participant_public_shares: std::array::from_fn(|index| {
                    setup.public.participant_shares[index]
                        .point
                        .compress()
                        .to_bytes()
                }),
                participant_authorities,
                key_relation_proof: dleq_input(&setup.public.key_relation_proof),
            },
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(1_000_000),
                initialize_key_epoch,
            ],
            Some(&authority.pubkey()),
        ),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();

    for (position, participant) in participant_keypairs.iter().enumerate() {
        let acknowledge = Instruction {
            program_id: beatex::ID,
            accounts: vec![
                AccountMeta::new_readonly(participant.pubkey(), true),
                AccountMeta::new(key_epoch_address, false),
            ],
            data: beatex::instruction::AcknowledgeKeyEpoch {
                participant_index: position as u8 + 1,
            }
            .data(),
        };
        let transaction = Transaction::new(
            &[&authority, participant],
            Message::new(&[acknowledge], Some(&authority.pubkey())),
            svm.latest_blockhash(),
        );
        svm.send_transaction(transaction).unwrap();
    }

    let activate = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new(key_epoch_address, false),
        ],
        data: beatex::instruction::ActivateKeyEpoch {}.data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(&[activate], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();

    let auction_id = 0_u64;
    let (auction, _) = Pubkey::find_program_address(
        &[b"auction", market.as_ref(), &auction_id.to_le_bytes()],
        &beatex::ID,
    );
    let open = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(authority.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(key_epoch_address, false),
            AccountMeta::new(auction, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data: beatex::instruction::OpenAuction { auction_id }.data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(&[open], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();
    let opened: Auction = deserialize_account(&svm, &auction);
    assert_eq!(opened.state, AuctionState::Open);

    let buy_quantities = [15_u64, 15, 11, 0, 0];
    let sell_quantities = [0_u64, 0, 2, 9, 12];
    let bidder_entries = buy_quantities
        .iter()
        .enumerate()
        .map(|(index, quantity)| EncryptedOrderEntry {
            index: index as u8,
            buy: setup
                .public
                .encryption_pubkey
                .encrypt_u64(*quantity)
                .to_bytes(),
            sell: setup.public.encryption_pubkey.encrypt_u64(0).to_bytes(),
        })
        .collect();
    let seller_entries = sell_quantities
        .iter()
        .enumerate()
        .map(|(index, quantity)| EncryptedOrderEntry {
            index: index as u8,
            buy: setup.public.encryption_pubkey.encrypt_u64(0).to_bytes(),
            sell: setup
                .public
                .encryption_pubkey
                .encrypt_u64(*quantity)
                .to_bytes(),
        })
        .collect();
    submit_test_encrypted_order(
        &mut svm,
        &authority,
        &Keypair::new(),
        market,
        auction,
        1,
        bidder_entries,
    );
    submit_test_encrypted_order(
        &mut svm,
        &authority,
        &Keypair::new(),
        market,
        auction,
        2,
        seller_entries,
    );
    let with_orders: Auction = deserialize_account(&svm, &auction);
    assert_eq!(with_orders.finalized_order_count, 2);

    let mut clock = svm.get_sysvar::<Clock>();
    clock.slot = opened.submission_end_slot;
    svm.set_sysvar(&clock);
    let seal = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(auction, false),
        ],
        data: beatex::instruction::SealAuction {}.data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(&[seal], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();

    let sealed: Auction = deserialize_account(&svm, &auction);
    let mut decryption_compute_units = 0_u64;
    for (side, amounts) in [(0_u8, buy_quantities), (1_u8, sell_quantities)] {
        for (position, amount) in amounts.into_iter().enumerate() {
            let ciphertext = if side == 0 {
                sealed.aggregate_buy[position]
            } else {
                sealed.aggregate_sell[position]
            };
            let finalize_entry = Instruction {
                program_id: beatex::ID,
                accounts: vec![
                    AccountMeta::new_readonly(market, false),
                    AccountMeta::new(auction, false),
                    AccountMeta::new_readonly(key_epoch_address, false),
                ],
                data: beatex::instruction::FinalizeDecryptedEntry {
                    args: threshold_args(
                        &setup,
                        market,
                        key_epoch_address,
                        epoch,
                        auction_id,
                        side,
                        position as u16,
                        ciphertext,
                        amount,
                    ),
                }
                .data(),
            };
            let transaction = Transaction::new(
                &[&authority],
                Message::new(&[finalize_entry], Some(&authority.pubkey())),
                svm.latest_blockhash(),
            );
            decryption_compute_units += svm
                .send_transaction(transaction)
                .unwrap()
                .compute_units_consumed;
        }
    }
    let finalize_decryption = Instruction {
        program_id: beatex::ID,
        accounts: vec![AccountMeta::new(auction, false)],
        data: beatex::instruction::FinalizeDecryption {}.data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(&[finalize_decryption], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();
    eprintln!("ten threshold-decrypted entries consumed {decryption_compute_units} CU total");

    let clear = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(market, false),
            AccountMeta::new(auction, false),
        ],
        data: beatex::instruction::ClearAuction {}.data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(&[clear], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    let metadata = svm.send_transaction(transaction).unwrap();
    let cleared: Auction = deserialize_account(&svm, &auction);
    assert_eq!(cleared.state, AuctionState::ClaimsOpen);
    assert_eq!(cleared.clearing_index, 2);
    assert_eq!(cleared.clearing_price, 100);
    assert_eq!(cleared.clearing_volume, 2);
    eprintln!(
        "plaintext clearing instruction consumed {} CU",
        metadata.compute_units_consumed
    );

    let cancelled_auction_id = 1_u64;
    let (cancelled_auction, _) = Pubkey::find_program_address(
        &[
            b"auction",
            market.as_ref(),
            &cancelled_auction_id.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let open_cancelled = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(authority.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(key_epoch_address, false),
            AccountMeta::new(cancelled_auction, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data: beatex::instruction::OpenAuction {
            auction_id: cancelled_auction_id,
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&authority],
        Message::new(&[open_cancelled], Some(&authority.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();
    let awaiting_timeout: Auction = deserialize_account(&svm, &cancelled_auction);
    let mut clock = svm.get_sysvar::<Clock>();
    clock.slot = awaiting_timeout.decryption_deadline_slot;
    svm.set_sysvar(&clock);
    let cancel = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(cancelled_auction, false),
        ],
        data: beatex::instruction::CancelExpiredAuction {}.data(),
    };
    let relayer = Keypair::new();
    svm.airdrop(&relayer.pubkey(), 1_000_000_000).unwrap();
    let transaction = Transaction::new(
        &[&relayer],
        Message::new(&[cancel], Some(&relayer.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction).unwrap();
    let cancelled: Auction = deserialize_account(&svm, &cancelled_auction);
    assert_eq!(cancelled.state, AuctionState::Cancelled);
}

#[test]
#[ignore = "requires an SBF build and deterministic aggregate-publication setup"]
fn aggregate_publication_proof_executes_inside_sbf() {
    let fixture =
        beatex_babyjubjub_groth16_spike::decryption::deterministic_solana_fixture().unwrap();
    let mut svm = svm_with_program();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 2_000_000_000).unwrap();

    let market = Pubkey::new_unique();
    let auction = Pubkey::new_from_array(fixture.auction);
    let key_epoch = Pubkey::new_unique();
    let counter = Pubkey::new_unique();
    let accumulator = Pubkey::new_unique();
    let verification_key = Pubkey::new_unique();
    let participant_authorities = std::array::from_fn(|_| Pubkey::new_unique());
    let market_state = Market {
        version: 1,
        authority: payer.pubkey(),
        base_mint: Pubkey::new_unique(),
        quote_mint: Pubkey::new_unique(),
        base_vault: Pubkey::new_unique(),
        quote_vault: Pubkey::new_unique(),
        base_lot_size: 1,
        quote_lot_size: 1,
        tick_size: 1,
        grid_radius: 8,
        max_order_quantity: 15,
        max_aggregate_quantity: 1_000,
        current_key_epoch: key_epoch,
        last_clearing_price: 100,
        last_cleared_auction_sequence: 0,
        next_auction_id: 1,
        submission_duration_slots: 10,
        aggregation_duration_slots: 10,
        decryption_duration_slots: 100,
        claim_duration_slots: 100,
        allocation_rule_version: 1,
        order_circuit_version: 1,
        claim_circuit_version: 1,
        paused: false,
        vault_authority_bump: 0,
        bump: 0,
    };
    let current_slot = svm.get_sysvar::<Clock>().slot;
    let auction_state = Auction {
        version: 1,
        market,
        auction_id: 0,
        key_epoch,
        state: AuctionState::Decrypting,
        open_slot: current_slot.saturating_sub(30),
        submission_end_slot: current_slot.saturating_sub(20),
        aggregation_deadline_slot: current_slot.saturating_sub(10),
        decryption_deadline_slot: current_slot + 100,
        claim_deadline_slot: current_slot + 200,
        reference_price: 100,
        tick_size: 1,
        grid_size: 17,
        finalized_order_count: 0,
        aggregate_buy: [[0; 64]; 17],
        aggregate_sell: [[0; 64]; 17],
        decrypted_demand: [0; 17],
        decrypted_supply: [0; 17],
        decrypted_entry_bitmap: 0,
        clearing_index: 8,
        clearing_price: 100,
        clearing_volume: 0,
        base_residual: 0,
        quote_residual: 0,
        bump: 0,
    };
    let key_epoch_state = KeyEpoch {
        authority: payer.pubkey(),
        market,
        epoch: 1,
        encryption_pubkey: [0; 32],
        babyjub_encryption_pubkey: fixture.committee_key,
        babyjub_participant_public_shares: fixture.participant_public_shares,
        coefficient_commitments: [[0; 32]; 3],
        participant_public_shares: [[0; 32]; COMMITTEE_SIZE],
        participant_authorities,
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
    let counter_state = SubmissionCounter {
        version: 1,
        auction,
        accepted_order_count: fixture.accepted_order_count,
        bump: 0,
    };
    let accumulator_state = AggregateAccumulator {
        version: 1,
        auction,
        folded_order_count: fixture.accepted_order_count,
        fold_revision: 3,
        ciphertexts: queue_hash::order_bytes(&fixture.aggregate),
        ciphertext_digest: [0; 32],
        bump: 0,
    };
    let vk_state = QueueVerificationKey {
        version: 1,
        market,
        circuit_kind: QueueCircuitKind::AggregatePublication,
        circuit_version: beatex::queue::AGGREGATE_PUBLICATION_CIRCUIT_VERSION,
        alpha_g1: fixture.vk_alpha_g1,
        beta_g2: fixture.vk_beta_g2,
        gamma_g2: fixture.vk_gamma_g2,
        delta_g2: fixture.vk_delta_g2,
        ic: fixture.vk_ic,
        bump: 0,
    };
    put_anchor_account(&mut svm, market, &market_state);
    put_anchor_account(&mut svm, auction, &auction_state);
    put_anchor_account(&mut svm, key_epoch, &key_epoch_state);
    put_anchor_account(&mut svm, counter, &counter_state);
    put_anchor_account(&mut svm, accumulator, &accumulator_state);
    put_anchor_account(&mut svm, verification_key, &vk_state);

    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(auction, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new_readonly(counter, false),
            AccountMeta::new_readonly(accumulator, false),
            AccountMeta::new_readonly(verification_key, false),
        ],
        data: beatex::instruction::PublishAggregateCurves {
            args: PublishAggregateCurvesArgs {
                participant_ids: fixture.participant_ids,
                demand: fixture.demand,
                supply: fixture.supply,
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
    let published: Auction = deserialize_account(&svm, &auction);
    assert_eq!(published.state, AuctionState::Decrypted);
    assert_eq!(published.decrypted_demand, fixture.demand);
    assert_eq!(published.decrypted_supply, fixture.supply);
    eprintln!(
        "aggregate publication proof consumed {} CU",
        metadata.compute_units_consumed
    );
}
