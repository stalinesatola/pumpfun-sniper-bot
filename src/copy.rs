use std::collections::HashSet;
use std::env;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use anyhow::{bail, Result};
use log::info;
use log::warn;
use serde::{Deserialize, Serialize};
use solana_sdk::hash::Hash;
use solana_sdk::message::compiled_instruction::CompiledInstruction;
use solana_sdk::pubkey::Pubkey;

use crate::config::{optional_parsed, positive_sol};
use crate::persist::{self, Loaded};
use crate::risk::{lamports_to_sol, Candidate};
use crate::state::{PositionSource, TradingMode, MAX_BUY_AMOUNT_SOL};
use crate::trader::{BuyRequest, Trader};
use crate::types::{BuyAmounts, PUMP_FUN_PROGRAM_ID, PUMP_ORDER_BUY, PUMP_ORDER_SELL};

const MAX_HOLD_SLOTS_LIMIT: u64 = 2_000;

/// Copy-trade settings. Disabled unless `COPY_WALLETS` lists at least one wallet.
#[derive(Clone, Debug)]
pub struct CopyConfig {
    /// Wallets to follow at startup (`COPY_WALLETS`). After that the list lives in
    /// [`CopyFollow`], which the dashboard edits.
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
    /// The on/off switch (set from the dashboard).
    pub enabled: bool,
    /// On and at least one wallet followed: copy-trade is actually running.
    pub active: bool,
    pub wallets: Vec<String>,
    pub buy_sol: f64,
    pub min_leader_sol: f64,
    pub max_leader_sol: f64,
    pub sell_with_leader: bool,
    pub max_hold_slots: u64,
    /// Where the wallet list is saved, if it is.
    pub persisted: bool,
    /// Set when the last attempt to save the list failed (the change is still in effect).
    pub save_error: Option<String>,
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

