//! Reads what a trade needs from the chain: the token's curve and mint, and the program's
//! global settings (cached, they change rarely).
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;

use super::accounts::{BondingCurve, Global};
use super::ix::{self, Market, TOKEN_PROGRAM};
use super::{TOKEN_2022_PROGRAM, USDC_MINT, WSOL_MINT};

const GLOBAL_TTL: Duration = Duration::from_secs(600);

/// The program's global settings, fetched when first needed and refreshed every 10 minutes.
#[derive(Default)]
pub struct GlobalCache {
    cached: Mutex<Option<(Instant, Global)>>,
    next_buyback: AtomicUsize,
}

impl GlobalCache {
    async fn get(&self, rpc: &RpcClient) -> Result<Global> {
        if let Some((at, g)) = self
            .cached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            if at.elapsed() < GLOBAL_TTL {
                return Ok(g.clone());
            }
        }
        self.refresh(rpc).await
    }

    /// Reads the global settings from the chain and caches them.
    pub async fn refresh(&self, rpc: &RpcClient) -> Result<Global> {
        let account = rpc
            .get_account(&ix::global())
            .await
            .context("could not read the Pump.fun global account")?;
        let g = Global::decode(&account.data)
            .ok_or_else(|| anyhow!("the Pump.fun global account has an unexpected layout"))?;
        *self.cached.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), g.clone()));
        Ok(g)
    }

    /// Spreads trades over the program's buyback wallets, as its own front end does.
    fn pick_buyback(&self, g: &Global) -> Pubkey {
        let n = self.next_buyback.fetch_add(1, Ordering::Relaxed);
        g.buyback_fee_recipients[n % g.buyback_fee_recipients.len()]
    }
}

fn token_program_of(owner: &Pubkey) -> Result<Pubkey> {
    if *owner == TOKEN_PROGRAM || *owner == TOKEN_2022_PROGRAM {
        Ok(*owner)
    } else {
        bail!("mint is owned by {owner}, not a token program")
    }
}

/// Loads the curve, the token programs and a buyback wallet for `mint`.
pub async fn load_market(rpc: &RpcClient, cache: &GlobalCache, mint: &Pubkey) -> Result<Market> {
    let curve_key = ix::bonding_curve(mint);
    let accounts = rpc
        .get_multiple_accounts(&[curve_key, *mint])
        .await
        .context("could not read the bonding curve and mint")?;
    let curve_account = accounts
        .first()
        .and_then(|a| a.as_ref())
        .ok_or_else(|| anyhow!("no bonding curve for {mint}: not a Pump.fun token"))?;
    let mint_account = accounts
        .get(1)
        .and_then(|a| a.as_ref())
        .ok_or_else(|| anyhow!("mint {mint} does not exist"))?;
    let curve = BondingCurve::decode(&curve_account.data)
        .ok_or_else(|| anyhow!("bonding curve of {mint} has an unexpected layout"))?;
    let base_token_program = token_program_of(&mint_account.owner)?;
    let quote_token_program = if curve.quote_mint == WSOL_MINT || curve.quote_mint == USDC_MINT {
        TOKEN_PROGRAM
    } else {
        let q = rpc
            .get_account(&curve.quote_mint)
            .await
            .context("could not read the quote mint")?;
        token_program_of(&q.owner)?
    };
    let global = cache.get(rpc).await?;
    Ok(Market {
        mint: *mint,
        curve,
        base_token_program,
        quote_token_program,
        buyback_recipient: cache.pick_buyback(&global),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_token_programs_are_accepted() {
        assert_eq!(token_program_of(&TOKEN_PROGRAM).unwrap(), TOKEN_PROGRAM);
        assert_eq!(
            token_program_of(&TOKEN_2022_PROGRAM).unwrap(),
            TOKEN_2022_PROGRAM
        );
        assert!(token_program_of(&Pubkey::new_unique()).is_err());
    }

    #[test]
    fn buyback_wallets_rotate() {
        let cache = GlobalCache::default();
        let mut g = Global {
            fee_recipient: Pubkey::default(),
            fee_recipients: [Pubkey::default(); 7],
            buyback_fee_recipients: [Pubkey::default(); 8],
            whitelisted_quote_mints: vec![],
        };
        for (i, k) in g.buyback_fee_recipients.iter_mut().enumerate() {
            *k = Pubkey::new_from_array([i as u8 + 1; 32]);
        }
        let picks: Vec<_> = (0..9).map(|_| cache.pick_buyback(&g)).collect();
        assert_eq!(picks[0], g.buyback_fee_recipients[0]);
        assert_eq!(picks[8], g.buyback_fee_recipients[0]);
        assert_ne!(picks[0], picks[1]);
    }
}
