use std::env;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use solana_sdk::signature::Keypair;

/// Runtime configuration, read once from the environment (`.env` is loaded by the caller).
pub struct Config {
    pub rpc_url: String,
    /// Fast transaction sender (Astralane). Without it transactions go through `rpc_url`
    /// and no landing tip is paid.
    pub astralane_url: Option<String>,
    /// Where new activity comes from.
    pub feed: Feed,
    /// WebSocket of the RPC, for the logs feed.
    pub ws_url: String,
    pub shredstream_url: Option<String>,
    /// Sent as the `x-token` header to hosted shred feeds that need it (`SHREDSTREAM_TOKEN`).
    pub shredstream_token: Option<String>,
    pub payer: Keypair,
    pub buy_amount_sol: f64,
    pub bribe_fee: f64,
    pub sell_delay_slots: u64,
}

/// Where the bot reads Pump.fun activity from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feed {
    /// A gRPC shred feed (`SHREDSTREAM_URL`): the fastest.
    Shredstream,
    /// The RPC's `logsSubscribe` WebSocket: needs only an RPC, about a second slower.
    RpcLogs,
}

/// The WebSocket address of an RPC endpoint: the same host with `ws`/`wss`.
pub fn ws_from_rpc(rpc_url: &str) -> Result<String> {
    let url = rpc_url.trim();
    if let Some(rest) = url.strip_prefix("https://") {
        Ok(format!("wss://{rest}"))
    } else if let Some(rest) = url.strip_prefix("http://") {
        Ok(format!("ws://{rest}"))
    } else {
        bail!("RPC_URL must start with http:// or https://, got {url:?}")
    }
}

fn optional(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn required(name: &str) -> Result<String> {
    match env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => bail!("missing required environment variable {name}"),
    }
}

pub fn optional_parsed<T: FromStr>(name: &str, default: T) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse::<T>()
            .map_err(|e| anyhow!("invalid value for {name}: {e}")),
        _ => Ok(default),
    }
}

pub fn positive_sol(name: &str, value: f64) -> Result<f64> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        bail!("{name} must be a positive number of SOL, got {value}")
    }
}

