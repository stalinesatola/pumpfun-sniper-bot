//! "New coins with an active community" buy rule.
//!
//! The bot only buys tokens whose creation it saw live on the Shredstream. After a token is
//! created it is watched for a short window; the buy goes out only if enough *different*
//! wallets bought it in that window (and enough SOL changed hands). Everything is read from
//! the same feed the sniper already uses, so there is no external service to depend on.
//!
//! Limits worth knowing: the amounts are the SOL *caps* of the buy instructions (what the
//! buyer was willing to pay, not what they paid), and one person with many wallets can fake
//! unique buyers. It filters out dead launches; it does not prove a real community.
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use anyhow::{bail, Result};
use log::info;
use serde::Serialize;
use solana_sdk::hash::Hash;
use solana_sdk::message::compiled_instruction::CompiledInstruction;
use solana_sdk::pubkey::Pubkey;

use crate::config::optional_parsed;
use crate::pump::parse::{self, Op, Quote, Side};
use crate::risk::{lamports_to_sol, Candidate};
use crate::state::{PositionSource, TradingMode};
use crate::trader::{BuyRequest, Trader};
use crate::types::BuyAmounts;

/// Unique buyers remembered per token; thresholds above this are rejected at startup.
const MAX_BUYERS_TRACKED: usize = 128;
/// Tokens watched at once. New ones are ignored beyond this.
const MAX_TRACKED_TOKENS: usize = 5_000;
/// Extra slots a token is kept after it could no longer be bought, then forgotten.
const GRACE_SLOTS: u64 = 20;
const MAX_WINDOW_SLOTS: u64 = 100;

#[derive(Clone, Debug)]
pub struct CommunityConfig {
    /// Wait for community activity before buying. When off, new tokens are bought as soon
    /// as they are created (the old sniper behaviour).
    pub require_community: bool,
    /// How long to watch a new token before deciding.
    pub window_slots: u64,
    /// Different wallets (not the creator) that must have bought within the window.
    pub min_buyers: usize,
    /// SOL (sum of the buys' caps) that must have been bid within the window, for tokens
    /// paired with SOL.
    pub min_volume_sol: f64,
    /// The same threshold for tokens paired with USDC.
    pub min_volume_usdc: f64,
    /// Copy-trade only follows buys of tokens at most this many slots old (0 = no limit).
    pub max_token_age_slots: u64,
}

