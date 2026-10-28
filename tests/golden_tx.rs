#![allow(deprecated)] // solana_entry::Entry is flagged unstable upstream; this test pins its wire format.
//! Byte-for-byte checks of the transactions the SDKs build. The expected values were
//! captured before the Solana SDK upgrade, so a dependency bump that silently changes an
//! instruction, an account order or a signature fails here instead of on mainnet.
use shred_pool_cache::config::keypair_from_base58;
use shred_pool_cache::pumpfun::pumpfun::{PumpFunBuyType, PumpFunSellType};
use shred_pool_cache::pumpfun::PumpFunSDK;
use shred_pool_cache::pumpswap::constants::PriorityFeePump;
use shred_pool_cache::pumpswap::pumpswap::{PumpSwapBuyType, PumpSwapSellType};
use shred_pool_cache::pumpswap::PumpSwapSDK;
use shred_pool_cache::types::BuyAmounts;
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;

fn fixed_keypair() -> Keypair {
    let secret = bs58::encode(FIXED_SECRET).into_string();
    keypair_from_base58(&secret).expect("fixed test key")
}

// A valid ed25519 keypair (secret || public), generated once and used only in tests.
const FIXED_SECRET: [u8; 64] = [
    9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 9, 8,
    231, 116, 210, 95, 184, 193, 140, 82, 197, 186, 79, 64, 43, 176, 54, 213, 254, 157, 20, 244,
    151, 121, 208, 10, 7, 69, 249, 31, 72, 233, 217, 103,
];

