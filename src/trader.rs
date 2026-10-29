use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use log::{error, info, warn};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

use crate::pumpfun::PumpFunSDK;
use crate::state::{AppState, Position, PositionSource};
use crate::types::BuyAmounts;
use solana_sdk::hash::Hash;

/// How many times a sell is attempted before giving up on a position.
const MAX_SELL_ATTEMPTS: u32 = 5;
/// Pause between sell attempts (about two slots).
const SELL_RETRY_DELAY: Duration = Duration::from_millis(800);

enum SellOutcome {
    Sold(String, u64),
    NothingToSell,
}

/// Everything needed to send one buy. The caller must already have reserved the mint with
/// `AppState::claim_mint`; [`Trader::spawn_buy`] releases it once the buy is recorded or has
/// failed.
pub struct BuyRequest {
    pub mint: Pubkey,
    pub creator_vault: Pubkey,
    pub sol: f64,
    pub tip_sol: f64,
    /// A buy of this token whose price the order is sized from.
    pub reference: BuyAmounts,
    pub blockhash: Hash,
    pub sell_at_slot: u64,
    pub source: PositionSource,
}

/// Executes sells for positions tracked in [`AppState`]. Cheap to clone; the slot timer,
/// the web API and the TUI all share one.
#[derive(Clone)]
pub struct Trader {
    pub sdk: PumpFunSDK,
    pub payer: Arc<Keypair>,
    pub state: Arc<AppState>,
}

impl Trader {
    pub fn new(sdk: PumpFunSDK, payer: Keypair, state: Arc<AppState>) -> Self {
        Self {
            sdk,
            payer: Arc::new(payer),
            state,
        }
    }

    /// Sends a buy from its own task, so a slow RPC never stalls the shredstream loop, and
    /// records the position (or the failure) when it completes.
    pub fn spawn_buy(&self, req: BuyRequest) {
        let trader = self.clone();
        tokio::spawn(async move {
            let state = trader.state.clone();
            let mut sdk = trader.sdk.clone();
            sdk.priority_fee.jito_tip_fee = req.tip_sol;

            let result = sdk
                .buy(
                    &req.mint,
                    req.sol,
                    trader.payer.insecure_clone(),
                    req.blockhash,
                    req.reference,
                    req.creator_vault,
                    state.risk.buy_slippage_percent,
                )
                .await;

            match result {
                Ok((signature, _)) => {
                    info!("Placed buy of {}: {}", req.mint, signature);
                    // The sell uses the real on-chain balance, not the buy estimate.
                    state.record_buy_sent_from(
                        &req.mint,
                        &req.creator_vault,
                        &signature,
                        req.sol,
                        req.sell_at_slot,
                        req.source,
                    );
                }
                Err(e) => {
                    error!("Buy of {} failed: {}", req.mint, e);
                    state.record_buy_failed(&req.mint, &e.to_string());
                }
            }
            state.release_mint(&req.mint);
        });
    }

    /// Sells every held position whose sell slot has been reached. Each sell runs in its
    /// own task, so slow RPC calls never stall the caller (the shredstream loop).
    pub fn sell_due(&self, slot: u64) {
        for mint in self.state.due_positions(slot) {
            // Losing the claim just means another sell got there first.
            let _ = self.sell_now(&mint);
        }
    }

    /// Claims the position and sells it in the background. Fails if the position does not
    /// exist or is already being sold / sold.
    pub fn sell_now(&self, mint: &Pubkey) -> Result<(), String> {
        let position = self
            .state
            .begin_sell(mint)
            .ok_or_else(|| "no sellable position for this mint".to_string())?;
        tokio::spawn(self.clone().sell_position(position));
        Ok(())
    }

    async fn try_sell(&self, position: &Position) -> Result<SellOutcome> {
        let owner = self.payer.pubkey();
        let balance = self.sdk.token_balance(&owner, &position.mint_key).await?;
        if balance == 0 {
            return Ok(SellOutcome::NothingToSell);
        }

        // A fresh blockhash: the one from the create tx may be expired by now.
        let blockhash = self.sdk.latest_blockhash().await?;
        let signature = self
            .sdk
            .clone()
            .sell(
                &position.mint_key,
                balance,
                self.payer.insecure_clone(),
                blockhash,
                position.creator,
            )
            .await?;

        Ok(SellOutcome::Sold(signature, balance))
    }

    /// Sells a claimed position with retries. Skips the sell when there is nothing to
    /// sell, which happens when the buy never landed.
    async fn sell_position(self, position: Position) {
        let mint = position.mint_key;
        let mut last_error = String::from("the buy never landed (no token balance)");

        for attempt in 1..=MAX_SELL_ATTEMPTS {
            match self.try_sell(&position).await {
                Ok(SellOutcome::Sold(signature, amount)) => {
                    info!(
                        "Placed PumpFun sell for {} with Signature: {}",
                        mint, signature
                    );
                    self.state.record_sell_sent(&mint, &signature, amount);
                    return;
                }
                Ok(SellOutcome::NothingToSell) => {
                    last_error = "no token balance to sell (the buy may not have landed)".into();
                    warn!(
                        "No {} balance to sell yet (attempt {}/{})",
                        mint, attempt, MAX_SELL_ATTEMPTS
                    );
                }
                Err(e) => {
                    last_error = format!("{e:#}");
                    error!(
                        "Sell of {} failed (attempt {}/{}): {:#}",
                        mint, attempt, MAX_SELL_ATTEMPTS, e
                    );
                }
            }

            if attempt < MAX_SELL_ATTEMPTS {
                tokio::time::sleep(SELL_RETRY_DELAY).await;
            }
        }

        error!(
            "Giving up selling {}: either the buy never landed or the sell kept failing. Check the wallet.",
            mint
        );
        self.state.record_sell_failed(&mint, &last_error);
    }
}
