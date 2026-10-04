use {
    beatex::{
        auction::{clear_auction, OrderCurve, PriceGrid},
        ID,
    },
    beatex_threshold_crypto::{
        create_decryption_share, dealer_setup_3_of_7, verify_plaintext_with_share_contexts,
        DecryptionProofContext, KeyRelationProofContext,
    },
    solana_zk_sdk::encryption::elgamal::{ElGamalCiphertext, ElGamalPubkey},
};

const MARKET: [u8; 32] = [7; 32];
const KEY_EPOCH: [u8; 32] = [8; 32];
const EPOCH: u64 = 1;
const AUCTION_ID: u64 = 42;

fn aggregate_level(public_key: &ElGamalPubkey, quantities: &[u64]) -> ElGamalCiphertext {
    quantities
        .iter()
        .map(|quantity| public_key.encrypt_u64(*quantity))
        .reduce(|aggregate, ciphertext| aggregate + ciphertext)
        .expect("the demo has at least one trader")
}

fn main() {
    let program_id = ID.to_bytes();
    let setup_context = KeyRelationProofContext {
        program_id,
        market: MARKET,
        key_epoch_address: KEY_EPOCH,
        epoch: EPOCH,
    }
    .to_bytes();
    let setup = dealer_setup_3_of_7(&setup_context);

    let buys = [[10, 10, 6, 0, 0], [5, 5, 5, 0, 0], [0, 0, 0, 0, 0]];
    let sells = [[0, 0, 0, 4, 4], [0, 0, 0, 3, 6], [0, 0, 2, 2, 2]];
    let grid = PriceGrid {
        reference_price: 100,
        tick_size: 5,
        level_count: 5,
    };

    for trader in 0..buys.len() {
        OrderCurve {
            buy: &buys[trader],
            sell: &sells[trader],
        }
        .validate(&grid)
        .expect("demo orders satisfy the public reference rules");
    }

    let mut demand = [0_u64; 5];
    let mut supply = [0_u64; 5];
    for position in 0..5 {
        demand[position] = buys.iter().map(|curve| curve[position]).sum();
        supply[position] = sells.iter().map(|curve| curve[position]).sum();

        for (side, quantities, claimed_amount) in [
            (0_u8, buys.map(|curve| curve[position]), demand[position]),
            (1_u8, sells.map(|curve| curve[position]), supply[position]),
        ] {
            let ciphertext = aggregate_level(&setup.public.encryption_pubkey, &quantities);
            let ciphertext_bytes = ciphertext.to_bytes();
            let contexts = setup.secret_shares[..3]
                .iter()
                .map(|secret_share| {
                    DecryptionProofContext {
                        program_id,
                        market: MARKET,
                        key_epoch_address: KEY_EPOCH,
                        epoch: EPOCH,
                        auction_id: AUCTION_ID,
                        side,
                        position: position as u16,
                        participant_index: secret_share.index() as u8,
                        commitment: ciphertext_bytes[..32]
                            .try_into()
                            .expect("ciphertext commitment is fixed-width"),
                    }
                    .to_bytes()
                })
                .collect::<Vec<_>>();
            let shares = setup.secret_shares[..3]
                .iter()
                .zip(&contexts)
                .map(|(secret_share, context)| {
                    create_decryption_share(secret_share, &ciphertext, context)
                })
                .collect::<Vec<_>>();
            let context_refs = contexts
                .iter()
                .map(|context| context.as_slice())
                .collect::<Vec<_>>();
            verify_plaintext_with_share_contexts(
                &setup.public,
                &ciphertext,
                &shares,
                claimed_amount,
                &context_refs,
            )
            .expect("three valid committee shares reveal the aggregate only");
        }
    }

    let result = clear_auction(&grid, &demand, &supply).expect("aggregate curves are valid");
    println!("BeatEx encrypted-auction simulation");
    println!("traders: {}", buys.len());
    println!("public demand after settlement: {demand:?}");
    println!("public supply after settlement: {supply:?}");
    println!(
        "clearing index: {}, price: {}, volume: {}",
        result.index, result.price, result.executable_volume
    );
}