fn key(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fee() -> PriorityFeePump {
    PriorityFeePump {
        unit_limit: 100_000,
        unit_price: 25_000_000,
        buy_tip_fee: 0.001,
        sell_tip_fee: 0.001,
        jito_tip_fee: 0.002,
    }
}

fn amounts() -> BuyAmounts {
    BuyAmounts {
        base_amount_out: 1_000_000_000_000,
        max_quote_amount_in: 2_000_000_000,
    }
}

const PF_BUY: &str = concat!(
    "0122fef1f62e76c5f95623435538e1701437409f13afa68a56cc9b129adde19ea04554cc3ac7bbafa4a351e58e3d8298",
    "a82ce5fb1888cddbc60b7259c2a6e56d098001000910e774d25fb8c18c52c5ba4f402bb036d5fe9d14f49779d00a0745",
    "f91f48e9d967020202020202020202020202020202020202020202020202020202020202020208adb678c919610969d7",
    "c67487ac70cdb9979f4dbd2bca863e42d0b8c70f090c227ae91af62cdb842aa7dd3c2f337181e19cdf80a1327a1dc47a",
    "6783f9afed037d55c9e0a4b2a0b45e410175a19e1ac18acddb576d0a5015c15fd43c85e2d2dead11e6a4fc2944a4fa82",
    "51bef815426e1bfb28c6b6646677607c6ad9f566a646d78d29e0c265e5ba9381973768ee2510661118ea51861fb875a2",
    "b2ceb8f210e4000000000000000000000000000000000000000000000000000000000000000001010101010101010101",
    "010101010101010101010101010101010101010101010156e0f693665acf44db1568bf175baa5189cb97f5d2ff3b655d",
    "2bb6fd6d18b00306466fe5211732ffecadba72c39be7bc8ce5bbc5f7126b2c439b3a4000000006ddf6e1d765a193d9cb",
    "e146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a90af1c3432188ca3a635135a13a18951acebd29e6ac2db1c142d4",
    "dd2f491fa4a63a865e69ee0f5480cabcf66357e4dc2f18d58d45c1ea7489fb3723d9793c72a68c97258f4e2489f1bb3d",
    "1029148e0d830b5a1399daff1084048e7bd8dbe9f859acf136eb01fc1c4e883d23c8b5844ab59a37f66add57c5e9ac3b",
    "53e059d35c640707070707070707070707070707070707070707070707070707070707070707050a010c0502f0490200",
    "0a00090340420f00000000000e0600040008070b0100090c0d050803060400070b010f091866063d1201daebea0040e5",
    "9c30120000a037a00000000000070200020c0200000080841e000000000000",
);

const PF_SELL: &str = concat!(
    "017a063e03c17f2664a3ca48bccea42114eeb82009fd65498c0cf9b6f107356855ba689a889e702e984fc85a4d0d7a2e",
    "612e2b40f60d3017c01a4b1b8253984804800100070ee774d25fb8c18c52c5ba4f402bb036d5fe9d14f49779d00a0745",
    "f91f48e9d967020202020202020202020202020202020202020202020202020202020202020208adb678c919610969d7",
    "c67487ac70cdb9979f4dbd2bca863e42d0b8c70f090c227ae91af62cdb842aa7dd3c2f337181e19cdf80a1327a1dc47a",
    "6783f9afed037d55c9e0a4b2a0b45e410175a19e1ac18acddb576d0a5015c15fd43c85e2d2dead11e6a4fc2944a4fa82",
    "51bef815426e1bfb28c6b6646677607c6ad9f566a646d78d29e0c265e5ba9381973768ee2510661118ea51861fb875a2",
    "b2ceb8f210e4000000000000000000000000000000000000000000000000000000000000000001010101010101010101",
    "010101010101010101010101010101010101010101010156e0f693665acf44db1568bf175baa5189cb97f5d2ff3b655d",
    "2bb6fd6d18b00306466fe5211732ffecadba72c39be7bc8ce5bbc5f7126b2c439b3a4000000006ddf6e1d765a193d9cb",
    "e146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a93a865e69ee0f5480cabcf66357e4dc2f18d58d45c1ea7489fb37",
    "23d9793c72a6acf136eb01fc1c4e883d23c8b5844ab59a37f66add57c5e9ac3b53e059d35c6407070707070707070707",
    "07070707070707070707070707070707070707070707050a000502f04902000a00090340420f0000000000070200020c",
    "0200000080841e0000000000090c0c05080306040007010b0d091833e685a4017f83ad404b4c00000000000000000000",
    "0000000b03040000010900",
);

const PS_BUY: &str = concat!(
    "012f9410513b9fe219e0ca48b51f3061004e4bced8472542b5928db03f86292f10309ae1eb81fe2f487797ab62781140",
    "e277663fafd8a6b776bb34964fe3baa50c01000d15e774d25fb8c18c52c5ba4f402bb036d5fe9d14f49779d00a0745f9",
    "1f48e9d96708adb678c919610969d7c67487ac70cdb9979f4dbd2bca863e42d0b8c70f090c3734217cae129bf5c70fd5",
    "68412210dc7a43d6dae3d70958113c576ca44ee41740ad66e77bd26d45b8f48f026053b84c13df48ef847e818acb4a0f",
    "fa479347e1459d45697315299f98e1c28a489be27316852c8e669ec15ec287323cf21969f577d915955f8880731ceb4a",
    "75a0cc96c174fa4095c4e1d9967acfc42845ae67ae7d55c9e0a4b2a0b45e410175a19e1ac18acddb576d0a5015c15fd4",
    "3c85e2d2de9905aa1d53ff9a293fdb1a560c2ea3d3cd86867c89a4bbfc95cff71577ab79600000000000000000000000",
    "000000000000000000000000000000000000000000010101010101010101010101010101010101010101010101010101",
    "010101010103030303030303030303030303030303030303030303030303030303030303030306466fe5211732ffecad",
    "ba72c39be7bc8ce5bbc5f7126b2c439b3a40000000069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f0",
    "000000000106ddf6e1d765a193d9cbe146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a90af1c3432188ca3a635135",
    "a13a18951acebd29e6ac2db1c142d4dd2f491fa4a60c14defc825ec67694250818bb654065f4298d3156d571b4d4f809",
    "0c18e9a8634ac2f8d0dd5cbc97e3289c197cb5062a54f3d956b9ce6e5115f96567aa5cb3e671b95fc8c6bae22deb7244",
    "c2a8ac4ce56e72cfe165cd8577e975613e90b3124d890ba644fe1f55aa19f11cd2d2ec14d3233b6e0a4beaeef72b6985",
    "8e21e170d68c97258f4e2489f1bb3d1029148e0d830b5a1399daff1084048e7bd8dbe9f859e54a709528839f61c0b9b8",
    "6079891c139216e47a71b62fb73bec72169458745e070707070707070707070707070707070707070707070707070707",
    "0707070707090b010e0502f04902000b00090340420f0000000000080200010c0200000080841e000000000013060003",
    "000c080d0101080200030c0200000080969800000000000d01030111130600060009080d01010f130a0012090c060302",
    "0710050d0d0813140f04111866063d1201daebea0040e59c30120000a037a000000000000d030300000109",
);

const PS_SELL: &str = concat!(
    "014f2e9375086425484c7f2ab7636783912d51dedd1b385b04b14e1f25a844703a2e2bdd94fc8eefcb52c207b6456b3c",
    "8603ff4ac10b28ac45f5a863d8a23465068001000d15e774d25fb8c18c52c5ba4f402bb036d5fe9d14f49779d00a0745",
    "f91f48e9d96708adb678c919610969d7c67487ac70cdb9979f4dbd2bca863e42d0b8c70f090c3734217cae129bf5c70f",
    "d568412210dc7a43d6dae3d70958113c576ca44ee41740ad66e77bd26d45b8f48f026053b84c13df48ef847e818acb4a",
    "0ffa479347e1459d45697315299f98e1c28a489be27316852c8e669ec15ec287323cf21969f577d915955f8880731ceb",
    "4a75a0cc96c174fa4095c4e1d9967acfc42845ae67ae7d55c9e0a4b2a0b45e410175a19e1ac18acddb576d0a5015c15f",
    "d43c85e2d2de9905aa1d53ff9a293fdb1a560c2ea3d3cd86867c89a4bbfc95cff71577ab796000000000000000000000",
    "000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101",
    "01010101010103030303030303030303030303030303030303030303030303030303030303030306466fe5211732ffec",
    "adba72c39be7bc8ce5bbc5f7126b2c439b3a40000000069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0",
    "f0000000000106ddf6e1d765a193d9cbe146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a90af1c3432188ca3a6351",
    "35a13a18951acebd29e6ac2db1c142d4dd2f491fa4a60c14defc825ec67694250818bb654065f4298d3156d571b4d4f8",
    "090c18e9a8634ac2f8d0dd5cbc97e3289c197cb5062a54f3d956b9ce6e5115f96567aa5cb3e671b95fc8c6bae22deb72",
    "44c2a8ac4ce56e72cfe165cd8577e975613e90b3124d890ba644fe1f55aa19f11cd2d2ec14d3233b6e0a4beaeef72b69",
    "858e21e170d68c97258f4e2489f1bb3d1029148e0d830b5a1399daff1084048e7bd8dbe9f859e54a709528839f61c0b9",
    "b86079891c139216e47a71b62fb73bec72169458745e0707070707070707070707070707070707070707070707070707",
    "070707070707080b010e0502f04902000b00090340420f0000000000080200010c0200000080841e0000000000130600",
    "03000c080d01010d01030111130600060009080d01010f130a0012090c0603020710050d0d0813140f04111833e685a4",
    "017f83ad404b4c000000000000000000000000000d03030000010900",
);

const EXPECTED_TOKENS: u64 = 20000000000000;

const BLOCKHASH: [u8; 32] = [7; 32];

#[tokio::test]
async fn pumpfun_buy_and_sell_are_stable() {
    let sdk = PumpFunSDK::new("http://127.0.0.1:1", "http://127.0.0.1:1", fee()).await;

    let (buy, tokens) = sdk
        .create_buy_tx(PumpFunBuyType {
            mint: key(1),
            sol_to_buy: 0.01,
            user_keypair: fixed_keypair(),
            blockhash: Hash::new_from_array(BLOCKHASH),
            buy_amounts: amounts(),
            slippage: 5,
            cu_price: 1,
            creator: key(2),
        })
        .await
        .unwrap();
    let sell = sdk
        .create_sell_exact_amount_tx(PumpFunSellType {
            mint: key(1),
            token_amount_lamports: 5_000_000,
            user_keypair: fixed_keypair(),
            blockhash: Hash::new_from_array(BLOCKHASH),
            slippage: 0,
            cu_price: 1,
            creator: key(2),
        })
        .await
        .unwrap();

    assert_eq!(tokens, EXPECTED_TOKENS);
    assert_eq!(
        hex(&bincode::serialize(&buy).unwrap()),
        PF_BUY,
        "buy tx changed"
    );
    assert_eq!(
        hex(&bincode::serialize(&sell).unwrap()),
        PF_SELL,
        "sell tx changed"
    );
}

#[tokio::test]
async fn pumpswap_buy_and_sell_are_stable() {
    let sdk = PumpSwapSDK::new("http://127.0.0.1:1", "http://127.0.0.1:1", fee()).await;

    let (buy, tokens) = sdk
        .create_buy_tx(PumpSwapBuyType {
            mint: key(1),
            sol_to_buy: 0.01,
            user_keypair: fixed_keypair(),
            blockhash: Hash::new_from_array(BLOCKHASH),
            pool: key(3),
            buy_amounts: amounts(),
            slippage: 5,
            cu_price: 1,
            creator_pubkey: key(2),
        })
        .await
        .unwrap();
    let sell = sdk
        .create_sell_exact_amount_tx(PumpSwapSellType {
            mint: key(1),
            token_amount_lamports: 5_000_000,
            user_keypair: fixed_keypair(),
            blockhash: Hash::new_from_array(BLOCKHASH),
            pool: key(3),
            slippage: 0,
            cu_price: 1,
            creator_pubkey: key(2),
        })
        .await
        .unwrap();

    assert_eq!(tokens, EXPECTED_TOKENS);
    assert_eq!(
        hex(&bincode::serialize(&buy).unwrap()),
        PS_BUY,
        "buy tx changed"
    );
    assert_eq!(
        hex(&bincode::serialize(&sell).unwrap()),
        PS_SELL,
        "sell tx changed"
    );
}

/// The shredstream delivers `Vec<Entry>` in bincode; this is what `run_stream` decodes.
#[tokio::test]
async fn shredstream_entries_decode_the_same() {
    use solana_entry::entry::Entry;

    let sdk = PumpFunSDK::new("http://127.0.0.1:1", "http://127.0.0.1:1", fee()).await;
    let (tx, _) = sdk
        .create_buy_tx(PumpFunBuyType {
            mint: key(1),
            sol_to_buy: 0.01,
            user_keypair: fixed_keypair(),
            blockhash: Hash::new_from_array(BLOCKHASH),
            buy_amounts: amounts(),
            slippage: 5,
            cu_price: 1,
            creator: key(2),
        })
        .await
        .unwrap();

    let entries = vec![Entry {
        num_hashes: 3,
        hash: Hash::new_from_array([5; 32]),
        transactions: vec![tx],
    }];
    let bytes = bincode::serialize(&entries).unwrap();
    assert_eq!(hex(&bytes), ENTRIES, "entry encoding changed");

    // And the reverse, which is what the bot does with bytes from the network.
    let wire: Vec<u8> = (0..ENTRIES.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&ENTRIES[i..i + 2], 16).unwrap())
        .collect();
    let decoded: Vec<Entry> = bincode::deserialize(&wire).unwrap();
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].num_hashes, 3);
    assert_eq!(decoded[0].transactions.len(), 1);
    assert_eq!(bincode::serialize(&decoded).unwrap(), wire);
}