/// Reads the wallet key from what a person pastes into `.env`. Accepted: the base58 secret key
/// (64 bytes, as wallets export it) or the JSON array of numbers that `solana-keygen` writes. Quotes and spaces around it are ignored. Never panics, and the
/// messages never contain the key.
pub fn keypair_from_base58(secret: &str) -> Result<Keypair> {
    let text = secret.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    let bytes: Vec<u8> = if text.starts_with('[') {
        serde_json::from_str(text).map_err(|_| {
            anyhow!("PVT_KEY looks like a JSON array but is not a list of 64 numbers (0-255)")
        })?
    } else {
        bs58::decode(text).into_vec().map_err(|_| {
            anyhow!(
                "PVT_KEY is not base58: paste the private key exactly as the wallet shows it, on one line, \
                 without quotes or spaces (a seed phrase of words does not work)"
            )
        })?
    };
    match bytes.len() {
        64 => Keypair::try_from(bytes.as_slice()).map_err(|_| {
            anyhow!(
                "PVT_KEY has 64 bytes but its public half does not match its secret half: the key was \
                 probably copied with a character missing or changed. Copy it again, or make a new \
                 wallet (`cargo run --example newkey`)"
            )
        }),
        32 => bail!(
            "PVT_KEY has only 32 bytes: that is a wallet address (public key) or a seed, and neither can sign. \
             Use the private key (64 bytes, about 87-88 base58 characters)"
        ),
        n => bail!(
            "PVT_KEY decodes to {n} bytes; a wallet secret key has 64 (about 87-88 base58 characters). \
             Did you paste the wallet address or part of the key?"
        ),
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let payer = keypair_from_base58(&required("PVT_KEY")?)?;

        let buy_amount_sol = positive_sol(
            "SOL_BUY_AMOUNT",
            optional_parsed("SOL_BUY_AMOUNT", 0.0001_f64)?,
        )?;
        let astralane_url = optional("ASTRALANE_URL");
        // The landing tip only makes sense with a sender that uses it.
        let bribe_fee = if astralane_url.is_some() {
            positive_sol("BRIBE_FEE", optional_parsed("BRIBE_FEE", 0.0001_f64)?)?
        } else {
            0.0
        };

        let sell_delay_slots: u64 = optional_parsed("SELL_DELAY_SLOTS", 7)?;
        if sell_delay_slots == 0 {
            bail!("SELL_DELAY_SLOTS must be at least 1");
        }

        let rpc_url = required("RPC_URL")?;
        let shredstream_url = optional("SHREDSTREAM_URL");
        let feed = match optional("FEED").map(|f| f.to_ascii_lowercase()).as_deref() {
            None if shredstream_url.is_some() => Feed::Shredstream,
            None | Some("rpc") => Feed::RpcLogs,
            Some("shredstream") => Feed::Shredstream,
            Some(other) => bail!("FEED must be \"shredstream\" or \"rpc\", got {other:?}"),
        };
        if feed == Feed::Shredstream && shredstream_url.is_none() {
            bail!("FEED=shredstream needs SHREDSTREAM_URL");
        }
        let ws_url = match optional("WS_URL") {
            Some(u) => u,
            None => ws_from_rpc(&rpc_url)?,
        };

        Ok(Self {
            rpc_url,
            astralane_url,
            feed,
            ws_url,
            shredstream_url,
            shredstream_token: optional("SHREDSTREAM_TOKEN"),
            payer,
            buy_amount_sol,
            bribe_fee,
            sell_delay_slots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signer::Signer;

    #[test]
    fn keypair_roundtrip_and_errors_do_not_panic() {
        let kp = Keypair::new();
        let encoded = bs58::encode(kp.to_bytes()).into_string();
        assert_eq!(keypair_from_base58(&encoded).unwrap().pubkey(), kp.pubkey());

        assert!(keypair_from_base58("").is_err());
        assert!(keypair_from_base58("not base58 !!!").is_err());
        assert!(keypair_from_base58("abc").is_err());
    }

    #[test]
    fn accepted_formats_all_give_the_same_wallet() {
        let kp = Keypair::new();
        let b58 = bs58::encode(kp.to_bytes()).into_string();
        let json = serde_json::to_string(&kp.to_bytes().to_vec()).unwrap();
        for text in [
            b58.clone(),
            format!("  {b58}  "),
            format!("\"{b58}\""),
            format!("'{b58}'"),
            json,
        ] {
            assert_eq!(keypair_from_base58(&text).unwrap().pubkey(), kp.pubkey());
        }
        // A wallet address (32 bytes) is refused, not turned into a different wallet.
        let address = kp.pubkey().to_string();
        let err = keypair_from_base58(&address).unwrap_err().to_string();
        assert!(err.contains("32 bytes") && err.contains("address"), "{err}");
    }

    #[test]
    fn each_mistake_gets_its_own_message_and_never_echoes_the_key() {
        let kp = Keypair::new();
        let mut bytes = kp.to_bytes();
        bytes[40] ^= 0xff; // a changed byte in the public half
        let mismatched = bs58::encode(bytes).into_string();
        let err = keypair_from_base58(&mismatched).unwrap_err().to_string();
        assert!(err.contains("does not match"), "{err}");
        assert!(!err.contains(&mismatched));

        let wrong_len = bs58::encode([1u8; 40]).into_string();
        let err = keypair_from_base58(&wrong_len).unwrap_err().to_string();
        assert!(err.contains("40 bytes"), "{err}");

        let words = "word another word";
        assert!(keypair_from_base58(words)
            .unwrap_err()
            .to_string()
            .contains("not base58"));
        assert!(keypair_from_base58("[1,2,3]")
            .unwrap_err()
            .to_string()
            .contains("3 bytes"));
        assert!(keypair_from_base58("[300, \"x\"]")
            .unwrap_err()
            .to_string()
            .contains("JSON array"));
        assert!(keypair_from_base58("   ").is_err());
    }

    #[test]
    fn the_websocket_is_the_rpc_host_with_ws_or_wss() {
        assert_eq!(
            ws_from_rpc("https://mainnet.helius-rpc.com/?api-key=k").unwrap(),
            "wss://mainnet.helius-rpc.com/?api-key=k"
        );
        assert_eq!(
            ws_from_rpc(" http://127.0.0.1:8899 ").unwrap(),
            "ws://127.0.0.1:8899"
        );
        assert!(ws_from_rpc("127.0.0.1:8899").is_err());
    }

    #[test]
    fn sol_amounts_must_be_positive_and_finite() {
        assert!(positive_sol("X", 0.001).is_ok());
        assert!(positive_sol("X", 0.0).is_err());
        assert!(positive_sol("X", -1.0).is_err());
        assert!(positive_sol("X", f64::NAN).is_err());
    }
}
