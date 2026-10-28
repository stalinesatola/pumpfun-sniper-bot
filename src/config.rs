use std::env;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use solana_sdk::signature::Keypair;

/// Runtime configuration, read once from the environment (`.env` is loaded by the caller).
pub struct Config {
    pub rpc_url: String,
    pub astralane_url: String,
    pub shredstream_url: String,
    pub payer: Keypair,
    pub buy_amount_sol: f64,
    pub bribe_fee: f64,
    pub sell_delay_slots: u64,
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
#[allow(deprecated)]
pub fn keypair_from_base58(secret: &str) -> Result<Keypair> {
    let bytes = bs58::decode(secret.trim())
        .into_vec()
        .context("PVT_KEY is not valid base58")?;
    Keypair::from_bytes(&bytes).map_err(|e| anyhow!("PVT_KEY is not a valid 64-byte keypair: {e}"))
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let payer = keypair_from_base58(&required("PVT_KEY")?)?;

        let buy_amount_sol = positive_sol(
            "SOL_BUY_AMOUNT",
            optional_parsed("SOL_BUY_AMOUNT", 0.0001_f64)?,
        )?;
        let bribe_fee = positive_sol("BRIBE_FEE", optional_parsed("BRIBE_FEE", 0.0001_f64)?)?;

        let sell_delay_slots: u64 = optional_parsed("SELL_DELAY_SLOTS", 7)?;
        if sell_delay_slots == 0 {
            bail!("SELL_DELAY_SLOTS must be at least 1");
        }

        Ok(Self {
            rpc_url: required("RPC_URL")?,
            astralane_url: required("ASTRALANE_URL")?,
            shredstream_url: required("SHREDSTREAM_URL")?,
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
    fn sol_amounts_must_be_positive_and_finite() {
        assert!(positive_sol("X", 0.001).is_ok());
        assert!(positive_sol("X", 0.0).is_err());
        assert!(positive_sol("X", -1.0).is_err());
        assert!(positive_sol("X", f64::NAN).is_err());
    }
}
