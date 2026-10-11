#![cfg(feature = "sbf-test-fixtures")]

use {
    anchor_lang::{
        prelude::{Clock, Pubkey},
        AccountDeserialize, AccountSerialize, AnchorDeserialize, InstructionData, Space,
    },
    beatex::{
        queue::{pending_event, record_pending_event, SUBMISSION_COUNTER_SPACE},
        queue_hash, ActiveOrder, AggregateAccumulator, Auction, AuctionResult, AuctionState,
        DecryptionShareInput, DleqProofInput, EncryptedPrivateBalance, FoldQueuedOrdersArgs,
        Groth16ProofInput, KeyEpoch, Market, PublishAggregateCurvesArgs, QueueCircuitKind,
        QueueVerificationKey, QueuedOrder, QueuedOrderState, RecentClearing,
        ReleaseOrderCollateralArgs, SubmissionCounter, SubmitQueuedOrderArgs,
        VerifyThresholdDecryptionArgs, WithdrawalState, COMMITTEE_SIZE, RECENT_CLEARINGS,
    },
    beatex_threshold_crypto::{
        create_decryption_share, dealer_setup_3_of_7, DecryptionProofContext,
        KeyRelationProofContext,
    },
    litesvm::LiteSVM,
    pyth_solana_receiver_sdk::{
        price_update::{PriceFeedMessage, PriceUpdateV2, VerificationLevel},
        ID as PYTH_RECEIVER_ID,
    },
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

fn dleq_input(proof: &beatex_threshold_crypto::DleqProof) -> DleqProofInput {
    DleqProofInput {
        commitment_1: proof.commitment_1_bytes(),
        commitment_2: proof.commitment_2_bytes(),
        response: proof.response_bytes(),
    }
}

#[cfg(any())]
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
    put_owned_anchor_account(svm, address, value, beatex::ID);
}

fn put_owned_anchor_account<T: AccountSerialize>(
    svm: &mut LiteSVM,
    address: Pubkey,
    value: &T,
    owner: Pubkey,
) {
    let mut data = Vec::new();
    value.try_serialize(&mut data).unwrap();
    svm.set_account(
        address,
        Account {
            lamports: 1_000_000_000,
            data,
            owner,
            ..Account::default()
        },
    )
    .unwrap();
}

/// Write a submission counter with its pending-event ring, as `initialize_auction` allocates it.
fn put_submission_counter(
    svm: &mut LiteSVM,
    address: Pubkey,
    counter: &SubmissionCounter,
    pending: &[(u64, Pubkey)],
) {
    let mut data = Vec::new();
    counter.try_serialize(&mut data).unwrap();
    data.resize(SUBMISSION_COUNTER_SPACE, 0);
    for (sequence, event) in pending {
        record_pending_event(&mut data, *sequence, *event).unwrap();
    }
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

fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &beatex::ID).0
}

fn account_is_closed(svm: &LiteSVM, address: &Pubkey) -> bool {
    svm.get_account(address)
        .is_none_or(|account| account.lamports == 0 && account.data.is_empty())
}

fn deserialize_account<T: AccountDeserialize>(svm: &LiteSVM, address: &Pubkey) -> T {
    let account = svm.get_account(address).unwrap();
    T::try_deserialize(&mut account.data.as_slice()).unwrap()
}

#[cfg(any())]
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
#[cfg(any())]
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

