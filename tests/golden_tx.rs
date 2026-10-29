#![allow(deprecated)] // solana_entry::Entry is flagged unstable upstream; this test pins its wire format.
//! Byte-for-byte checks of the transactions the SDKs build. The expected values were
//! captured before the Solana SDK upgrade, so a dependency bump that silently changes an
//! instruction, an account order or a signature fails here instead of on mainnet.
use shred_pool_cache::config::keypair_from_base58;
use shred_pool_cache::pump::accounts::BondingCurve;
use shred_pool_cache::pump::ix::Market;
use shred_pool_cache::pump::parse::Quote;
use shred_pool_cache::pump::{TOKEN_2022_PROGRAM, WSOL_MINT};
use shred_pool_cache::pumpfun::pumpfun::{buy_transaction, sell_transaction};
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
    "01058c1ee5d6622beecffd38e04cc1807cab1e6b416a512394982dd88fd67879390189a9e85e968efb4275cecd1ffabb",
    "dd9c65315713483922c6f9824c9b9fed0d8001000c15e774d25fb8c18c52c5ba4f402bb036d5fe9d14f49779d00a0745",
    "f91f48e9d967040404040404040404040404040404040404040404040404040404040404040408adb678c919610969d7",
    "c67487ac70cdb9979f4dbd2bca863e42d0b8c70f090c1e3b71748f5d1a253f27b7bc0899715e699ff69ae97db0bf0a5e",
    "242c7d399023227ae91af62cdb842aa7dd3c2f337181e19cdf80a1327a1dc47a6783f9afed0336f170dab8acc29c29cc",
    "8cc26e9060e5076a4e1eef385d301fbdc5c84dcdf01740ad66e77bd26d45b8f48f026053b84c13df48ef847e818acb4a",
    "0ffa479347e1a87f29f996a18621f27e1b18b2cd4a9d91638bb86b8269b4fd46eeb0c4266feeccc9fa9c497fbbda9d3b",
    "2b4abe0e1f2776219de0652d6445809b767e0aaf68040000000000000000000000000000000000000000000000000000",
    "00000000000001010101010101010101010101010101010101010101010101010101010101010156e0f693665acf44db",
    "1568bf175baa5189cb97f5d2ff3b655d2bb6fd6d18b00306466fe5211732ffecadba72c39be7bc8ce5bbc5f7126b2c43",
    "9b3a40000000069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f0000000000106ddf6e1d765a193d9cb",
    "e146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a906ddf6e1ee758fde18425dbce46ccddab61afc4d83b90d27febd",
    "f928d8a18bfc0af1c3432188ca3a635135a13a18951acebd29e6ac2db1c142d4dd2f491fa4a63a865e69ee0f5480cabc",
    "f66357e4dc2f18d58d45c1ea7489fb3723d9793c72a66f9ab4a4f1958dc0a9c94c3fb72c07995843eda485e3a24f10c6",
    "9399f819940f8c97258f4e2489f1bb3d1029148e0d830b5a1399daff1084048e7bd8dbe9f859acf136eb01fc1c4e883d",
    "23c8b5844ab59a37f66add57c5e9ac3b53e059d35c640707070707070707070707070707070707070707070707070707",
    "070707070707050c01100502f04902000c00090340420f000000000013060005000a090f01010b11110a0d0f0e040807",
    "00050603120109140b19e1f7501ed5b384888096980000000000405489000000000000090200020c0200000040420f00",
    "0000000000",
);
const PF_SELL: &str = concat!(
    "0181b0707c888ec0dd35997fb8de2c88062a436ba13057693e72718f00930df65308c4063f261bd0baa23186bc34d627",
    "c00b043c38d8e7cd5b71423bf25b44a1048001000b14e774d25fb8c18c52c5ba4f402bb036d5fe9d14f49779d00a0745",
    "f91f48e9d967040404040404040404040404040404040404040404040404040404040404040408adb678c919610969d7",
    "c67487ac70cdb9979f4dbd2bca863e42d0b8c70f090c1e3b71748f5d1a253f27b7bc0899715e699ff69ae97db0bf0a5e",
    "242c7d399023227ae91af62cdb842aa7dd3c2f337181e19cdf80a1327a1dc47a6783f9afed0336f170dab8acc29c29cc",
    "8cc26e9060e5076a4e1eef385d301fbdc5c84dcdf01740ad66e77bd26d45b8f48f026053b84c13df48ef847e818acb4a",
    "0ffa479347e1a87f29f996a18621f27e1b18b2cd4a9d91638bb86b8269b4fd46eeb0c4266feeccc9fa9c497fbbda9d3b",
    "2b4abe0e1f2776219de0652d6445809b767e0aaf68040000000000000000000000000000000000000000000000000000",
    "00000000000001010101010101010101010101010101010101010101010101010101010101010156e0f693665acf44db",
    "1568bf175baa5189cb97f5d2ff3b655d2bb6fd6d18b00306466fe5211732ffecadba72c39be7bc8ce5bbc5f7126b2c43",
    "9b3a40000000069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f0000000000106ddf6e1d765a193d9cb",
    "e146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a906ddf6e1ee758fde18425dbce46ccddab61afc4d83b90d27febd",
    "f928d8a18bfc0af1c3432188ca3a635135a13a18951acebd29e6ac2db1c142d4dd2f491fa4a63a865e69ee0f5480cabc",
    "f66357e4dc2f18d58d45c1ea7489fb3723d9793c72a66f9ab4a4f1958dc0a9c94c3fb72c07995843eda485e3a24f10c6",
    "9399f819940facf136eb01fc1c4e883d23c8b5844ab59a37f66add57c5e9ac3b53e059d35c6407070707070707070707",
    "07070707070707070707070707070707070707070707050c01100502f04902000c00090340420f000000000009020002",
    "0c0200000040420f00000000000b11110a0d0f0e04080700050603120109130b181c92de7726c469d5404b4c00000000",
    "0000000000000000000f03050000010900",
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

// PumpSwap still uses its original estimate.
const EXPECTED_TOKENS: u64 = 20000000000000;

const BLOCKHASH: [u8; 32] = [7; 32];

/// A SOL-paired Token-2022 token, the kind Pump.fun launches today.
fn market() -> Market {
    Market {
        mint: key(1),
        curve: BondingCurve {
            virtual_token_reserves: 1_000_000_000_000_000,
            virtual_quote_reserves: 30_000_000_000,
            real_token_reserves: 800_000_000_000_000,
            real_quote_reserves: 0,
            token_total_supply: 1_000_000_000_000_000,
            complete: false,
            creator: key(2),
            quote: Quote::Sol,
            quote_mint: WSOL_MINT,
        },
        base_token_program: TOKEN_2022_PROGRAM,
        quote_token_program: shred_pool_cache::pump::ix::TOKEN_PROGRAM,
        buyback_recipient: key(4),
    }
}

fn pumpfun_buy() -> solana_sdk::transaction::VersionedTransaction {
    buy_transaction(
        &market(),
        &fixed_keypair(),
        Hash::new_from_array(BLOCKHASH),
        10_000_000,
        9_000_000,
        1_000_000,
    )
    .unwrap()
}

#[tokio::test]
async fn pumpfun_buy_and_sell_are_stable() {
    let buy = pumpfun_buy();
    let sell = sell_transaction(
        &market(),
        &fixed_keypair(),
        Hash::new_from_array(BLOCKHASH),
        5_000_000,
        1_000_000,
        true,
    )
    .unwrap();

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

    let tx = pumpfun_buy();

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
    "010000000000000001058c1ee5d6622beecffd38e04cc1807cab1e6b416a512394982dd88fd67879390189a9e85e968e",
    "fb4275cecd1ffabbdd9c65315713483922c6f9824c9b9fed0d8001000c15e774d25fb8c18c52c5ba4f402bb036d5fe9d",
    "14f49779d00a0745f91f48e9d967040404040404040404040404040404040404040404040404040404040404040408ad",
    "b678c919610969d7c67487ac70cdb9979f4dbd2bca863e42d0b8c70f090c1e3b71748f5d1a253f27b7bc0899715e699f",
    "f69ae97db0bf0a5e242c7d399023227ae91af62cdb842aa7dd3c2f337181e19cdf80a1327a1dc47a6783f9afed0336f1",
    "70dab8acc29c29cc8cc26e9060e5076a4e1eef385d301fbdc5c84dcdf01740ad66e77bd26d45b8f48f026053b84c13df",
    "48ef847e818acb4a0ffa479347e1a87f29f996a18621f27e1b18b2cd4a9d91638bb86b8269b4fd46eeb0c4266feeccc9",
    "fa9c497fbbda9d3b2b4abe0e1f2776219de0652d6445809b767e0aaf6804000000000000000000000000000000000000",
    "000000000000000000000000000001010101010101010101010101010101010101010101010101010101010101010156",
    "e0f693665acf44db1568bf175baa5189cb97f5d2ff3b655d2bb6fd6d18b00306466fe5211732ffecadba72c39be7bc8c",
    "e5bbc5f7126b2c439b3a40000000069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f0000000000106dd",
    "f6e1d765a193d9cbe146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a906ddf6e1ee758fde18425dbce46ccddab61a",
    "fc4d83b90d27febdf928d8a18bfc0af1c3432188ca3a635135a13a18951acebd29e6ac2db1c142d4dd2f491fa4a63a86",
    "5e69ee0f5480cabcf66357e4dc2f18d58d45c1ea7489fb3723d9793c72a66f9ab4a4f1958dc0a9c94c3fb72c07995843",
    "eda485e3a24f10c69399f819940f8c97258f4e2489f1bb3d1029148e0d830b5a1399daff1084048e7bd8dbe9f859acf1",
    "36eb01fc1c4e883d23c8b5844ab59a37f66add57c5e9ac3b53e059d35c64070707070707070707070707070707070707",
    "0707070707070707070707070707050c01100502f04902000c00090340420f000000000013060005000a090f01010b11",
    "110a0d0f0e04080700050603120109140b19e1f7501ed5b384888096980000000000405489000000000000090200020c",
    "0200000040420f000000000000",
);