    pub fn view(&self, follow: &CopyFollow) -> CopyView {
        CopyView {
            enabled: follow.enabled(),
            active: follow.active(),
            wallets: follow.list(),
            buy_sol: self.buy_sol,
            min_leader_sol: self.min_leader_sol,
            max_leader_sol: self.max_leader_sol,
            sell_with_leader: self.sell_with_leader,
            max_hold_slots: self.max_hold_slots,
            persisted: follow.persisted(),
            save_error: follow.save_error(),
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

/// Most wallets that can be followed at once.
pub const MAX_FOLLOWED_WALLETS: usize = 50;

#[derive(Serialize, Deserialize)]
struct SavedFollow {
    enabled: bool,
    wallets: Vec<String>,
}

/// The wallets being followed and the on/off switch. Unlike the rest of [`CopyConfig`] this
/// changes while the bot runs (from the dashboard), so it is shared and thread-safe. Reading
/// it is cheap: the stream loop takes one `Arc` snapshot per message.
pub struct CopyFollow {
    enabled: AtomicBool,
    wallets: RwLock<Arc<HashSet<Pubkey>>>,
    /// The bot's own wallet, which can never be followed (it would copy itself).
    own: Pubkey,
    store: Option<PathBuf>,
    save_error: Mutex<Option<String>>,
}

fn lock_err(m: &Mutex<Option<String>>) -> MutexGuard<'_, Option<String>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl CopyFollow {
    pub fn new(wallets: HashSet<Pubkey>, own: Pubkey) -> Self {
        Self {
            enabled: AtomicBool::new(true),
            wallets: RwLock::new(Arc::new(wallets)),
            own,
            store: None,
            save_error: Mutex::new(None),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Switch on and at least one wallet followed.
    pub fn active(&self) -> bool {
        self.enabled() && !self.wallets().is_empty()
    }

    pub fn persisted(&self) -> bool {
        self.store.is_some()
    }

    pub fn save_error(&self) -> Option<String> {
        lock_err(&self.save_error).clone()
    }

    /// A snapshot of the followed wallets; later changes do not affect it.
    pub fn wallets(&self) -> Arc<HashSet<Pubkey>> {
        self.wallets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The followed wallets as base58, sorted, for display.
    pub fn list(&self) -> Vec<String> {
        let mut list: Vec<String> = self.wallets().iter().map(|k| k.to_string()).collect();
        list.sort();
        list
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        self.save();
    }

    /// Parses and validates an address typed by a person.
    pub fn parse_wallet(&self, text: &str) -> Result<Pubkey, String> {
        let text = text.trim();
        let key = Pubkey::from_str(text).map_err(|_| {
            "not a valid Solana address (expected base58, 32-44 characters)".to_string()
        })?;
        if key == self.own {
            return Err("that is this bot's own wallet; it cannot copy itself".into());
        }
        Ok(key)
    }

    /// Adds a wallet. Adding one that is already followed is not an error.
    pub fn add_wallet(&self, text: &str) -> Result<Pubkey, String> {
        let key = self.parse_wallet(text)?;
        {
            let mut guard = self.wallets.write().unwrap_or_else(|e| e.into_inner());
            if !guard.contains(&key) {
                if guard.len() >= MAX_FOLLOWED_WALLETS {
                    return Err(format!(
                        "at most {MAX_FOLLOWED_WALLETS} wallets can be followed"
                    ));
                }
                let mut next = (**guard).clone();
                next.insert(key);
                *guard = Arc::new(next);
            }
        }
        self.save();
        Ok(key)
    }

    /// Removes a wallet. Returns whether it was being followed.
    pub fn remove_wallet(&self, text: &str) -> Result<bool, String> {
        let key =
            Pubkey::from_str(text.trim()).map_err(|_| "not a valid Solana address".to_string())?;
        let removed = {
            let mut guard = self.wallets.write().unwrap_or_else(|e| e.into_inner());
            if guard.contains(&key) {
                let mut next = (**guard).clone();
                next.remove(&key);
                *guard = Arc::new(next);
                true
            } else {
                false
            }
        };
        if removed {
            self.save();
        }
        Ok(removed)
    }

    /// Saves the list to `path` from now on. If the file already exists it replaces the
    /// startup list and switch, so what was set in the dashboard survives a restart. A file
    /// that cannot be read is ignored (and kept as it is until the next change).
    pub fn attach_store(&mut self, path: PathBuf) {
        match persist::read_json::<SavedFollow>(&path) {
            Loaded::Value(saved) => {
                let mut wallets = HashSet::new();
                for w in &saved.wallets {
                    match self.parse_wallet(w) {
                        Ok(k) => {
                            wallets.insert(k);
                        }
                        Err(e) => warn!("Ignoring `{w}` in {}: {e}", path.display()),
                    }
                }
                wallets.truncate_to(MAX_FOLLOWED_WALLETS);
                *self.wallets.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(wallets);
                self.enabled.store(saved.enabled, Ordering::Relaxed);
            }
            Loaded::Missing => {}
            Loaded::Unreadable(e) => warn!(
                "Could not read {}: {e}; using the startup list",
                path.display()
            ),
        }
        self.store = Some(path);
    }

    /// Writes the current list (atomically) and remembers whether that worked.
    fn save(&self) {
        let Some(path) = &self.store else {
            return;
        };
        let saved = SavedFollow {
            enabled: self.enabled(),
            wallets: self.list(),
        };
        let result = persist::write_json(path, &saved);
        *lock_err(&self.save_error) = match result {
            Ok(()) => None,
            Err(e) => {
                warn!("Could not save {}: {e}", path.display());
                Some(format!("could not save {}: {e}", path.display()))
            }
        };
    }
}

trait TruncateTo {
    fn truncate_to(&mut self, max: usize);
}

impl TruncateTo for HashSet<Pubkey> {
    fn truncate_to(&mut self, max: usize) {
        while self.len() > max {
            let Some(k) = self.iter().next().copied() else {
                break;
            };
            self.remove(&k);
        }
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
pub(crate) const IX_MINT: usize = 2;
pub(crate) const IX_USER: usize = 6;
pub(crate) const IX_BUY_CREATOR_VAULT: usize = 9;
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
                if !state.settings.mode().allows_sells() {
                    state.record_skipped(&trade.mint, TradingMode::SELL_REFUSED);
                    return;
                }
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
    if !state.settings.mode().allows_buys() {
        return skip(TradingMode::BUY_REFUSED.into());
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
    // Only new coins, and (if required) ones that already have an active community.
    let creator_wallet = match state.community.check_for_copy(&trade.mint, slot) {
        Ok(wallet) => wallet,
        Err(reason) => return skip(reason),
    };

    let buy_sol = state.copy.buy_sol;
    let tip_sol = state.settings.bribe_fee_sol();
    let candidate = Candidate {
        // The creator's wallet when we saw the launch; the vault address is all we have otherwise.
        creator: creator_wallet.as_ref().unwrap_or(&trade.creator_vault),
        name: None,
        symbol: None,
        dev_buy_sol: trade.sol(),
    };
    if let Err(reason) = state
        .risk
        .check(&candidate, &state.exposure(), buy_sol, tip_sol)
    {
        return skip(reason);
    }
    // Reserve the mint and the SOL now, before the buy is sent from its own task, so the
    // next signal in this slot already sees this one in the risk limits.
    if !state.claim_mint(&trade.mint, buy_sol) {
        return skip("already holding or buying this token".into());
    }

    trader.spawn_buy(BuyRequest {
        mint: trade.mint,
        creator_vault: trade.creator_vault,
        sol: buy_sol,
        tip_sol,
        reference: BuyAmounts {
            base_amount_out: trade.token_amount,
            max_quote_amount_in: trade.sol_amount,
        },
        blockhash,
        sell_at_slot: slot + state.copy.max_hold_slots,
        source: PositionSource::Copy {
            leader: trade.leader.to_string(),
        },
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

        cfg.wallets.insert(key(1));
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

    use crate::community::{CommunityConfig, NewToken, ObservedBuy};
    use crate::pumpfun::PumpFunSDK;
    use crate::pumpswap::constants::PriorityFeePump;
    use crate::state::{AppState, Event, PositionStatus, Settings};
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;
    use std::sync::Arc;
    use std::time::Duration;

    /// Community and age rules off, so these tests are about the copy logic itself.
    fn open_gate() -> CommunityConfig {
        CommunityConfig {
            require_community: false,
            max_token_age_slots: 0,
            ..CommunityConfig::default()
        }
    }

    async fn trader(copy: CopyConfig) -> Trader {
        let payer = Keypair::new();
        let state = Arc::new(
            AppState::new(payer.pubkey(), Settings::new(0.001, 0.001, 7))
                .with_copy(copy)
                .with_community(open_gate()),
        );
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

    // ---- the "new coins with an active community" gate ----

    async fn gated_trader() -> Trader {
        let payer = Keypair::new();
        let state = Arc::new(
            AppState::new(payer.pubkey(), Settings::new(0.001, 0.001, 7))
                .with_copy(cfg())
                .with_community(CommunityConfig {
                    require_community: true,
                    window_slots: 10,
                    min_buyers: 2,
                    min_volume_sol: 0.0,
                    max_token_age_slots: 100,
                }),
        );
        let sdk = PumpFunSDK::new(
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
            PriorityFeePump::default(),
        )
        .await;
        Trader::new(sdk, payer, state)
    }

    fn launch(t: &Trader, mint: Pubkey, slot: u64) {
        t.state.community.track(NewToken {
            mint,
            creator_wallet: key(200),
            creator_vault: None,
            name: None,
            symbol: None,
            dev_buy_sol: 0.0,
            slot,
            reference: None,
        });
    }

    fn community_buy(t: &Trader, mint: Pubkey, buyer: u8) {
        t.state.community.observe_buy(
            &ObservedBuy {
                mint,
                buyer: key(buyer),
                creator_vault: key(10),
                token_amount: 1_000,
                sol_amount: 1_000_000,
            },
            Hash::default(),
        );
    }

    #[tokio::test]
    async fn copying_needs_a_new_token_with_an_active_community() {
        let t = gated_trader().await;
        let bh = Hash::default();

        // A token whose launch we never saw is not "new".
        handle_leader_trade(&t, trade(Side::Buy, key(120), 1_000_000_000), 100, bh);
        assert!(has_skip(&t, "not a new token"));

        // New, but nobody has bought yet.
        let fresh = key(121);
        launch(&t, fresh, 100);
        handle_leader_trade(&t, trade(Side::Buy, fresh, 1_000_000_000), 102, bh);
        assert!(has_skip(&t, "unique buyers"));

        // Too old.
        let old = key(122);
        launch(&t, old, 100);
        community_buy(&t, old, 1);
        community_buy(&t, old, 2);
        handle_leader_trade(&t, trade(Side::Buy, old, 1_000_000_000), 500, bh);
        assert!(has_skip(&t, "slots old"));
    }

    #[tokio::test]
    async fn an_active_new_token_is_copied() {
        let t = gated_trader().await;
        let mint = key(130);
        launch(&t, mint, 100);
        community_buy(&t, mint, 1);
        community_buy(&t, mint, 2);

        handle_leader_trade(
            &t,
            trade(Side::Buy, mint, 1_000_000_000),
            105,
            Hash::default(),
        );
        // Not skipped: a buy was attempted (and fails, since no RPC is reachable).
        let mut attempted = false;
        for _ in 0..100 {
            attempted = t.state.snapshot().recent.iter().any(
                |e| matches!(&e.event, Event::BuyFailed { mint: m, .. } if *m == mint.to_string()),
            );
            if attempted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(attempted, "the copy buy was never attempted");
    }

    // ---- CopyFollow: the list and switch the dashboard edits ----

    const OWN: u8 = 250;

    fn follow() -> CopyFollow {
        CopyFollow::new(HashSet::new(), key(OWN))
    }

    #[test]
    fn follow_adds_removes_and_validates_wallets() {
        let f = follow();
        assert!(!f.active(), "nothing followed yet");
        assert!(f.enabled());

        let a = key(1).to_string();
        assert_eq!(
            f.add_wallet(&format!("  {a}  ")),
            Ok(key(1)),
            "whitespace is trimmed"
        );
        assert_eq!(f.add_wallet(&a), Ok(key(1)), "adding twice is harmless");
        assert_eq!(f.list(), vec![a.clone()]);
        assert!(f.active());

        assert!(f.add_wallet("").unwrap_err().contains("valid"));
        assert!(f
            .add_wallet("not-an-address")
            .unwrap_err()
            .contains("valid"));
        assert!(f
            .add_wallet(&key(OWN).to_string())
            .unwrap_err()
            .contains("own wallet"));
        assert_eq!(f.list().len(), 1);

        assert_eq!(f.remove_wallet(&a), Ok(true));
        assert_eq!(f.remove_wallet(&a), Ok(false), "already gone");
        assert!(f.remove_wallet("x").is_err());
        assert!(!f.active());
    }

    #[test]
    fn follow_is_active_only_when_switched_on_with_wallets() {
        let f = follow();
        f.add_wallet(&key(1).to_string()).unwrap();
        assert!(f.active());
        f.set_enabled(false);
        assert!(!f.active());
        assert!(!f.enabled());
        assert_eq!(f.list().len(), 1, "switching off keeps the list");
        f.set_enabled(true);
        assert!(f.active());
    }

    #[test]
    fn follow_caps_the_number_of_wallets_and_snapshots_are_stable() {
        let f = follow();
        for n in 0..MAX_FOLLOWED_WALLETS as u8 {
            f.add_wallet(&key(n).to_string()).unwrap();
        }
        let before = f.wallets();
        assert!(f
            .add_wallet(&key(200).to_string())
            .unwrap_err()
            .contains("at most"));
        // Re-adding one that is already there still works at the cap.
        assert!(f.add_wallet(&key(0).to_string()).is_ok());

        f.remove_wallet(&key(0).to_string()).unwrap();
        assert_eq!(
            before.len(),
            MAX_FOLLOWED_WALLETS,
            "an earlier snapshot never changes"
        );
        assert_eq!(f.wallets().len(), MAX_FOLLOWED_WALLETS - 1);
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sniper-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("copy_trade.json")
    }

    #[test]
    fn follow_saves_and_reloads_across_restarts() {
        let path = temp_file("roundtrip");
        let _ = std::fs::remove_file(&path);

        let mut first = follow();
        first.add_wallet(&key(9).to_string()).unwrap(); // startup list, before the file exists
        first.attach_store(path.clone());
        assert!(first.persisted());
        first.add_wallet(&key(1).to_string()).unwrap();
        first.add_wallet(&key(2).to_string()).unwrap();
        first.remove_wallet(&key(9).to_string()).unwrap();
        first.set_enabled(false);
        assert!(first.save_error().is_none());

        // "Restart": a new bot with a different startup list; the saved file wins.
        let mut second = CopyFollow::new([key(77)].into_iter().collect(), key(OWN));
        second.attach_store(path.clone());
        assert_eq!(second.list().len(), 2);
        assert!(second.wallets().contains(&key(1)) && second.wallets().contains(&key(2)));
        assert!(
            !second.wallets().contains(&key(77)),
            "startup list is replaced by the file"
        );
        assert!(!second.enabled(), "the switch is restored too");

        // No stray temp file is left behind.
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn follow_survives_bad_or_hostile_files_and_unwritable_paths() {
        // A corrupt file is ignored and the startup list is kept.
        let path = temp_file("corrupt");
        std::fs::write(&path, "{ not json").unwrap();
        let mut f = CopyFollow::new([key(5)].into_iter().collect(), key(OWN));
        f.attach_store(path.clone());
        assert_eq!(f.list(), vec![key(5).to_string()]);

        // Invalid addresses and the bot's own wallet inside a file are dropped, the rest kept.
        std::fs::write(
            &path,
            format!(
                r#"{{"enabled":true,"wallets":["{}","garbage","{}"]}}"#,
                key(6),
                key(OWN)
            ),
        )
        .unwrap();
        let mut g = follow();
        g.attach_store(path.clone());
        assert_eq!(g.list(), vec![key(6).to_string()]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        // Cannot write: the change still applies in memory and the error is reported.
        let mut h = follow();
        h.attach_store(PathBuf::from("/nonexistent-dir-for-test/copy_trade.json"));
        h.add_wallet(&key(1).to_string()).unwrap();
        assert!(h.active());
        assert!(h.save_error().unwrap().contains("could not save"));
    }

    #[tokio::test]
    async fn the_trading_mode_gates_copy_buys_and_leader_sells() {
        let bh = Hash::default();

        // Sell-only: a leader buy is not copied.
        let t = trader(cfg()).await;
        t.state.set_mode(TradingMode::SellOnly);
        handle_leader_trade(&t, trade(Side::Buy, key(140), 1_000_000_000), 100, bh);
        assert!(has_skip(&t, "sell-only"));

        // Buy-only: we keep a copy when the leader sells.
        let t = trader(cfg()).await;
        let copied = key(141);
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
        t.state.set_mode(TradingMode::BuyOnly);
        handle_leader_trade(&t, trade(Side::Sell, copied, 1_000_000), 100, bh);
        assert!(has_skip(&t, "buy-only"));
        assert_eq!(t.state.snapshot().positions[0].status, PositionStatus::Held);
    }
}