#[cfg(any())]
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
                base_unit_size: 100,
                quote_unit_size: 1,
                grid_step_bps: 10,
                grid_radius: 2,
                max_order_quantity: beatex::queue::MAX_PRIVATE_ORDER_QUANTITY,
                max_aggregate_quantity: 10_000,
                initial_reference_price: 100,
                submission_duration_slots: 10,
                aggregation_duration_slots: 10,
                decryption_duration_slots: 10,
                claim_duration_slots: 10,
                allocation_rule_version: 2,
                order_circuit_version: 2,
                claim_circuit_version: 2,
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
#[ignore = "requires an SBF v0 build with --features sbf-test-fixtures"]
fn temporal_auction_initialization_and_timeout_are_permissionless() {
    let mut svm = svm_with_program();
    let market_authority = Pubkey::new_unique();
    let relayer = Keypair::new();
    svm.airdrop(&relayer.pubkey(), 10_000_000_000).unwrap();

    let current_clock = svm.get_sysvar::<Clock>();
    let market = Pubkey::new_unique();
    let key_epoch = Pubkey::new_unique();
    let oracle_price = Pubkey::new_unique();
    let feed_id = [7_u8; 32];
    let market_state = Market {
        version: beatex::state::PROTOCOL_VERSION,
        authority: market_authority,
        base_mint: Pubkey::new_unique(),
        quote_mint: Pubkey::new_unique(),
        base_vault: Pubkey::new_unique(),
        quote_vault: Pubkey::new_unique(),
        base_unit_size: 100,
        quote_unit_size: 1,
        grid_step_bps: 10,
        grid_radius: 8,
        max_order_quantity: beatex::queue::MAX_PRIVATE_ORDER_QUANTITY,
        max_aggregate_quantity: 10_000,
        current_key_epoch: key_epoch,
        market_start_slot: current_clock.slot,
        last_clearing_price: 0,
        last_clearing_volume: 0,
        last_cleared_auction_sequence: 0,
        minimum_reference_volume: 1,
        oracle_price_account: oracle_price,
        oracle_feed_id: feed_id,
        oracle_max_age_seconds: 90,
        oracle_max_confidence_bps: 100,
        oracle_exponent_adjustment: -2,
        min_reference_price: 80,
        max_reference_price: 2_000,
        submission_duration_slots: 10,
        aggregation_duration_slots: 10,
        decryption_duration_slots: 10,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        paused: false,
        vault_authority_bump: 0,
        bump: 0,
        recent_clearings: [RecentClearing::default(); RECENT_CLEARINGS],
    };
    let key_epoch_state = KeyEpoch {
        authority: market_authority,
        market,
        epoch: 1,
        encryption_pubkey: [1; 32],
        babyjub_encryption_pubkey: [1; 32],
        babyjub_participant_public_shares: [[1; 32]; COMMITTEE_SIZE],
        coefficient_commitments: [[1; 32]; 3],
        participant_public_shares: [[1; 32]; COMMITTEE_SIZE],
        participant_authorities: [Pubkey::new_unique(); COMMITTEE_SIZE],
        key_relation_proof: DleqProofInput {
            commitment_1: [0; 32],
            commitment_2: [0; 32],
            response: [0; 32],
        },
        threshold: 3,
        proof_transcript_version: 2,
        activation_slot: current_clock.slot,
        retired: false,
        acknowledgement_bitmap: 0x7f,
        bump: 0,
    };
    let oracle_state = PriceUpdateV2 {
        write_authority: Pubkey::new_unique(),
        verification_level: VerificationLevel::Full,
        price_message: PriceFeedMessage {
            feed_id,
            price: 10_000_000_000_000,
            conf: 1_000_000,
            exponent: -8,
            publish_time: current_clock.unix_timestamp,
            prev_publish_time: current_clock.unix_timestamp.saturating_sub(1),
            ema_price: 10_000_000_000_000,
            ema_conf: 1_000_000,
        },
        posted_slot: current_clock.slot,
    };
    put_anchor_account(&mut svm, market, &market_state);
    put_anchor_account(&mut svm, key_epoch, &key_epoch_state);
    put_owned_anchor_account(&mut svm, oracle_price, &oracle_state, PYTH_RECEIVER_ID);

    let auction_id = 0_u64;
    let (auction, _) = Pubkey::find_program_address(
        &[b"auction", market.as_ref(), &auction_id.to_le_bytes()],
        &beatex::ID,
    );
    let (counter, _) =
        Pubkey::find_program_address(&[b"submission-counter", auction.as_ref()], &beatex::ID);
    let (accumulator, _) =
        Pubkey::find_program_address(&[b"aggregate-accumulator", auction.as_ref()], &beatex::ID);
    let initialize = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(relayer.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new_readonly(oracle_price, false),
            AccountMeta::new(auction, false),
            AccountMeta::new(counter, false),
            AccountMeta::new(accumulator, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data: beatex::instruction::InitializeAuction { auction_id }.data(),
    };
    let transaction = Transaction::new(
        &[&relayer],
        Message::new(&[initialize], Some(&relayer.pubkey())),
        svm.latest_blockhash(),
    );
    let metadata = svm.send_transaction(transaction).unwrap();
    let initialized: Auction = deserialize_account(&svm, &auction);
    assert_eq!(initialized.state, AuctionState::Pending);
    assert_eq!(initialized.reference_price, 1_000);
    assert!(initialized.reference_from_oracle);
    assert_eq!(
        initialized.oracle_publish_time,
        current_clock.unix_timestamp
    );
    assert_eq!(initialized.open_slot, current_clock.slot);
    let initialized_counter: SubmissionCounter = deserialize_account(&svm, &counter);
    let initialized_accumulator: AggregateAccumulator = deserialize_account(&svm, &accumulator);
    assert_eq!(initialized_counter.accepted_order_count, 0);
    assert_eq!(initialized_accumulator.folded_order_count, 0);
    eprintln!(
        "permissionless temporal initialization consumed {} CU",
        metadata.compute_units_consumed
    );

    let mut timeout_clock = svm.get_sysvar::<Clock>();
    timeout_clock.slot = initialized.decryption_deadline_slot;
    svm.set_sysvar(&timeout_clock);
    let cancel = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(auction, false),
            AccountMeta::new(counter, false),
            AccountMeta::new(accumulator, false),
            AccountMeta::new(market_authority, false),
            AccountMeta::new_readonly(event_authority(), false),
            AccountMeta::new_readonly(beatex::ID, false),
        ],
        data: beatex::instruction::CancelPendingAuction {}.data(),
    };
    let transaction = Transaction::new(
        &[&relayer],
        Message::new(&[cancel], Some(&relayer.pubkey())),
        svm.latest_blockhash(),
    );
    let authority_before = svm.get_balance(&market_authority).unwrap_or(0);
    let cancel_metadata = svm.send_transaction(transaction).unwrap();
    // The cancellation event is a self-CPI an indexer can decode.
    let event = cancel_metadata.inner_instructions[0]
        .iter()
        .map(|inner| &inner.instruction.data)
        .find(|data| data.starts_with(anchor_lang::event::EVENT_IX_TAG_LE))
        .expect("cancellation emits an event");
    let event = beatex::AuctionCancelled::try_from_slice(event[16..].as_ref()).unwrap();
    assert_eq!(event.market, market);
    // Cancellation compacts the auction and reclaims its queue accounts.
    let cancelled: AuctionResult = deserialize_account(&svm, &auction);
    assert_eq!(cancelled.state, AuctionState::Cancelled);
    assert_eq!(
        svm.get_account(&auction).unwrap().data.len(),
        8 + AuctionResult::INIT_SPACE
    );
    assert!(account_is_closed(&svm, &counter));
    assert!(account_is_closed(&svm, &accumulator));
    assert!(svm.get_balance(&market_authority).unwrap_or(0) > authority_before);
}