const ENTRIES: &str = concat!(
    "010000000000000003000000000000000505050505050505050505050505050505050505050505050505050505050505",
    "01000000000000000122fef1f62e76c5f95623435538e1701437409f13afa68a56cc9b129adde19ea04554cc3ac7bbaf",
    "a4a351e58e3d8298a82ce5fb1888cddbc60b7259c2a6e56d098001000910e774d25fb8c18c52c5ba4f402bb036d5fe9d",
    "14f49779d00a0745f91f48e9d967020202020202020202020202020202020202020202020202020202020202020208ad",
    "b678c919610969d7c67487ac70cdb9979f4dbd2bca863e42d0b8c70f090c227ae91af62cdb842aa7dd3c2f337181e19c",
    "df80a1327a1dc47a6783f9afed037d55c9e0a4b2a0b45e410175a19e1ac18acddb576d0a5015c15fd43c85e2d2dead11",
    "e6a4fc2944a4fa8251bef815426e1bfb28c6b6646677607c6ad9f566a646d78d29e0c265e5ba9381973768ee25106611",
    "18ea51861fb875a2b2ceb8f210e400000000000000000000000000000000000000000000000000000000000000000101",
    "0101010101010101010101010101010101010101010101010101010101010156e0f693665acf44db1568bf175baa5189",
    "cb97f5d2ff3b655d2bb6fd6d18b00306466fe5211732ffecadba72c39be7bc8ce5bbc5f7126b2c439b3a4000000006dd",
    "f6e1d765a193d9cbe146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a90af1c3432188ca3a635135a13a18951acebd",
    "29e6ac2db1c142d4dd2f491fa4a63a865e69ee0f5480cabcf66357e4dc2f18d58d45c1ea7489fb3723d9793c72a68c97",
    "258f4e2489f1bb3d1029148e0d830b5a1399daff1084048e7bd8dbe9f859acf136eb01fc1c4e883d23c8b5844ab59a37",
    "f66add57c5e9ac3b53e059d35c640707070707070707070707070707070707070707070707070707070707070707050a",
    "010c0502f04902000a00090340420f00000000000e0600040008070b0100090c0d050803060400070b010f091866063d",
    "1201daebea0040e59c30120000a037a00000000000070200020c0200000080841e000000000000",
);
