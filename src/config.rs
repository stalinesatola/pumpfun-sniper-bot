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

/// Decode a base58 secret key without panicking (`Keypair::from_base58_string` panics).
pub fn keypair_from_base58(secret: &str) -> Result<Keypair> {
    let bytes = bs58::decode(secret.trim())
        .into_vec()
        .context("PVT_KEY is not valid base58")?;
    Keypair::try_from(bytes.as_slice())
        .map_err(|e| anyhow!("PVT_KEY is not a valid 64-byte keypair: {e}"))
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