#[test]
#[ignore = "requires an SBF v0 build and deterministic collateral-release setup"]
fn cancelled_order_refund_to_balance_executes_inside_sbf() {
    let mut svm = svm_with_program();
    let owner = Keypair::new();
    svm.airdrop(&owner.pubkey(), 10_000_000_000).unwrap();
    let mut clock = svm.get_sysvar::<Clock>();
    clock.slot = 100;
    svm.set_sysvar(&clock);

    let market = Pubkey::new_unique();
    let auction = Pubkey::new_unique();
    let key_epoch = Pubkey::new_unique();
    let private_balance = Pubkey::new_unique();
    let previous_event = Pubkey::new_unique();
    let release_vk = Pubkey::new_unique();
    let (active_order, _) = Pubkey::find_program_address(
        &[b"active-order", auction.as_ref(), owner.pubkey().as_ref()],
        &beatex::ID,
    );
    let fixture =
        beatex_babyjubjub_groth16_spike::release::deterministic_solana_fixture_for_context(
            market.to_bytes(),
            auction.to_bytes(),
            active_order.to_bytes(),
            owner.pubkey().to_bytes(),
        )
        .unwrap();
    let market_state = Market {
        version: beatex::state::PROTOCOL_VERSION,
        authority: owner.pubkey(),
        base_mint: Pubkey::new_unique(),
        quote_mint: Pubkey::new_unique(),
        base_vault: Pubkey::new_unique(),
        quote_vault: Pubkey::new_unique(),
        base_unit_size: 100,
        quote_unit_size: 1,
        grid_step_bps: 1,
        grid_radius: 8,
        max_order_quantity: 15,
        max_aggregate_quantity: 10_000,
        current_key_epoch: key_epoch,
        market_start_slot: 0,
        last_clearing_price: 100,
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
        submission_duration_slots: 50,
        aggregation_duration_slots: 50,
        decryption_duration_slots: 50,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        paused: false,
        vault_authority_bump: 0,
        bump: 0,
        recent_clearings: [RecentClearing::default(); RECENT_CLEARINGS],
    };
    let auction_state = Auction {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction_id: 0,
        key_epoch,
        state: AuctionState::Cancelled,
        open_slot: 90,
        submission_end_slot: 150,
        aggregation_deadline_slot: 200,
        decryption_deadline_slot: 250,
        reference_price: 100,
        reference_from_oracle: false,
        oracle_publish_time: 0,
        grid_step_bps: 1,
        grid_size: 17,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
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
        authority: owner.pubkey(),
        market,
        epoch: 1,
        encryption_pubkey: [1; 32],
        babyjub_encryption_pubkey: fixture.committee_key,
        babyjub_participant_public_shares: [fixture.committee_key; COMMITTEE_SIZE],
        coefficient_commitments: [[1; 32]; 3],
        participant_public_shares: [[1; 32]; COMMITTEE_SIZE],
        participant_authorities: [owner.pubkey(); COMMITTEE_SIZE],
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
    let balance_state = EncryptedPrivateBalance {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        owner: owner.pubkey(),
        key_epoch,
        encrypted_base: fixture.balance_ciphertexts[0],
        encrypted_quote: fixture.balance_ciphertexts[1],
        deposited_base: 100,
        deposited_quote: 10_000_000,
        balance_digest: [0; 32],
        revision: fixture.balance_revision,
        bump: 0,
    };
    let previous_event_state = QueuedOrder {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction,
        owner: owner.pubkey(),
        funder: owner.pubkey(),
        client_order_nonce: fixture.client_order_nonce,
        sequence: 0,
        state: QueuedOrderState::Accepted,
        withdrawal_state: WithdrawalState::Pending,
        terminal_nullifier: [0; 32],
        previous_event: Pubkey::default(),
        ciphertexts: queue_hash::order_bytes(&queue_hash::identity_order()),
        ciphertext_digest: [0; 32],
        order_commitment: fixture.order_commitment,
        order_circuit_version: 2,
        bump: 0,
    };
    let active_order_state = ActiveOrder {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction,
        owner: owner.pubkey(),
        latest_event: previous_event,
        revision: 0,
        active: true,
        bump: 0,
    };
    let release_vk_state = QueueVerificationKey {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        circuit_kind: QueueCircuitKind::OrderModification,
        circuit_version: beatex::queue::ORDER_MODIFICATION_CIRCUIT_VERSION,
        alpha_g1: fixture.vk_alpha_g1,
        beta_g2: fixture.vk_beta_g2,
        gamma_g2: fixture.vk_gamma_g2,
        delta_g2: fixture.vk_delta_g2,
        ic: fixture.vk_ic,
        bump: 0,
    };
    put_anchor_account(&mut svm, market, &market_state);
    put_anchor_account(
        &mut svm,
        auction,
        &AuctionResult::from_auction(&auction_state),
    );
    put_anchor_account(&mut svm, key_epoch, &key_epoch_state);
    put_anchor_account(&mut svm, private_balance, &balance_state);
    put_anchor_account(&mut svm, previous_event, &previous_event_state);
    put_anchor_account(&mut svm, active_order, &active_order_state);
    put_anchor_account(&mut svm, release_vk, &release_vk_state);

    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(owner.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(auction, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new_readonly(release_vk, false),
            AccountMeta::new(private_balance, false),
            AccountMeta::new(active_order, false),
            AccountMeta::new(previous_event, false),
        ],
        data: beatex::instruction::RefundOrderToBalance {
            args: ReleaseOrderCollateralArgs {
                released_encrypted_base: fixture.balance_ciphertexts[2],
                released_encrypted_quote: fixture.balance_ciphertexts[3],
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
        Message::new(
            &[
                ComputeBudgetInstruction::request_heap_frame(256 * 1024),
                ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                instruction,
            ],
            Some(&owner.pubkey()),
        ),
        svm.latest_blockhash(),
    );
    let metadata = svm.send_transaction(transaction).unwrap();
    let balance: EncryptedPrivateBalance = deserialize_account(&svm, &private_balance);
    // The refunded order is terminal, so both accounts are closed.
    assert!(account_is_closed(&svm, &previous_event));
    assert!(account_is_closed(&svm, &active_order));
    assert_eq!(balance.encrypted_base, fixture.balance_ciphertexts[2]);
    assert_eq!(balance.encrypted_quote, fixture.balance_ciphertexts[3]);
    eprintln!(
        "cancelled-order internal refund consumed {} CU",
        metadata.compute_units_consumed
    );
}

#[test]
#[ignore = "requires an SBF v0 build and deterministic order/fold setups"]
fn queued_order_and_replacement_aware_fold_execute_inside_sbf() {
    let mut svm = svm_with_program();
    let owner = Keypair::new();
    svm.airdrop(&owner.pubkey(), 10_000_000_000).unwrap();
    let mut clock = svm.get_sysvar::<Clock>();
    clock.slot = 100;
    svm.set_sysvar(&clock);

    let market = Pubkey::new_unique();
    let auction = Pubkey::new_unique();
    let key_epoch = Pubkey::new_unique();
    let order_vk = Pubkey::new_unique();
    let counter = Pubkey::new_unique();
    let private_balance = Pubkey::new_unique();
    let order_fixture =
        beatex_babyjubjub_groth16_spike::transition::deterministic_solana_fixture_for_context(
            auction.to_bytes(),
            owner.pubkey().to_bytes(),
        )
        .unwrap();

    let market_state = Market {
        version: beatex::state::PROTOCOL_VERSION,
        authority: owner.pubkey(),
        base_mint: Pubkey::new_unique(),
        quote_mint: Pubkey::new_unique(),
        base_vault: Pubkey::new_unique(),
        quote_vault: Pubkey::new_unique(),
        base_unit_size: 100,
        quote_unit_size: 1,
        grid_step_bps: order_fixture.grid_step_bps,
        grid_radius: 8,
        max_order_quantity: 15,
        max_aggregate_quantity: 10_000,
        current_key_epoch: key_epoch,
        market_start_slot: 0,
        last_clearing_price: order_fixture.reference_price,
        last_clearing_volume: 0,
        last_cleared_auction_sequence: 0,
        minimum_reference_volume: 1,
        oracle_price_account: Pubkey::new_unique(),
        oracle_feed_id: [1; 32],
        oracle_max_age_seconds: 90,
        oracle_max_confidence_bps: 100,
        oracle_exponent_adjustment: 0,
        min_reference_price: 1,
        max_reference_price: u64::MAX - 10_000,
        submission_duration_slots: 50,
        aggregation_duration_slots: 50,
        decryption_duration_slots: 50,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        paused: false,
        vault_authority_bump: 0,
        bump: 0,
        recent_clearings: [RecentClearing::default(); RECENT_CLEARINGS],
    };
    let auction_state = Auction {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction_id: 0,
        key_epoch,
        state: AuctionState::Pending,
        open_slot: 90,
        submission_end_slot: 150,
        aggregation_deadline_slot: 200,
        decryption_deadline_slot: 250,
        reference_price: order_fixture.reference_price,
        reference_from_oracle: false,
        oracle_publish_time: 0,
        grid_step_bps: order_fixture.grid_step_bps,
        grid_size: 17,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        finalized_order_count: 0,
        aggregate_buy: [[0; 64]; 17],
        aggregate_sell: [[0; 64]; 17],
        decrypted_demand: [0; 17],
        decrypted_supply: [0; 17],
        decrypted_entry_bitmap: 0,
        clearing_index: 8,
        clearing_price: order_fixture.reference_price,
        clearing_volume: 0,
        base_residual: 0,
        quote_residual: 0,
        bump: 0,
    };
    let key_epoch_state = KeyEpoch {
        authority: owner.pubkey(),
        market,
        epoch: 1,
        encryption_pubkey: [1; 32],
        babyjub_encryption_pubkey: order_fixture.committee_key,
        babyjub_participant_public_shares: [order_fixture.committee_key; COMMITTEE_SIZE],
        coefficient_commitments: [[1; 32]; 3],
        participant_public_shares: [[1; 32]; COMMITTEE_SIZE],
        participant_authorities: [owner.pubkey(); COMMITTEE_SIZE],
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
    let order_vk_state = QueueVerificationKey {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        circuit_kind: QueueCircuitKind::OrderSubmission,
        circuit_version: 2,
        alpha_g1: order_fixture.vk_alpha_g1,
        beta_g2: order_fixture.vk_beta_g2,
        gamma_g2: order_fixture.vk_gamma_g2,
        delta_g2: order_fixture.vk_delta_g2,
        ic: order_fixture.vk_ic,
        bump: 0,
    };
    let counter_state = SubmissionCounter {
        version: beatex::state::PROTOCOL_VERSION,
        auction,
        accepted_order_count: 7,
        bump: 0,
    };
    let balance_state = EncryptedPrivateBalance {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        owner: owner.pubkey(),
        key_epoch,
        encrypted_base: order_fixture.old_base,
        encrypted_quote: order_fixture.old_quote,
        deposited_base: 100,
        deposited_quote: 10_000_000,
        balance_digest: [0; 32],
        revision: 0,
        bump: 0,
    };
    put_anchor_account(&mut svm, market, &market_state);
    put_anchor_account(&mut svm, auction, &auction_state);
    put_anchor_account(&mut svm, key_epoch, &key_epoch_state);
    put_anchor_account(&mut svm, order_vk, &order_vk_state);
    put_submission_counter(&mut svm, counter, &counter_state, &[]);
    put_anchor_account(&mut svm, private_balance, &balance_state);

    let (order, _) = Pubkey::find_program_address(
        &[
            b"queued-order",
            auction.as_ref(),
            owner.pubkey().as_ref(),
            &order_fixture.client_order_nonce.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let (active_order, _) = Pubkey::find_program_address(
        &[b"active-order", auction.as_ref(), owner.pubkey().as_ref()],
        &beatex::ID,
    );
    let submit = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(owner.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(auction, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new_readonly(order_vk, false),
            AccountMeta::new(counter, false),
            AccountMeta::new(private_balance, false),
            AccountMeta::new(order, false),
            AccountMeta::new(active_order, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data: beatex::instruction::SubmitQueuedOrder {
            args: SubmitQueuedOrderArgs {
                client_order_nonce: order_fixture.client_order_nonce,
                order_commitment: order_fixture.order_commitment,
                ciphertexts: queue_hash::order_bytes(&order_fixture.ciphertexts),
                new_encrypted_base: order_fixture.new_base,
                new_encrypted_quote: order_fixture.new_quote,
                proof: Groth16ProofInput {
                    proof_a: order_fixture.proof_a,
                    proof_b: order_fixture.proof_b,
                    proof_c: order_fixture.proof_c,
                    public_input: order_fixture.public_input,
                },
            },
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&owner],
        Message::new(
            &[
                ComputeBudgetInstruction::request_heap_frame(256 * 1024),
                ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                submit,
            ],
            Some(&owner.pubkey()),
        ),
        svm.latest_blockhash(),
    );
    let metadata = svm.send_transaction(transaction).unwrap();
    let submitted: QueuedOrder = deserialize_account(&svm, &order);
    let active: ActiveOrder = deserialize_account(&svm, &active_order);
    let balance: EncryptedPrivateBalance = deserialize_account(&svm, &private_balance);
    assert_eq!(submitted.state, QueuedOrderState::Accepted);
    assert_eq!(submitted.sequence, 7);
    assert_eq!(active.latest_event, order);
    assert_eq!(balance.encrypted_base, order_fixture.new_base);
    let submitted_counter: SubmissionCounter = deserialize_account(&svm, &counter);
    assert_eq!(submitted_counter.accepted_order_count, 8);
    assert_eq!(
        pending_event(&svm.get_account(&counter).unwrap().data, 7).unwrap(),
        order
    );
    eprintln!(
        "17-level queued-order submission consumed {} CU",
        metadata.compute_units_consumed
    );

    let fold_fixture =
        beatex_babyjubjub_groth16_spike::transition::deterministic_fold_solana_fixture().unwrap();
    let fold_auction = Pubkey::new_from_array(fold_fixture.auction);
    let fold_market = Pubkey::new_unique();
    let fold_counter = Pubkey::new_unique();
    let fold_accumulator = Pubkey::new_unique();
    let fold_vk = Pubkey::new_unique();
    let mut fold_market_state = market_state;
    fold_market_state.authority = owner.pubkey();
    let mut fold_auction_state = auction_state;
    fold_auction_state.market = fold_market;
    fold_auction_state.key_epoch = key_epoch;
    fold_auction_state.open_slot = 90;
    fold_auction_state.submission_end_slot = 150;
    fold_auction_state.aggregation_deadline_slot = 200;
    let fold_pending_events: Vec<(u64, Pubkey)> = (0u64..)
        .zip(&fold_fixture.order_keys[..3])
        .map(|(offset, key)| {
            (
                fold_fixture.old_folded_count + offset,
                Pubkey::new_from_array(*key),
            )
        })
        .collect();
    let fold_counter_state = SubmissionCounter {
        version: beatex::state::PROTOCOL_VERSION,
        auction: fold_auction,
        accepted_order_count: fold_fixture.old_folded_count + 3,
        bump: 0,
    };
    let fold_accumulator_state = AggregateAccumulator {
        version: beatex::state::PROTOCOL_VERSION,
        auction: fold_auction,
        folded_order_count: fold_fixture.old_folded_count,
        fold_revision: 4,
        ciphertexts: queue_hash::order_bytes(&fold_fixture.old_aggregate),
        ciphertext_digest: [0; 32],
        bump: 0,
    };
    let fold_vk_state = QueueVerificationKey {
        version: beatex::state::PROTOCOL_VERSION,
        market: fold_market,
        circuit_kind: QueueCircuitKind::AggregateFold,
        circuit_version: beatex::queue::AGGREGATE_FOLD_CIRCUIT_VERSION,
        alpha_g1: fold_fixture.vk_alpha_g1,
        beta_g2: fold_fixture.vk_beta_g2,
        gamma_g2: fold_fixture.vk_gamma_g2,
        delta_g2: fold_fixture.vk_delta_g2,
        ic: fold_fixture.vk_ic,
        bump: 0,
    };
    put_anchor_account(&mut svm, fold_market, &fold_market_state);
    put_anchor_account(&mut svm, fold_auction, &fold_auction_state);
    put_submission_counter(
        &mut svm,
        fold_counter,
        &fold_counter_state,
        &fold_pending_events,
    );
    put_anchor_account(&mut svm, fold_accumulator, &fold_accumulator_state);
    put_anchor_account(&mut svm, fold_vk, &fold_vk_state);
    for (index, order_address) in fold_fixture.order_keys.iter().enumerate().take(3) {
        let order_address = Pubkey::new_from_array(*order_address);
        let order_state = QueuedOrder {
            version: beatex::state::PROTOCOL_VERSION,
            market: fold_market,
            auction: fold_auction,
            owner: owner.pubkey(),
            funder: owner.pubkey(),
            client_order_nonce: index as u64,
            sequence: fold_fixture.old_folded_count + index as u64,
            state: QueuedOrderState::Accepted,
            withdrawal_state: WithdrawalState::Pending,
            terminal_nullifier: [0; 32],
            previous_event: Pubkey::default(),
            ciphertexts: queue_hash::order_bytes(&queue_hash::identity_order()),
            ciphertext_digest: fold_fixture.order_digests[index],
            order_commitment: [0; 32],
            order_circuit_version: 2,
            bump: 0,
        };
        put_anchor_account(&mut svm, order_address, &order_state);
    }
    let mut accounts = vec![
        AccountMeta::new_readonly(owner.pubkey(), true),
        AccountMeta::new_readonly(fold_market, false),
        AccountMeta::new_readonly(fold_auction, false),
        AccountMeta::new(fold_counter, false),
        AccountMeta::new(fold_accumulator, false),
        AccountMeta::new_readonly(fold_vk, false),
    ];
    accounts.extend(
        fold_fixture.order_keys[..3]
            .iter()
            .map(|key| AccountMeta::new(Pubkey::new_from_array(*key), false)),
    );
    let fold = Instruction {
        program_id: beatex::ID,
        accounts,
        data: beatex::instruction::FoldQueuedOrders {
            args: FoldQueuedOrdersArgs {
                new_aggregate: queue_hash::order_bytes(&fold_fixture.new_aggregate),
                proof: Groth16ProofInput {
                    proof_a: fold_fixture.proof_a,
                    proof_b: fold_fixture.proof_b,
                    proof_c: fold_fixture.proof_c,
                    public_input: fold_fixture.public_input,
                },
            },
        }
        .data(),
    };
    let transaction = Transaction::new(
        &[&owner],
        Message::new(
            &[
                ComputeBudgetInstruction::request_heap_frame(256 * 1024),
                ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                fold,
            ],
            Some(&owner.pubkey()),
        ),
        svm.latest_blockhash(),
    );
    let metadata = svm.send_transaction(transaction).unwrap();
    let folded: AggregateAccumulator = deserialize_account(&svm, &fold_accumulator);
    assert_eq!(folded.folded_order_count, fold_fixture.old_folded_count + 3);
    assert_eq!(
        folded.ciphertexts,
        queue_hash::order_bytes(&fold_fixture.new_aggregate)
    );
    for key in &fold_fixture.order_keys[..3] {
        let order: QueuedOrder = deserialize_account(&svm, &Pubkey::new_from_array(*key));
        assert_eq!(order.state, QueuedOrderState::Folded);
    }
    let fold_counter_after = svm.get_account(&fold_counter).unwrap().data;
    for (sequence, _) in &fold_pending_events {
        assert_eq!(
            pending_event(&fold_counter_after, *sequence).unwrap(),
            Pubkey::default()
        );
    }
    eprintln!(
        "replacement-aware three-order fold consumed {} CU",
        metadata.compute_units_consumed
    );
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
    let mut fixture_clock = svm.get_sysvar::<Clock>();
    fixture_clock.slot = 100;
    svm.set_sysvar(&fixture_clock);
    let current_slot = fixture_clock.slot;
    let market_state = Market {
        version: beatex::state::PROTOCOL_VERSION,
        authority: payer.pubkey(),
        base_mint: Pubkey::new_unique(),
        quote_mint: Pubkey::new_unique(),
        base_vault: Pubkey::new_unique(),
        quote_vault: Pubkey::new_unique(),
        base_unit_size: 100,
        quote_unit_size: 1,
        grid_step_bps: 1,
        grid_radius: 8,
        max_order_quantity: 15,
        max_aggregate_quantity: 1_000,
        current_key_epoch: key_epoch,
        market_start_slot: current_slot.saturating_sub(30),
        last_clearing_price: 100,
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
        decryption_duration_slots: 100,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        paused: false,
        vault_authority_bump: 0,
        bump: 0,
        recent_clearings: [RecentClearing::default(); RECENT_CLEARINGS],
    };
    let auction_state = Auction {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction_id: 0,
        key_epoch,
        state: AuctionState::Pending,
        open_slot: current_slot.saturating_sub(30),
        submission_end_slot: current_slot.saturating_sub(20),
        aggregation_deadline_slot: current_slot.saturating_sub(10),
        decryption_deadline_slot: current_slot + 100,
        // Large enough that 1 bps grid steps give distinct price levels.
        reference_price: 1_000_000,
        reference_from_oracle: false,
        oracle_publish_time: 0,
        grid_step_bps: 1,
        grid_size: 17,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        finalized_order_count: 0,
        aggregate_buy: [[0; 64]; 17],
        aggregate_sell: [[0; 64]; 17],
        decrypted_demand: [0; 17],
        decrypted_supply: [0; 17],
        decrypted_entry_bitmap: 0,
        clearing_index: 8,
        clearing_price: 1_000_000,
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
        version: beatex::state::PROTOCOL_VERSION,
        auction,
        accepted_order_count: fixture.accepted_order_count,
        bump: 0,
    };
    let accumulator_state = AggregateAccumulator {
        version: beatex::state::PROTOCOL_VERSION,
        auction,
        folded_order_count: fixture.accepted_order_count,
        fold_revision: 3,
        ciphertexts: queue_hash::order_bytes(&fixture.aggregate),
        ciphertext_digest: [0; 32],
        bump: 0,
    };
    let vk_state = QueueVerificationKey {
        version: beatex::state::PROTOCOL_VERSION,
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
    put_submission_counter(&mut svm, counter, &counter_state, &[]);
    put_anchor_account(&mut svm, accumulator, &accumulator_state);
    put_anchor_account(&mut svm, verification_key, &vk_state);

    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(market, false),
            AccountMeta::new(auction, false),
            AccountMeta::new_readonly(key_epoch, false),
            AccountMeta::new(counter, false),
            AccountMeta::new(accumulator, false),
            AccountMeta::new_readonly(verification_key, false),
            AccountMeta::new(payer.pubkey(), false),
            AccountMeta::new_readonly(event_authority(), false),
            AccountMeta::new_readonly(beatex::ID, false),
        ],
        data: beatex::instruction::PublishAndClear {
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
    let published: AuctionResult = deserialize_account(&svm, &auction);
    assert_eq!(published.state, AuctionState::Settled);
    let clearing = usize::from(published.clearing_index);
    assert_eq!(published.demand_at_clearing, fixture.demand[clearing]);
    assert_eq!(published.supply_at_clearing, fixture.supply[clearing]);
    assert!(published.clearing_price > 0);
    assert!(account_is_closed(&svm, &counter));
    assert!(account_is_closed(&svm, &accumulator));
    let market_after: Market = deserialize_account(&svm, &market);
    assert_eq!(
        market_after.recent_clearings[0].clearing_price,
        published.clearing_price
    );
    let event = metadata.inner_instructions[1]
        .iter()
        .map(|inner| &inner.instruction.data)
        .find(|data| data.starts_with(anchor_lang::event::EVENT_IX_TAG_LE))
        .expect("publication emits an event");
    let event = beatex::AuctionCleared::try_from_slice(&event[16..]).unwrap();
    assert_eq!(event.clearing_price, published.clearing_price);
    assert_eq!(event.demand, fixture.demand);
    assert_eq!(event.supply, fixture.supply);
    eprintln!(
        "aggregate publication proof consumed {} CU",
        metadata.compute_units_consumed
    );
}

#[test]
#[ignore = "requires an SBF v0 build"]
fn order_events_close_once_the_auction_stops_folding() {
    let mut svm = svm_with_program();
    let payer = Keypair::new();
    let owner = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
    svm.airdrop(&owner.pubkey(), 1_000_000).unwrap();

    let market = Pubkey::new_unique();
    let auction = Pubkey::new_unique();
    let (active_order, _) = Pubkey::find_program_address(
        &[b"active-order", auction.as_ref(), owner.pubkey().as_ref()],
        &beatex::ID,
    );
    let superseded = Pubkey::new_unique();
    let latest = Pubkey::new_unique();
    let foreign = Pubkey::new_unique();
    let market_state = Market {
        version: beatex::state::PROTOCOL_VERSION,
        authority: payer.pubkey(),
        base_mint: Pubkey::new_unique(),
        quote_mint: Pubkey::new_unique(),
        base_vault: Pubkey::new_unique(),
        quote_vault: Pubkey::new_unique(),
        base_unit_size: 100,
        quote_unit_size: 1,
        grid_step_bps: 10,
        grid_radius: 8,
        max_order_quantity: 15,
        max_aggregate_quantity: 10_000,
        current_key_epoch: Pubkey::new_unique(),
        market_start_slot: 0,
        last_clearing_price: 100,
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
        submission_duration_slots: 50,
        aggregation_duration_slots: 50,
        decryption_duration_slots: 50,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
        paused: false,
        vault_authority_bump: 0,
        bump: 0,
        recent_clearings: [RecentClearing::default(); RECENT_CLEARINGS],
    };
    let mut auction_state = Auction {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction_id: 0,
        key_epoch: market_state.current_key_epoch,
        state: AuctionState::Pending,
        open_slot: 0,
        submission_end_slot: 50,
        aggregation_deadline_slot: 100,
        decryption_deadline_slot: 150,
        reference_price: 100,
        reference_from_oracle: false,
        oracle_publish_time: 0,
        grid_step_bps: 10,
        grid_size: 17,
        allocation_rule_version: 2,
        order_circuit_version: 2,
        claim_circuit_version: 2,
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
    let event = |owner: Pubkey, previous_event: Pubkey| QueuedOrder {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction,
        owner,
        funder: owner,
        client_order_nonce: 0,
        sequence: 0,
        state: QueuedOrderState::Folded,
        withdrawal_state: WithdrawalState::Pending,
        terminal_nullifier: [0; 32],
        previous_event,
        ciphertexts: queue_hash::order_bytes(&queue_hash::identity_order()),
        ciphertext_digest: [0; 32],
        order_commitment: [0; 32],
        order_circuit_version: 2,
        bump: 0,
    };
    let mut pointer = ActiveOrder {
        version: beatex::state::PROTOCOL_VERSION,
        market,
        auction,
        owner: owner.pubkey(),
        latest_event: latest,
        revision: 1,
        active: true,
        bump: 0,
    };
    put_anchor_account(&mut svm, market, &market_state);
    put_anchor_account(&mut svm, auction, &auction_state);
    put_anchor_account(
        &mut svm,
        superseded,
        &event(owner.pubkey(), Pubkey::default()),
    );
    put_anchor_account(&mut svm, latest, &event(owner.pubkey(), superseded));
    put_anchor_account(
        &mut svm,
        foreign,
        &event(Pubkey::new_unique(), Pubkey::default()),
    );
    put_anchor_account(&mut svm, active_order, &pointer);

    let close = |svm: &mut LiteSVM, events: &[Pubkey]| {
        let mut accounts = vec![
            AccountMeta::new(owner.pubkey(), false),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(auction, false),
            AccountMeta::new(active_order, false),
        ];
        accounts.extend(events.iter().map(|event| AccountMeta::new(*event, false)));
        svm.expire_blockhash();
        svm.send_transaction(Transaction::new(
            &[&payer],
            Message::new(
                &[Instruction {
                    program_id: beatex::ID,
                    accounts,
                    data: beatex::instruction::CloseOrderEvents {}.data(),
                }],
                Some(&payer.pubkey()),
            ),
            svm.latest_blockhash(),
        ))
    };
    let lamports = |svm: &LiteSVM, address: &Pubkey| svm.get_account(address).unwrap().lamports;

    // Events stay while the auction can still fold them.
    assert!(close(&mut svm, &[superseded]).is_err());

    auction_state.state = AuctionState::Settled;
    put_anchor_account(
        &mut svm,
        auction,
        &AuctionResult::from_auction(&auction_state),
    );
    // Another trader's event, and an active order's latest event, are kept.
    assert!(close(&mut svm, &[foreign]).is_err());
    assert!(close(&mut svm, &[latest]).is_err());

    // A superseded event closes even while the order is still active.
    let before = lamports(&svm, &owner.pubkey());
    close(&mut svm, &[superseded]).unwrap();
    assert!(account_is_closed(&svm, &superseded));
    assert!(!account_is_closed(&svm, &active_order));
    assert_eq!(lamports(&svm, &owner.pubkey()), before + 1_000_000_000);

    // Once the order is inactive (e.g. cancelled), the rest is reclaimed.
    pointer.active = false;
    put_anchor_account(&mut svm, active_order, &pointer);
    let before = lamports(&svm, &owner.pubkey());
    close(&mut svm, &[latest]).unwrap();
    assert!(account_is_closed(&svm, &latest));
    assert!(account_is_closed(&svm, &active_order));
    assert_eq!(lamports(&svm, &owner.pubkey()), before + 2_000_000_000);
}