impl Default for CommunityConfig {
    fn default() -> Self {
        Self {
            require_community: true,
            window_slots: 10,
            min_buyers: 5,
            min_volume_sol: 0.5,
            min_volume_usdc: 75.0,
            max_token_age_slots: 150,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CommunityView {
    pub require_community: bool,
    pub window_slots: u64,
    pub min_buyers: usize,
    pub min_volume_sol: f64,
    pub min_volume_usdc: f64,
    pub max_token_age_slots: u64,
    pub watching: usize,
}

impl CommunityConfig {
    pub fn from_env() -> Result<Self> {
        let d = Self::default();
        let cfg = Self {
            require_community: optional_parsed("REQUIRE_COMMUNITY", d.require_community)?,
            window_slots: optional_parsed("COMMUNITY_WINDOW_SLOTS", d.window_slots)?,
            min_buyers: optional_parsed("COMMUNITY_MIN_BUYERS", d.min_buyers)?,
            min_volume_sol: optional_parsed("COMMUNITY_MIN_VOLUME_SOL", d.min_volume_sol)?,
            min_volume_usdc: optional_parsed("COMMUNITY_MIN_VOLUME_USDC", d.min_volume_usdc)?,
            max_token_age_slots: optional_parsed("MAX_TOKEN_AGE_SLOTS", d.max_token_age_slots)?,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if !self.min_volume_sol.is_finite() || self.min_volume_sol < 0.0 {
            bail!(
                "COMMUNITY_MIN_VOLUME_SOL must be 0 (off) or positive, got {}",
                self.min_volume_sol
            );
        }
        if !self.min_volume_usdc.is_finite() || self.min_volume_usdc < 0.0 {
            bail!(
                "COMMUNITY_MIN_VOLUME_USDC must be 0 (off) or positive, got {}",
                self.min_volume_usdc
            );
        }
        if self.require_community {
            if !(1..=MAX_WINDOW_SLOTS).contains(&self.window_slots) {
                bail!("COMMUNITY_WINDOW_SLOTS must be between 1 and {MAX_WINDOW_SLOTS}");
            }
            if !(1..=MAX_BUYERS_TRACKED - 28).contains(&self.min_buyers) {
                bail!(
                    "COMMUNITY_MIN_BUYERS must be between 1 and {}",
                    MAX_BUYERS_TRACKED - 28
                );
            }
            if self.max_token_age_slots != 0 && self.max_token_age_slots < self.window_slots {
                bail!("MAX_TOKEN_AGE_SLOTS cannot be smaller than COMMUNITY_WINDOW_SLOTS");
            }
        }
        Ok(())
    }

    fn evaluate(&self, t: &Tracked) -> Result<(), String> {
        if !self.require_community {
            return Ok(());
        }
        let buyers = t.buyers.len();
        if buyers < self.min_buyers {
            return Err(format!(
                "community: {buyers} of {} unique buyers within {} slots",
                self.min_buyers, self.window_slots
            ));
        }
        match t.quote {
            Quote::Usdc => {
                let volume = t.volume as f64 / 1e6;
                if self.min_volume_usdc > 0.0 && volume < self.min_volume_usdc {
                    return Err(format!(
                        "community: {volume:.2} of {} USDC bid within {} slots",
                        self.min_volume_usdc, self.window_slots
                    ));
                }
            }
            _ => {
                let volume = lamports_to_sol(t.volume);
                if self.min_volume_sol > 0.0 && volume < self.min_volume_sol {
                    return Err(format!(
                        "community: {volume:.2} of {} SOL bid within {} slots",
                        self.min_volume_sol, self.window_slots
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The most recent buy seen on a token: its price and a blockhash that is still fresh.
#[derive(Clone, Copy, Debug)]
pub struct LastBuy {
    pub token_amount: u64,
    pub sol_amount: u64,
    pub blockhash: Hash,
}

/// A token whose creation was just seen.
pub struct NewToken {
    pub mint: Pubkey,
    /// What the token is priced in.
    pub quote: Quote,
    pub creator_wallet: Pubkey,
    /// Known from the creator's own launch buy; otherwise learned from the first buyer.
    pub creator_vault: Option<Pubkey>,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub dev_buy_sol: f64,
    pub slot: u64,
    /// The creator's launch buy, if it had one.
    pub reference: Option<LastBuy>,
}

/// A buy of a Pump.fun token by anyone, read from a transaction on the feed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObservedBuy {
    pub mint: Pubkey,
    pub buyer: Pubkey,
    pub quote: Quote,
    /// Newer instructions do not carry the creator vault.
    pub creator_vault: Option<Pubkey>,
    pub token_amount: u64,
    /// Quote units: lamports for SOL, millionths for USDC.
    pub sol_amount: u64,
}

struct Tracked {
    quote: Quote,
    created_slot: u64,
    creator_wallet: Pubkey,
    creator_vault: Option<Pubkey>,
    name: Option<String>,
    symbol: Option<String>,
    dev_buy_sol: f64,
    buyers: HashSet<Pubkey>,
    /// Quote units bid, in the token's own quote.
    volume: u64,
    last_buy: Option<LastBuy>,
    decided: bool,
}

/// A token whose watch window ended.
pub struct Due {
    pub mint: Pubkey,
    pub quote: Quote,
    pub creator_wallet: Pubkey,
    pub creator_vault: Option<Pubkey>,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub dev_buy_sol: f64,
    pub reference: Option<LastBuy>,
    pub unique_buyers: usize,
    pub verdict: Result<(), String>,
}

#[derive(Default)]
struct Inner {
    tokens: HashMap<Pubkey, Tracked>,
    /// (slot the decision is due, mint)
    due: BinaryHeap<Reverse<(u64, Pubkey)>>,
    /// (slot the token is forgotten, mint)
    expiry: BinaryHeap<Reverse<(u64, Pubkey)>>,
}

pub struct CommunityTracker {
    pub config: CommunityConfig,
    inner: Mutex<Inner>,
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl CommunityTracker {
    pub fn new(config: CommunityConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(Inner::default()),
        }
    }

    pub fn watching(&self) -> usize {
        lock(&self.inner).tokens.len()
    }

    pub fn view(&self) -> CommunityView {
        CommunityView {
            require_community: self.config.require_community,
            window_slots: self.config.window_slots,
            min_buyers: self.config.min_buyers,
            min_volume_sol: self.config.min_volume_sol,
            min_volume_usdc: self.config.min_volume_usdc,
            max_token_age_slots: self.config.max_token_age_slots,
            watching: self.watching(),
        }
    }

    /// Starts watching a token. The decision is due `window_slots` later, or right away when
    /// the community rule is off.
    pub fn track(&self, t: NewToken) {
        let mut inner = lock(&self.inner);
        if inner.tokens.len() >= MAX_TRACKED_TOKENS || inner.tokens.contains_key(&t.mint) {
            return;
        }
        let window = if self.config.require_community {
            self.config.window_slots
        } else {
            0
        };
        let keep = self.config.max_token_age_slots.max(window) + GRACE_SLOTS;
        inner.due.push(Reverse((t.slot + window, t.mint)));
        inner.expiry.push(Reverse((t.slot + keep, t.mint)));
        inner.tokens.insert(
            t.mint,
            Tracked {
                quote: t.quote,
                created_slot: t.slot,
                creator_wallet: t.creator_wallet,
                creator_vault: t.creator_vault,
                name: t.name,
                symbol: t.symbol,
                dev_buy_sol: t.dev_buy_sol,
                buyers: HashSet::new(),
                volume: 0,
                last_buy: t.reference,
                decided: false,
            },
        );
    }

    /// Counts a buy towards a watched token. Buys of tokens we are not watching, and buys by
    /// the creator's own wallet, never count as community.
    pub fn observe_buy(&self, buy: &ObservedBuy, blockhash: Hash) {
        let mut inner = lock(&self.inner);
        let Some(t) = inner.tokens.get_mut(&buy.mint) else {
            return;
        };
        if let Some(v) = buy.creator_vault {
            t.creator_vault.get_or_insert(v);
        }
        if buy.quote != t.quote {
            return;
        }
        // Later buys give a fresher price than the creator's launch buy.
        t.last_buy = Some(LastBuy {
            token_amount: buy.token_amount,
            sol_amount: buy.sol_amount,
            blockhash,
        });
        if buy.buyer == t.creator_wallet {
            return;
        }
        if t.buyers.len() < MAX_BUYERS_TRACKED {
            t.buyers.insert(buy.buyer);
        }
        t.volume = t.volume.saturating_add(buy.sol_amount);
    }

    /// Tokens whose window ended by `slot`, each returned once, and forgets expired ones.
    pub fn take_due(&self, slot: u64) -> Vec<Due> {
        let mut inner = lock(&self.inner);
        let mut out = Vec::new();
        while let Some(Reverse((due_slot, mint))) = inner.due.peek().copied() {
            if due_slot > slot {
                break;
            }
            inner.due.pop();
            let Some(t) = inner.tokens.get_mut(&mint) else {
                continue;
            };
            if t.decided {
                continue;
            }
            t.decided = true;
            out.push(Due {
                mint,
                quote: t.quote,
                creator_wallet: t.creator_wallet,
                creator_vault: t.creator_vault,
                name: t.name.clone(),
                symbol: t.symbol.clone(),
                dev_buy_sol: t.dev_buy_sol,
                reference: t.last_buy,
                unique_buyers: t.buyers.len(),
                verdict: self.config.evaluate(t),
            });
        }
        while let Some(Reverse((expire_slot, mint))) = inner.expiry.peek().copied() {
            if expire_slot >= slot {
                break;
            }
            inner.expiry.pop();
            inner.tokens.remove(&mint);
        }
        out
    }

    /// Gate for copy-trade: the token must be new (its creation was seen live and is not too
    /// old) and, if the community rule is on, already have an active community. Returns the
    /// creator's wallet when known.
    pub fn check_for_copy(&self, mint: &Pubkey, slot: u64) -> Result<Option<Pubkey>, String> {
        let cfg = &self.config;
        if cfg.max_token_age_slots == 0 && !cfg.require_community {
            return Ok(lock(&self.inner).tokens.get(mint).map(|t| t.creator_wallet));
        }
        let inner = lock(&self.inner);
        let Some(t) = inner.tokens.get(mint) else {
            return Err("not a new token (its creation was not seen)".into());
        };
        let age = slot.saturating_sub(t.created_slot);
        if cfg.max_token_age_slots != 0 && age > cfg.max_token_age_slots {
            return Err(format!(
                "token is {age} slots old (max {})",
                cfg.max_token_age_slots
            ));
        }
        cfg.evaluate(t)?;
        Ok(Some(t.creator_wallet))
    }
}

/// Every Pump.fun buy in a transaction, by anyone, whichever generation of instruction.
pub fn pump_buys<'a>(
    account_keys: &'a [Pubkey],
    instructions: &'a [CompiledInstruction],
) -> impl Iterator<Item = ObservedBuy> + 'a {
    parse::ops(account_keys, instructions).filter_map(|op| match op {
        Op::Trade(t) if t.side == Side::Buy => Some(ObservedBuy {
            mint: t.mint,
            buyer: t.user,
            quote: t.quote,
            creator_vault: t.creator_vault,
            token_amount: t.token_amount,
            sol_amount: t.quote_amount,
        }),
        _ => None,
    })
}

/// Decides every token whose window just ended: buy it if its community is active and the
/// risk limits allow, otherwise record why it was skipped. Cheap when nothing is due, so it
/// is called on every feed message.
pub fn handle_due(trader: &Trader, slot: u64) {
    for due in trader.state.community.take_due(slot) {
        decide(trader, due, slot);
    }
}

fn decide(trader: &Trader, due: Due, slot: u64) {
    let state = &trader.state;
    let skip = |reason: &str| {
        info!("Skipping {}: {}", due.mint, reason);
        state.record_skipped(&due.mint, reason);
    };

    if let Err(reason) = &due.verdict {
        return skip(reason);
    }
    if state.settings.paused() {
        return skip("bot is paused");
    }
    if !state.settings.mode().allows_buys() {
        return skip(TradingMode::BUY_REFUSED);
    }
    if due.quote != Quote::Sol {
        return skip("token is paired with USDC; buying it is not supported yet");
    }
    let (Some(reference), Some(creator_vault)) = (due.reference, due.creator_vault) else {
        return skip("no buy seen to price the order from");
    };

    // Settings can change from the dashboard at any time; read them per buy.
    let buy_sol = state.settings.buy_amount_sol();
    let tip_sol = state.settings.bribe_fee_sol();

    let candidate = Candidate {
        creator: &due.creator_wallet,
        name: due.name.as_deref(),
        symbol: due.symbol.as_deref(),
        dev_buy_sol: due.dev_buy_sol,
    };
    if let Err(reason) = state
        .risk
        .check(&candidate, &state.exposure(), buy_sol, tip_sol)
    {
        return skip(&reason);
    }
    // Reserve the mint and the SOL before the buy is sent, so tokens decided in the same
    // slot already see each other in the risk limits.
    if !state.claim_mint(&due.mint, buy_sol) {
        return skip("already holding or buying this token");
    }

    info!(
        "Sniping {} (creator {}, {} unique buyers)",
        due.mint, due.creator_wallet, due.unique_buyers
    );
    trader.spawn_buy(BuyRequest {
        mint: due.mint,
        creator_vault,
        sol: buy_sol,
        tip_sol,
        reference: BuyAmounts {
            base_amount_out: reference.token_amount,
            max_quote_amount_in: reference.sol_amount,
        },
        blockhash: reference.blockhash,
        sell_at_slot: slot + state.settings.sell_delay_slots(),
        source: PositionSource::Snipe,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PUMP_FUN_PROGRAM_ID, PUMP_ORDER_BUY};

    fn key(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    fn cfg() -> CommunityConfig {
        CommunityConfig {
            require_community: true,
            window_slots: 10,
            min_buyers: 3,
            min_volume_sol: 0.5,
            min_volume_usdc: 75.0,
            max_token_age_slots: 100,
        }
    }

    fn new_token(mint: u8, slot: u64) -> NewToken {
        NewToken {
            mint: key(mint),
            quote: Quote::Sol,
            creator_wallet: key(200),
            creator_vault: None,
            name: Some("Moon Cat".into()),
            symbol: Some("MCAT".into()),
            dev_buy_sol: 1.0,
            slot,
            reference: None,
        }
    }

    fn buy(mint: u8, buyer: u8, sol: u64) -> ObservedBuy {
        ObservedBuy {
            mint: key(mint),
            buyer: key(buyer),
            quote: Quote::Sol,
            creator_vault: Some(key(99)),
            token_amount: 1_000,
            sol_amount: sol,
        }
    }

    const SOL: u64 = 1_000_000_000;

    #[test]
    fn config_validation() {
        assert!(CommunityConfig::default().validate().is_ok());
        let bad = |c: CommunityConfig| c.validate().is_err();
        assert!(bad(CommunityConfig {
            window_slots: 0,
            ..cfg()
        }));
        assert!(bad(CommunityConfig {
            window_slots: 101,
            ..cfg()
        }));
        assert!(bad(CommunityConfig {
            min_buyers: 0,
            ..cfg()
        }));
        assert!(bad(CommunityConfig {
            min_buyers: 101,
            ..cfg()
        }));
        assert!(bad(CommunityConfig {
            min_volume_sol: -1.0,
            min_volume_usdc: 75.0,
            ..cfg()
        }));
        assert!(bad(CommunityConfig {
            min_volume_sol: f64::NAN,
            min_volume_usdc: 75.0,
            ..cfg()
        }));
        assert!(bad(CommunityConfig {
            max_token_age_slots: 5,
            ..cfg()
        }));
        // 0 = no age limit, and nothing is checked when the rule is off.
        assert!(CommunityConfig {
            max_token_age_slots: 0,
            ..cfg()
        }
        .validate()
        .is_ok());
        assert!(CommunityConfig {
            require_community: false,
            window_slots: 0,
            min_buyers: 0,
            ..cfg()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn decision_waits_for_the_window_and_is_made_once() {
        let t = CommunityTracker::new(cfg());
        t.track(new_token(1, 100));
        for b in 1..=3 {
            t.observe_buy(&buy(1, b, SOL), Hash::default());
        }
        assert!(t.take_due(109).is_empty(), "window not over yet");
        let due = t.take_due(110);
        assert_eq!(due.len(), 1);
        assert!(due[0].verdict.is_ok());
        assert_eq!(due[0].unique_buyers, 3);
        assert!(t.take_due(111).is_empty(), "a token is decided only once");
    }

    #[test]
    fn needs_enough_different_buyers_and_volume() {
        let t = CommunityTracker::new(cfg());
        t.track(new_token(1, 0));
        t.track(new_token(2, 0));
        t.track(new_token(3, 0));

        // Token 1: three buyers but too little SOL.
        for b in 1..=3 {
            t.observe_buy(&buy(1, b, SOL / 100), Hash::default());
        }
        // Token 2: lots of SOL from a single wallet buying repeatedly.
        for _ in 0..10 {
            t.observe_buy(&buy(2, 1, SOL), Hash::default());
        }
        // Token 3: three buyers and enough SOL.
        for b in 1..=3 {
            t.observe_buy(&buy(3, b, SOL), Hash::default());
        }

        let due = t.take_due(10);
        let verdict = |m: u8| {
            due.iter()
                .find(|d| d.mint == key(m))
                .unwrap()
                .verdict
                .clone()
        };
        assert!(verdict(1).unwrap_err().contains("SOL bid"));
        assert!(verdict(2).unwrap_err().contains("1 of 3 unique buyers"));
        assert!(verdict(3).is_ok());
    }

    #[test]
    fn creator_buys_and_unwatched_tokens_never_count() {
        let t = CommunityTracker::new(cfg());
        t.track(new_token(1, 0));
        // The creator's own wallet buying more is not community.
        t.observe_buy(&buy(1, 200, 5 * SOL), Hash::default());
        // A token we never saw created is ignored entirely.
        t.observe_buy(&buy(7, 1, SOL), Hash::default());
        let due = t.take_due(10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].unique_buyers, 0);
        assert!(due[0].verdict.is_err());
        assert_eq!(t.watching(), 1);
    }

    #[test]
    fn the_latest_buy_prices_the_order_and_the_vault_is_learned() {
        let t = CommunityTracker::new(cfg());
        let mut nt = new_token(1, 0);
        nt.reference = Some(LastBuy {
            token_amount: 1,
            sol_amount: 2,
            blockhash: Hash::new_from_array([1; 32]),
        });
        t.track(nt);
        let mut b = buy(1, 1, 3 * SOL);
        b.token_amount = 777;
        t.observe_buy(&b, Hash::new_from_array([9; 32]));
        let due = t.take_due(10);
        let r = due[0].reference.unwrap();
        assert_eq!((r.token_amount, r.sol_amount), (777, 3 * SOL));
        assert_eq!(r.blockhash, Hash::new_from_array([9; 32]));
        assert_eq!(due[0].creator_vault, Some(key(99)));
    }

    #[test]
    fn rule_off_decides_immediately_without_checks() {
        let t = CommunityTracker::new(CommunityConfig {
            require_community: false,
            ..cfg()
        });
        t.track(new_token(1, 50));
        let due = t.take_due(50);
        assert_eq!(due.len(), 1, "due in the same slot");
        assert!(due[0].verdict.is_ok());
    }

    #[test]
    fn old_tokens_are_forgotten_and_duplicates_ignored() {
        let t = CommunityTracker::new(cfg());
        t.track(new_token(1, 0));
        t.track(new_token(1, 5)); // same mint again
        assert_eq!(t.watching(), 1);
        t.take_due(10);
        assert_eq!(t.watching(), 1);
        // keep = max_age (100) + grace (20) slots after creation
        t.take_due(120);
        assert_eq!(t.watching(), 1);
        t.take_due(121);
        assert_eq!(t.watching(), 0);
    }

    #[test]
    fn copy_gate_requires_new_and_active_tokens() {
        let t = CommunityTracker::new(cfg());
        let unknown = key(50);
        assert!(t
            .check_for_copy(&unknown, 10)
            .unwrap_err()
            .contains("not a new token"));

        t.track(new_token(1, 100));
        // New but no community yet.
        assert!(t
            .check_for_copy(&key(1), 105)
            .unwrap_err()
            .contains("community"));
        for b in 1..=3 {
            t.observe_buy(&buy(1, b, SOL), Hash::default());
        }
        assert_eq!(t.check_for_copy(&key(1), 105), Ok(Some(key(200))));
        // Too old, whatever its community.
        assert!(t
            .check_for_copy(&key(1), 201)
            .unwrap_err()
            .contains("slots old"));

        // Both rules off: anything goes.
        let open = CommunityTracker::new(CommunityConfig {
            require_community: false,
            max_token_age_slots: 0,
            ..cfg()
        });
        assert_eq!(open.check_for_copy(&unknown, 10), Ok(None));
    }

    #[test]
    fn reads_every_buy_in_a_transaction_and_survives_garbage() {
        let keys: Vec<Pubkey> = std::iter::once(PUMP_FUN_PROGRAM_ID)
            .chain((1..=12).map(key))
            .collect();
        let mut data = PUMP_ORDER_BUY.to_vec();
        data.extend_from_slice(&5u64.to_le_bytes());
        data.extend_from_slice(&7u64.to_le_bytes());
        let good = CompiledInstruction::new_from_raw_parts(0, data.clone(), (1..=12).collect());
        let other_program =
            CompiledInstruction::new_from_raw_parts(3, data.clone(), (1..=12).collect());
        let short =
            CompiledInstruction::new_from_raw_parts(0, PUMP_ORDER_BUY.to_vec(), (1..=12).collect());
        let few = CompiledInstruction::new_from_raw_parts(0, data.clone(), vec![1, 2]);
        let bad_index = CompiledInstruction::new_from_raw_parts(0, data, vec![250; 12]);

        let ixs = [other_program, short, few, bad_index, good.clone(), good];
        let found: Vec<_> = pump_buys(&keys, &ixs).collect();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].mint, key(3));
        assert_eq!(found[0].buyer, key(7));
        assert_eq!(found[0].creator_vault, Some(key(10)));
        assert_eq!((found[0].token_amount, found[0].sol_amount), (5, 7));
    }

    // ---- the sniper's decision, end to end (no RPC is reachable, so buys fail) ----

    use crate::pumpfun::PumpFunSDK;
    use crate::pumpswap::constants::PriorityFeePump;
    use crate::state::{AppState, Event, Settings};
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;
    use std::sync::Arc;
    use std::time::Duration;

    async fn trader(config: CommunityConfig) -> Trader {
        let payer = Keypair::new();
        let state = Arc::new(
            AppState::new(payer.pubkey(), Settings::new(0.001, 0.001, 7)).with_community(config),
        );
        let sdk = PumpFunSDK::new(
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
            PriorityFeePump::default(),
        )
        .await;
        Trader::new(sdk, payer, state)
    }

    fn skipped(t: &Trader, needle: &str) -> bool {
        t.state
            .snapshot()
            .recent
            .iter()
            .any(|e| matches!(&e.event, Event::Skipped { reason, .. } if reason.contains(needle)))
    }

    async fn buy_attempted(t: &Trader, mint: Pubkey) -> bool {
        buy_attempted_within(t, mint, 100).await
    }

    async fn buy_attempted_within(t: &Trader, mint: Pubkey, tries: u32) -> bool {
        for _ in 0..tries {
            if t.state.snapshot().recent.iter().any(
                |e| matches!(&e.event, Event::BuyFailed { mint: m, .. } if *m == mint.to_string()),
            ) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    fn launch_with_dev_buy(t: &Trader, mint: u8, slot: u64) {
        t.state.community.track(NewToken {
            mint: key(mint),
            quote: Quote::Sol,
            creator_wallet: key(200),
            creator_vault: Some(key(99)),
            name: Some("Moon Cat".into()),
            symbol: Some("MCAT".into()),
            dev_buy_sol: 1.0,
            slot,
            reference: Some(LastBuy {
                token_amount: 1_000_000,
                sol_amount: SOL,
                blockhash: Hash::default(),
            }),
        });
    }

    #[tokio::test]
    async fn only_tokens_with_an_active_community_are_bought_after_the_window() {
        let t = trader(cfg()).await;
        launch_with_dev_buy(&t, 1, 100); // will have a community
        launch_with_dev_buy(&t, 2, 100); // will not

        for b in 1..=3 {
            t.state
                .community
                .observe_buy(&buy(1, b, SOL), Hash::default());
        }
        t.state
            .community
            .observe_buy(&buy(2, 1, SOL), Hash::default());

        handle_due(&t, 105);
        assert!(
            t.state.snapshot().recent.is_empty(),
            "nothing is decided inside the window"
        );

        handle_due(&t, 110);
        assert!(
            buy_attempted(&t, key(1)).await,
            "the active token must be bought"
        );
        assert!(
            skipped(&t, "1 of 3 unique buyers"),
            "the quiet token must be skipped"
        );
        assert!(!buy_attempted_within(&t, key(2), 4).await);
    }

    #[tokio::test]
    async fn with_the_rule_off_a_new_token_is_bought_at_once() {
        let t = trader(CommunityConfig {
            require_community: false,
            ..cfg()
        })
        .await;
        launch_with_dev_buy(&t, 1, 100);
        handle_due(&t, 100);
        assert!(buy_attempted(&t, key(1)).await);
    }

    #[tokio::test]
    async fn pause_risk_limits_and_missing_prices_stop_the_buy() {
        let t = trader(CommunityConfig {
            require_community: false,
            ..cfg()
        })
        .await;

        t.state.set_paused(true);
        launch_with_dev_buy(&t, 1, 100);
        handle_due(&t, 100);
        assert!(skipped(&t, "paused"));
        t.state.set_paused(false);

        // A launch without a creator buy has nothing to price the order from.
        t.state.community.track(NewToken {
            mint: key(2),
            quote: Quote::Sol,
            creator_wallet: key(200),
            creator_vault: None,
            name: None,
            symbol: None,
            dev_buy_sol: 0.0,
            slot: 100,
            reference: None,
        });
        handle_due(&t, 100);
        assert!(skipped(&t, "no buy seen"));

        // The blocked-keyword list still applies, using the token name read from the launch.
        let mut blocked = trader(CommunityConfig {
            require_community: false,
            ..cfg()
        })
        .await;
        Arc::get_mut(&mut blocked.state)
            .unwrap()
            .risk
            .blocked_keywords = vec!["cat".into()];
        launch_with_dev_buy(&blocked, 3, 100);
        handle_due(&blocked, 100);
        assert!(skipped(&blocked, "blocked keyword"));
    }

    #[tokio::test]
    async fn sell_only_mode_opens_no_new_positions() {
        let t = trader(CommunityConfig {
            require_community: false,
            ..cfg()
        })
        .await;
        t.state.set_mode(TradingMode::SellOnly);
        launch_with_dev_buy(&t, 1, 100);
        handle_due(&t, 100);
        assert!(skipped(&t, "sell-only"));
        assert!(!buy_attempted_within(&t, key(1), 4).await);

        t.state.set_mode(TradingMode::BuyOnly);
        launch_with_dev_buy(&t, 2, 100);
        handle_due(&t, 100);
        assert!(buy_attempted(&t, key(2)).await, "buy-only still buys");
    }
}
