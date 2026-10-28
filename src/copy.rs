use std::collections::HashSet;
use std::env;
use std::str::FromStr;

use anyhow::{bail, Result};
use log::{error, info};
use serde::Serialize;
use solana_sdk::hash::Hash;
use solana_sdk::message::compiled_instruction::CompiledInstruction;
use solana_sdk::pubkey::Pubkey;

use crate::config::{optional_parsed, positive_sol};
use crate::risk::{lamports_to_sol, Candidate};
use crate::state::{PositionSource, MAX_BUY_AMOUNT_SOL};
use crate::trader::Trader;
use crate::types::{BuyAmounts, PUMP_FUN_PROGRAM_ID, PUMP_ORDER_BUY, PUMP_ORDER_SELL};

const MAX_HOLD_SLOTS_LIMIT: u64 = 2_000;

/// Copy-trade settings. Disabled unless `COPY_WALLETS` lists at least one wallet.
#[derive(Clone, Debug)]
pub struct CopyConfig {
    pub wallets: HashSet<Pubkey>,
    /// SOL spent per copied buy, whatever the leader spent.
    pub buy_sol: f64,
    /// Ignore leader buys smaller / larger than this many SOL (0 = no limit).
    pub min_leader_sol: f64,
    pub max_leader_sol: f64,
    /// Sell our copy as soon as the leader sells the same token.
    pub sell_with_leader: bool,
    /// Safety exit: sell after this many slots even if the leader never sells.
    pub max_hold_slots: u64,
}

impl Default for CopyConfig {
    fn default() -> Self {
        Self {
            wallets: HashSet::new(),
            buy_sol: 0.001,
            min_leader_sol: 0.0,
            max_leader_sol: 0.0,
            sell_with_leader: true,
            max_hold_slots: 150,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CopyView {
    pub enabled: bool,
    pub wallets: usize,
    pub buy_sol: f64,
    pub min_leader_sol: f64,
    pub max_leader_sol: f64,
    pub sell_with_leader: bool,
    pub max_hold_slots: u64,
}

impl CopyConfig {
    /// `default_buy_sol` is used when `COPY_BUY_SOL` is not set.
    pub fn from_env(default_buy_sol: f64) -> Result<Self> {
        let wallets = env::var("COPY_WALLETS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                Pubkey::from_str(s)
                    .map_err(|_| anyhow::anyhow!("COPY_WALLETS has an invalid address: {s}"))
            })
            .collect::<Result<HashSet<_>>>()?;

        let d = Self::default();
        let cfg = Self {
            wallets,
            buy_sol: optional_parsed("COPY_BUY_SOL", default_buy_sol)?,
            min_leader_sol: optional_parsed("COPY_MIN_LEADER_SOL", d.min_leader_sol)?,
            max_leader_sol: optional_parsed("COPY_MAX_LEADER_SOL", d.max_leader_sol)?,
            sell_with_leader: optional_parsed("COPY_SELL_WITH_LEADER", d.sell_with_leader)?,
            max_hold_slots: optional_parsed("COPY_MAX_HOLD_SLOTS", d.max_hold_slots)?,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        positive_sol("COPY_BUY_SOL", self.buy_sol)?;
        if self.buy_sol > MAX_BUY_AMOUNT_SOL {
            bail!("COPY_BUY_SOL must be at most {MAX_BUY_AMOUNT_SOL}");
        }
        for (name, v) in [
            ("COPY_MIN_LEADER_SOL", self.min_leader_sol),
            ("COPY_MAX_LEADER_SOL", self.max_leader_sol),
        ] {
            if !v.is_finite() || v < 0.0 {
                bail!("{name} must be 0 (off) or a positive number, got {v}");
            }
        }
        if self.max_leader_sol > 0.0 && self.min_leader_sol > self.max_leader_sol {
            bail!("COPY_MIN_LEADER_SOL cannot be larger than COPY_MAX_LEADER_SOL");
        }
        if !(1..=MAX_HOLD_SLOTS_LIMIT).contains(&self.max_hold_slots) {
            bail!("COPY_MAX_HOLD_SLOTS must be between 1 and {MAX_HOLD_SLOTS_LIMIT}");
        }
        Ok(())
    }

    pub fn enabled(&self) -> bool {
        !self.wallets.is_empty()
    }

    pub fn view(&self) -> CopyView {
        CopyView {
            enabled: self.enabled(),
            wallets: self.wallets.len(),
            buy_sol: self.buy_sol,
            min_leader_sol: self.min_leader_sol,
            max_leader_sol: self.max_leader_sol,
            sell_with_leader: self.sell_with_leader,
            max_hold_slots: self.max_hold_slots,
        }
    }

    /// `Err(reason)` when a leader's buy of `leader_sol` is outside the configured range.
    pub fn check_leader_size(&self, leader_sol: f64) -> Result<(), String> {
        if self.min_leader_sol > 0.0 && leader_sol < self.min_leader_sol {
            return Err(format!(
                "leader buy {leader_sol:.3} SOL is below the minimum {}",
                self.min_leader_sol
            ));
        }
        if self.max_leader_sol > 0.0 && leader_sol > self.max_leader_sol {
            return Err(format!(
                "leader buy {leader_sol:.3} SOL is above the maximum {}",
                self.max_leader_sol
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

/// A Pump.fun buy or sell made by one of the followed wallets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeaderTrade {
    pub side: Side,
    pub leader: Pubkey,
    pub mint: Pubkey,
    /// The account the program pays the creator's fee to (the same one our own buy/sell
    /// instruction must carry).
    pub creator_vault: Pubkey,
    /// Tokens (raw units) and the SOL cap of the leader's instruction.
    pub token_amount: u64,
    pub sol_amount: u64,
}

impl LeaderTrade {
    pub fn sol(&self) -> f64 {
        lamports_to_sol(self.sol_amount)
    }
}

// Account positions in the Pump.fun `buy` / `sell` instructions.
const IX_MINT: usize = 2;
const IX_USER: usize = 6;
const IX_BUY_CREATOR_VAULT: usize = 9;
const IX_SELL_CREATOR_VAULT: usize = 8;

/// Looks for a Pump.fun buy or sell signed by a followed wallet. Never panics on
/// malformed input; returns the first matching instruction.
pub fn leader_trade(
    account_keys: &[Pubkey],
    instructions: &[CompiledInstruction],
    followed: &HashSet<Pubkey>,
) -> Option<LeaderTrade> {
    // Cheap pre-filter: skip the parsing unless a followed wallet is in the transaction.
    if !account_keys.iter().any(|k| followed.contains(k)) {
        return None;
    }

    let key_at = |ix: &CompiledInstruction, n: usize| -> Option<Pubkey> {
        ix.accounts
            .get(n)
            .and_then(|i| account_keys.get(*i as usize))
            .copied()
    };

    for ix in instructions {
        if account_keys.get(ix.program_id_index as usize) != Some(&PUMP_FUN_PROGRAM_ID) {
            continue;
        }
        let data = &ix.data;
        if data.len() < 24 {
            continue;
        }
        let side = if data[0..8] == PUMP_ORDER_BUY {
            Side::Buy
        } else if data[0..8] == PUMP_ORDER_SELL {
            Side::Sell
        } else {
            continue;
        };

        let Some(leader) = key_at(ix, IX_USER).filter(|k| followed.contains(k)) else {
            continue;
        };
        let vault_index = match side {
            Side::Buy => IX_BUY_CREATOR_VAULT,
            Side::Sell => IX_SELL_CREATOR_VAULT,
        };
        let (Some(mint), Some(creator_vault)) = (key_at(ix, IX_MINT), key_at(ix, vault_index))
        else {
            continue;
        };

        return Some(LeaderTrade {
            side,
            leader,
            mint,
            creator_vault,
            token_amount: u64::from_le_bytes(data[8..16].try_into().ok()?),
            sol_amount: u64::from_le_bytes(data[16..24].try_into().ok()?),
        });
    }
    None
}

/// Reacts to a followed wallet's trade. Buys are sent from their own task so a slow RPC
/// never stalls the shredstream loop.
pub fn handle_leader_trade(trader: &Trader, trade: LeaderTrade, slot: u64, blockhash: Hash) {
    let state = &trader.state;
    let copy = &state.copy;
    state.record_copy_signal(&trade, slot);

    match trade.side {
        Side::Sell => {
            if copy.sell_with_leader && state.is_copy_held(&trade.mint) {
                info!(
                    "Leader {} sold {}: selling our copy",
                    trade.leader, trade.mint
                );
                // Losing the claim just means a sell is already under way.
                let _ = trader.sell_now(&trade.mint);
            }
        }
        Side::Buy => copy_buy(trader, trade, slot, blockhash),
    }
}

fn copy_buy(trader: &Trader, trade: LeaderTrade, slot: u64, blockhash: Hash) {
    let state = &trader.state;
    let skip = |reason: String| {
        info!(
            "Not copying {} from {}: {}",
            trade.mint, trade.leader, reason
        );
        state.record_skipped(&trade.mint, &reason);
    };

    if state.settings.paused() {
        return skip("bot is paused".into());
    }
    if trade.token_amount == 0 || trade.sol_amount == 0 {
        return skip("leader instruction has a zero amount".into());
    }
    if let Err(reason) = state.copy.check_leader_size(trade.sol()) {
        return skip(reason);
    }
    if state.mint_busy(&trade.mint) {
        return skip("already holding or buying this token".into());
    }

    let tip_sol = state.settings.bribe_fee_sol();
    let candidate = Candidate {
        creator: &trade.creator_vault,
        name: None,
        symbol: None,
        dev_buy_sol: trade.sol(),
    };
    if let Err(reason) =
        state
            .risk
            .check(&candidate, &state.exposure(), state.copy.buy_sol, tip_sol)
    {
        return skip(reason);
    }
    // Reserve the mint and the SOL now, before the buy is sent from its own task, so the
    // next signal in this slot already sees this one in the risk limits.
    if !state.claim_mint(&trade.mint, state.copy.buy_sol) {
        return skip("already holding or buying this token".into());
    }

    let trader = trader.clone();
    tokio::spawn(async move {
        let state = trader.state.clone();
        let buy_sol = state.copy.buy_sol;
        let mut sdk = trader.sdk.clone();
        sdk.priority_fee.jito_tip_fee = tip_sol;

        let result = sdk
            .buy(
                &trade.mint,
                buy_sol,
                trader.payer.insecure_clone(),
                blockhash,
                BuyAmounts {
                    base_amount_out: trade.token_amount,
                    max_quote_amount_in: trade.sol_amount,
                },
                trade.creator_vault,
                state.risk.buy_slippage_percent,
            )
            .await;

        match result {
            Ok((signature, _)) => {
                info!("Copied {} from {}: {}", trade.mint, trade.leader, signature);
                state.record_buy_sent_from(
                    &trade.mint,
                    &trade.creator_vault,
                    &signature,
                    buy_sol,
                    slot + state.copy.max_hold_slots,
                    PositionSource::Copy {
                        leader: trade.leader.to_string(),
                    },
                );
            }
            Err(e) => {
                error!("Copy buy of {} failed: {}", trade.mint, e);
                state.record_buy_failed(&trade.mint, &e.to_string());
            }
        }
        state.release_mint(&trade.mint);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    fn data(discriminator: [u8; 8], amount: u64, sol: u64) -> Vec<u8> {
        let mut d = discriminator.to_vec();
        d.extend_from_slice(&amount.to_le_bytes());
        d.extend_from_slice(&sol.to_le_bytes());
        d
    }

    /// Keys: 0 = Pump.fun program, 1..=12 = 12 distinct accounts.
    fn keys() -> Vec<Pubkey> {
        std::iter::once(PUMP_FUN_PROGRAM_ID)
            .chain((1..=12).map(key))
            .collect()
    }

    /// Account list laid out like Pump.fun's `buy`: user is the 7th account.
    fn ix(discriminator: [u8; 8]) -> CompiledInstruction {
        CompiledInstruction::new_from_raw_parts(
            0,
            data(discriminator, 5_000, 7_000),
            (1..=12).collect(),
        )
    }

    fn followed(n: u8) -> HashSet<Pubkey> {
        HashSet::from([key(n)])
    }

    #[test]
    fn detects_a_followed_wallets_buy_and_sell() {
        // accounts[6] is the key at index 7 in `keys()` => key(7)
        let buy = leader_trade(&keys(), &[ix(PUMP_ORDER_BUY)], &followed(7)).unwrap();
        assert_eq!(buy.side, Side::Buy);
        assert_eq!(buy.leader, key(7));
        assert_eq!(buy.mint, key(3));
        assert_eq!(
            buy.creator_vault,
            key(10),
            "buy puts the creator vault at index 9"
        );
        assert_eq!((buy.token_amount, buy.sol_amount), (5_000, 7_000));

        let sell = leader_trade(&keys(), &[ix(PUMP_ORDER_SELL)], &followed(7)).unwrap();
        assert_eq!(sell.side, Side::Sell);
        assert_eq!(
            sell.creator_vault,
            key(9),
            "sell puts the creator vault at index 8"
        );
    }

    #[test]
    fn ignores_everything_that_is_not_a_followed_pump_fun_trade() {
        let keys = keys();
        // Followed wallet is in the transaction but is not the instruction's user.
        assert!(leader_trade(&keys, &[ix(PUMP_ORDER_BUY)], &followed(2)).is_none());
        // Nobody followed is involved.
        assert!(leader_trade(&keys, &[ix(PUMP_ORDER_BUY)], &followed(99)).is_none());
        assert!(leader_trade(&keys, &[ix(PUMP_ORDER_BUY)], &HashSet::new()).is_none());
        // Another instruction (create) from the followed wallet.
        assert!(
            leader_trade(&keys, &[ix([24, 30, 200, 40, 5, 28, 7, 119])], &followed(7)).is_none()
        );
        // Same bytes sent to a different program.
        let other = CompiledInstruction::new_from_raw_parts(
            5,
            data(PUMP_ORDER_BUY, 1, 1),
            (1..=12).collect(),
        );
        assert!(leader_trade(&keys, &[other], &followed(7)).is_none());
    }

    #[test]
    fn malformed_instructions_never_panic() {
        let keys = keys();
        let f = followed(7);
        let short_data =
            CompiledInstruction::new_from_raw_parts(0, PUMP_ORDER_BUY.to_vec(), (1..=12).collect());
        let few_accounts =
            CompiledInstruction::new_from_raw_parts(0, data(PUMP_ORDER_BUY, 1, 1), vec![1, 2, 3]);
        let bad_index =
            CompiledInstruction::new_from_raw_parts(0, data(PUMP_ORDER_BUY, 1, 1), vec![200; 12]);
        let bad_program = CompiledInstruction::new_from_raw_parts(
            200,
            data(PUMP_ORDER_BUY, 1, 1),
            (1..=12).collect(),
        );
        assert!(leader_trade(
            &keys,
            &[short_data, few_accounts, bad_index, bad_program],
            &f
        )
        .is_none());
        // A later valid instruction is still found after bad ones.
        let found = leader_trade(
            &keys,
            &[
                CompiledInstruction::new_from_raw_parts(0, vec![], vec![]),
                ix(PUMP_ORDER_BUY),
            ],
            &f,
        );
        assert!(found.is_some());
    }

    #[test]
    fn config_validation_and_size_filter() {
        let mut cfg = CopyConfig::default();
        assert!(cfg.validate().is_ok());
        assert!(!cfg.enabled());

        cfg.wallets.insert(key(1));
        assert!(cfg.enabled());
        cfg.min_leader_sol = 0.5;
        cfg.max_leader_sol = 2.0;
        assert!(cfg.check_leader_size(0.1).unwrap_err().contains("below"));
        assert!(cfg.check_leader_size(3.0).unwrap_err().contains("above"));
        assert!(cfg.check_leader_size(1.0).is_ok());

        assert!(CopyConfig {
            buy_sol: 0.0,
            ..CopyConfig::default()
        }
        .validate()
        .is_err());
        assert!(CopyConfig {
            buy_sol: 11.0,
            ..CopyConfig::default()
        }
        .validate()
        .is_err());
        assert!(CopyConfig {
            max_hold_slots: 0,
            ..CopyConfig::default()
        }
        .validate()
        .is_err());
        assert!(CopyConfig {
            max_hold_slots: 5_000,
            ..CopyConfig::default()
        }
        .validate()
        .is_err());
        assert!(CopyConfig {
            min_leader_sol: 3.0,
            max_leader_sol: 2.0,
            ..CopyConfig::default()
        }
        .validate()
        .is_err());
        assert!(CopyConfig {
            min_leader_sol: -1.0,
            ..CopyConfig::default()
        }
        .validate()
        .is_err());
    }

    // ---- handle_leader_trade, against a state with no reachable RPC ----

    use crate::pumpfun::PumpFunSDK;
    use crate::pumpswap::constants::PriorityFeePump;
    use crate::state::{AppState, Event, PositionStatus, Settings};
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;
    use std::sync::Arc;
    use std::time::Duration;

    async fn trader(copy: CopyConfig) -> Trader {
        let payer = Keypair::new();
        let state =
            Arc::new(AppState::new(payer.pubkey(), Settings::new(0.001, 0.001, 7)).with_copy(copy));
        let sdk = PumpFunSDK::new(
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
            PriorityFeePump::default(),
        )
        .await;
        Trader::new(sdk, payer, state)
    }

    fn trade(side: Side, mint: Pubkey, sol: u64) -> LeaderTrade {
        LeaderTrade {
            side,
            leader: key(7),
            mint,
            creator_vault: key(10),
            token_amount: 1_000_000,
            sol_amount: sol,
        }
    }

    fn cfg() -> CopyConfig {
        CopyConfig {
            wallets: HashSet::from([key(7)]),
            ..CopyConfig::default()
        }
    }

    fn has_skip(t: &Trader, needle: &str) -> bool {
        t.state
            .snapshot()
            .recent
            .iter()
            .any(|e| matches!(&e.event, Event::Skipped { reason, .. } if reason.contains(needle)))
    }

    #[tokio::test]
    async fn buys_are_skipped_when_paused_out_of_range_or_already_held() {
        let t = trader(CopyConfig {
            min_leader_sol: 0.5,
            ..cfg()
        })
        .await;
        let bh = Hash::default();

        t.state.set_paused(true);
        handle_leader_trade(&t, trade(Side::Buy, key(50), 1_000_000_000), 100, bh);
        assert!(has_skip(&t, "paused"));
        t.state.set_paused(false);

        handle_leader_trade(&t, trade(Side::Buy, key(51), 10_000_000), 100, bh);
        assert!(has_skip(&t, "below the minimum"));

        let held = key(52);
        t.state.record_buy_sent(&held, &key(10), "sig", 0.001, 999);
        handle_leader_trade(&t, trade(Side::Buy, held, 1_000_000_000), 100, bh);
        assert!(has_skip(&t, "already holding"));

        handle_leader_trade(&t, trade(Side::Buy, key(53), 0), 100, bh);
        assert!(has_skip(&t, "zero amount"));

        // The leader's signal itself is always recorded.
        let signals = t
            .state
            .snapshot()
            .recent
            .iter()
            .filter(|e| matches!(e.event, Event::CopySignal { .. }))
            .count();
        assert_eq!(signals, 4);
    }

    #[tokio::test]
    async fn risk_limits_apply_to_copied_buys() {
        let mut t = trader(cfg()).await;
        let state = Arc::get_mut(&mut t.state).unwrap();
        state.risk.max_open_positions = 1;
        t.state
            .record_buy_sent(&key(60), &key(10), "sig", 0.001, 999);

        handle_leader_trade(
            &t,
            trade(Side::Buy, key(61), 1_000_000_000),
            100,
            Hash::default(),
        );
        assert!(has_skip(&t, "positions already open"));
        assert!(
            !t.state.mint_busy(&key(61)),
            "a refused buy must not keep the mint reserved"
        );
    }

    #[tokio::test]
    async fn a_burst_of_signals_cannot_exceed_the_position_limit() {
        let mut t = trader(cfg()).await;
        Arc::get_mut(&mut t.state).unwrap().risk.max_open_positions = 2;

        // Five different tokens in the same slot: no buy has been recorded yet, but the
        // reservations of the earlier ones must already count.
        for n in 0..5u8 {
            handle_leader_trade(
                &t,
                trade(Side::Buy, key(100 + n), 1_000_000_000),
                100,
                Hash::default(),
            );
        }
        let skipped = t
            .state
            .snapshot()
            .recent
            .iter()
            .filter(|e| matches!(&e.event, Event::Skipped { reason, .. } if reason.contains("positions already open")))
            .count();
        assert_eq!(skipped, 3, "only 2 of the 5 may go through");
    }

    #[tokio::test]
    async fn a_failed_copy_buy_is_reported_and_releases_the_mint() {
        let t = trader(cfg()).await;
        let mint = key(70);
        handle_leader_trade(
            &t,
            trade(Side::Buy, mint, 1_000_000_000),
            100,
            Hash::default(),
        );

        // The RPC is unreachable, so the spawned buy fails; wait for that to be reported.
        let mut failed = false;
        for _ in 0..100 {
            failed = t.state.snapshot().recent.iter().any(
                |e| matches!(&e.event, Event::BuyFailed { mint: m, .. } if *m == mint.to_string()),
            );
            if failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(failed, "buy failure was not reported");
        assert!(
            t.state.snapshot().positions.is_empty(),
            "no position for a failed buy"
        );
        assert!(
            !t.state.mint_busy(&mint),
            "the mint must be released after a failure"
        );
    }

    #[tokio::test]
    async fn leader_sell_only_closes_copied_positions() {
        let t = trader(cfg()).await;
        let bh = Hash::default();

        let copied = key(80);
        t.state.record_buy_sent_from(
            &copied,
            &key(10),
            "sig",
            0.001,
            999,
            PositionSource::Copy {
                leader: key(7).to_string(),
            },
        );
        let sniped = key(81);
        t.state
            .record_buy_sent(&sniped, &key(10), "sig", 0.001, 999);

        // A leader sale of a token we sniped ourselves must not close our position.
        handle_leader_trade(&t, trade(Side::Sell, sniped, 1_000_000), 100, bh);
        let status = |m: &Pubkey| {
            t.state
                .snapshot()
                .positions
                .iter()
                .find(|p| p.mint_key == *m)
                .unwrap()
                .status
        };
        assert_eq!(status(&sniped), PositionStatus::Held);

        handle_leader_trade(&t, trade(Side::Sell, copied, 1_000_000), 100, bh);
        assert_eq!(
            status(&copied),
            PositionStatus::Selling,
            "copy is sold with the leader"
        );

        // Disabled: the copy keeps waiting for the slot timer.
        let t2 = trader(CopyConfig {
            sell_with_leader: false,
            ..cfg()
        })
        .await;
        t2.state.record_buy_sent_from(
            &copied,
            &key(10),
            "sig",
            0.001,
            999,
            PositionSource::Copy {
                leader: key(7).to_string(),
            },
        );
        handle_leader_trade(&t2, trade(Side::Sell, copied, 1_000_000), 100, bh);
        assert_eq!(
            t2.state.snapshot().positions[0].status,
            PositionStatus::Held
        );
    }
}
