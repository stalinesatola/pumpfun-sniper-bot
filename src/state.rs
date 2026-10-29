use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::broadcast;

use crate::community::{CommunityConfig, CommunityTracker, CommunityView};
use crate::config::{positive_sol, Config};
use crate::copy::{CopyConfig, CopyView, LeaderTrade, Side};
use crate::risk::{lamports_to_sol, Exposure, RiskConfig, RiskView};

const MAX_RECENT_EVENTS: usize = 200;
const MAX_POSITIONS: usize = 500;
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Safety rails for values changed at runtime from the UI/API.
pub const MAX_BUY_AMOUNT_SOL: f64 = 10.0;
pub const MAX_BRIBE_FEE_SOL: f64 = 0.1;
pub const MAX_SELL_DELAY_SLOTS: u64 = 200;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// RPC errors can repeat the same message several times; keep what the UI shows readable.
fn clip(error: &str) -> String {
    const MAX: usize = 200;
    if error.chars().count() <= MAX {
        error.to_string()
    } else {
        let head: String = error.chars().take(MAX).collect();
        format!("{head}…")
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Something that happened in the bot. Pubkeys and signatures are strings so the same
/// payload can go straight to the web UI as JSON.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    CreateDetected {
        mint: String,
        creator: String,
        signature: String,
        slot: u64,
    },
    BuySent {
        mint: String,
        signature: String,
        sol: f64,
        sell_at_slot: u64,
        source: PositionSource,
    },
    BuyFailed {
        mint: String,
        error: String,
    },
    Skipped {
        mint: String,
        reason: String,
    },
    CopySignal {
        leader: String,
        mint: String,
        side: Side,
        sol: f64,
        slot: u64,
    },
    SellStarted {
        mint: String,
    },
    SellSent {
        mint: String,
        signature: String,
        amount: u64,
    },
    SellFailed {
        mint: String,
        error: String,
    },
    Balance {
        lamports: u64,
    },
    Stream {
        connected: bool,
    },
    Paused {
        paused: bool,
    },
    SettingsChanged {
        settings: SettingsView,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct Stamped {
    pub ts_ms: u64,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionStatus {
    /// Buy sent, waiting for the sell slot.
    Held,
    Selling,
    Sold,
    /// Buy never landed or the sell kept failing; check the wallet.
    Failed,
}

/// Why we hold a position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PositionSource {
    /// Bought at token creation.
    Snipe,
    /// Mirrored a followed wallet's buy.
    Copy { leader: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct Position {
    pub mint: String,
    #[serde(skip)]
    pub mint_key: Pubkey,
    #[serde(skip)]
    pub creator: Pubkey,
    pub status: PositionStatus,
    pub source: PositionSource,
    pub opened_ms: u64,
    pub sol: f64,
    pub sell_at_slot: u64,
    pub buy_signature: Option<String>,
    pub sell_signature: Option<String>,
    pub error: Option<String>,
}

/// Values the operator may change while the bot runs.
pub struct Settings {
    buy_amount_sol: AtomicU64,
    bribe_fee_sol: AtomicU64,
    sell_delay_slots: AtomicU64,
    paused: AtomicBool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SettingsView {
    pub buy_amount_sol: f64,
    pub bribe_fee_sol: f64,
    pub sell_delay_slots: u64,
    pub paused: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SettingsUpdate {
    pub buy_amount_sol: Option<f64>,
    pub bribe_fee_sol: Option<f64>,
    pub sell_delay_slots: Option<u64>,
}

impl Settings {
    pub fn new(buy_amount_sol: f64, bribe_fee_sol: f64, sell_delay_slots: u64) -> Self {
        Self {
            buy_amount_sol: AtomicU64::new(buy_amount_sol.to_bits()),
            bribe_fee_sol: AtomicU64::new(bribe_fee_sol.to_bits()),
            sell_delay_slots: AtomicU64::new(sell_delay_slots),
            paused: AtomicBool::new(false),
        }
    }

    pub fn from_config(config: &Config) -> Self {
        Self::new(
            config.buy_amount_sol,
            config.bribe_fee,
            config.sell_delay_slots,
        )
    }

    pub fn buy_amount_sol(&self) -> f64 {
        f64::from_bits(self.buy_amount_sol.load(Ordering::Relaxed))
    }

    pub fn bribe_fee_sol(&self) -> f64 {
        f64::from_bits(self.bribe_fee_sol.load(Ordering::Relaxed))
    }

    pub fn sell_delay_slots(&self) -> u64 {
        self.sell_delay_slots.load(Ordering::Relaxed)
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn view(&self) -> SettingsView {
        SettingsView {
            buy_amount_sol: self.buy_amount_sol(),
            bribe_fee_sol: self.bribe_fee_sol(),
            sell_delay_slots: self.sell_delay_slots(),
            paused: self.paused(),
        }
    }

    /// Validates every field first, then applies them together, so a bad value never
    /// leaves the settings half updated.
    pub fn apply(&self, update: &SettingsUpdate) -> Result<(), String> {
        if let Some(v) = update.buy_amount_sol {
            positive_sol("buy_amount_sol", v).map_err(|e| e.to_string())?;
            if v > MAX_BUY_AMOUNT_SOL {
                return Err(format!(
                    "buy_amount_sol must be at most {MAX_BUY_AMOUNT_SOL}"
                ));
            }
        }
        if let Some(v) = update.bribe_fee_sol {
            positive_sol("bribe_fee_sol", v).map_err(|e| e.to_string())?;
            if v > MAX_BRIBE_FEE_SOL {
                return Err(format!("bribe_fee_sol must be at most {MAX_BRIBE_FEE_SOL}"));
            }
        }
        if let Some(v) = update.sell_delay_slots {
            if !(1..=MAX_SELL_DELAY_SLOTS).contains(&v) {
                return Err(format!(
                    "sell_delay_slots must be between 1 and {MAX_SELL_DELAY_SLOTS}"
                ));
            }
        }

        if let Some(v) = update.buy_amount_sol {
            self.buy_amount_sol.store(v.to_bits(), Ordering::Relaxed);
        }
        if let Some(v) = update.bribe_fee_sol {
            self.bribe_fee_sol.store(v.to_bits(), Ordering::Relaxed);
        }
        if let Some(v) = update.sell_delay_slots {
            self.sell_delay_slots.store(v, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub wallet: String,
    pub slot: u64,
    pub balance_lamports: Option<u64>,
    pub stream_connected: bool,
    pub uptime_secs: u64,
    pub settings: SettingsView,
    pub risk: RiskView,
    pub copy: CopyView,
    pub community: CommunityView,
    pub session_spent_sol: f64,
    pub drawdown_sol: Option<f64>,
    pub positions: Vec<Position>,
    pub recent: Vec<Stamped>,
}

/// Single source of truth shared by the trading loop, the web API and the TUI.
/// Every state change goes through a `record_*` method, which updates the state and
/// publishes an event; nothing here ever waits on a consumer.
pub struct AppState {
    pub settings: Settings,
    pub risk: RiskConfig,
    pub copy: CopyConfig,
    pub community: CommunityTracker,
    /// Mints a buy is currently being sent for, so two signals cannot both buy one token.
    /// SOL reserved per mint for buys being sent, counted by [`Self::exposure`].
    inflight: Mutex<HashMap<Pubkey, f64>>,
    wallet: String,
    started: Instant,
    slot: AtomicU64,
    balance_lamports: AtomicU64,
    has_balance: AtomicBool,
    start_balance_lamports: AtomicU64,
    has_start_balance: AtomicBool,
    spent_lamports: AtomicU64,
    stream_connected: AtomicBool,
    positions: Mutex<Vec<Position>>,
    recent: Mutex<VecDeque<Stamped>>,
    bus: broadcast::Sender<Stamped>,
}

impl AppState {
    pub fn new(wallet: Pubkey, settings: Settings) -> Self {
        let (bus, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            settings,
            risk: RiskConfig::default(),
            copy: CopyConfig::default(),
            community: CommunityTracker::new(CommunityConfig::default()),
            inflight: Mutex::new(HashMap::new()),
            wallet: wallet.to_string(),
            started: Instant::now(),
            slot: AtomicU64::new(0),
            balance_lamports: AtomicU64::new(0),
            has_balance: AtomicBool::new(false),
            start_balance_lamports: AtomicU64::new(0),
            has_start_balance: AtomicBool::new(false),
            spent_lamports: AtomicU64::new(0),
            stream_connected: AtomicBool::new(false),
            positions: Mutex::new(Vec::new()),
            recent: Mutex::new(VecDeque::with_capacity(MAX_RECENT_EVENTS)),
            bus,
        }
    }

    pub fn with_risk(mut self, risk: RiskConfig) -> Self {
        self.risk = risk;
        self
    }

    pub fn with_community(mut self, config: CommunityConfig) -> Self {
        self.community = CommunityTracker::new(config);
        self
    }

    pub fn with_copy(mut self, copy: CopyConfig) -> Self {
        self.copy = copy;
        self
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Stamped> {
        self.bus.subscribe()
    }

    pub fn wallet(&self) -> &str {
        &self.wallet
    }

    pub fn slot(&self) -> u64 {
        self.slot.load(Ordering::Relaxed)
    }

    pub fn set_slot(&self, slot: u64) {
        self.slot.store(slot, Ordering::Relaxed);
    }

    fn publish(&self, event: Event) {
        let stamped = Stamped {
            ts_ms: now_ms(),
            event,
        };
        {
            let mut recent = lock(&self.recent);
            if recent.len() == MAX_RECENT_EVENTS {
                recent.pop_front();
            }
            recent.push_back(stamped.clone());
        }
        // An error only means nobody is listening right now.
        let _ = self.bus.send(stamped);
    }

    pub fn record_create(&self, mint: &Pubkey, creator: &Pubkey, signature: &str, slot: u64) {
        self.publish(Event::CreateDetected {
            mint: mint.to_string(),
            creator: creator.to_string(),
            signature: signature.to_string(),
            slot,
        });
    }

    pub fn record_buy_sent(
        &self,
        mint: &Pubkey,
        creator: &Pubkey,
        signature: &str,
        sol: f64,
        sell_at_slot: u64,
    ) {
        self.record_buy_sent_from(
            mint,
            creator,
            signature,
            sol,
            sell_at_slot,
            PositionSource::Snipe,
        );
    }

    pub fn record_buy_sent_from(
        &self,
        mint: &Pubkey,
        creator: &Pubkey,
        signature: &str,
        sol: f64,
        sell_at_slot: u64,
        source: PositionSource,
    ) {
        {
            let mut positions = lock(&self.positions);
            if positions.len() >= MAX_POSITIONS {
                // Drop the oldest finished position; active ones are never evicted.
                if let Some(i) = positions
                    .iter()
                    .position(|p| matches!(p.status, PositionStatus::Sold | PositionStatus::Failed))
                {
                    positions.remove(i);
                }
            }
            positions.push(Position {
                mint: mint.to_string(),
                mint_key: *mint,
                creator: *creator,
                status: PositionStatus::Held,
                source: source.clone(),
                opened_ms: now_ms(),
                sol,
                sell_at_slot,
                buy_signature: Some(signature.to_string()),
                sell_signature: None,
                error: None,
            });
        }
        self.spent_lamports
            .fetch_add((sol * 1e9) as u64, Ordering::Relaxed);
        self.publish(Event::BuySent {
            mint: mint.to_string(),
            signature: signature.to_string(),
            sol,
            sell_at_slot,
            source,
        });
    }

    pub fn record_buy_failed(&self, mint: &Pubkey, error: &str) {
        self.publish(Event::BuyFailed {
            mint: mint.to_string(),
            error: clip(error),
        });
    }

    pub fn record_copy_signal(&self, trade: &LeaderTrade, slot: u64) {
        self.publish(Event::CopySignal {
            leader: trade.leader.to_string(),
            mint: trade.mint.to_string(),
            side: trade.side,
            sol: trade.sol(),
            slot,
        });
    }

    /// True while we hold, are selling, or failed to sell this token (it may still be in the wallet).
    pub fn has_active_position(&self, mint: &Pubkey) -> bool {
        lock(&self.positions).iter().any(|p| {
            p.mint_key == *mint
                && matches!(
                    p.status,
                    PositionStatus::Held | PositionStatus::Selling | PositionStatus::Failed
                )
        })
    }

    /// True if we hold this token or a buy for it is being sent.
    pub fn mint_busy(&self, mint: &Pubkey) -> bool {
        lock(&self.inflight).contains_key(mint) || self.has_active_position(mint)
    }

    /// Reserves `sol` and the mint for a buy about to be sent, so two signals cannot both
    /// buy one token and a burst of signals cannot slip past the risk limits before the
    /// first buy is recorded. Returns false if the mint is busy. Pair with
    /// [`Self::release_mint`] once the buy is recorded or has failed.
    pub fn claim_mint(&self, mint: &Pubkey, sol: f64) -> bool {
        let mut inflight = lock(&self.inflight);
        if inflight.contains_key(mint) || self.has_active_position(mint) {
            return false;
        }
        inflight.insert(*mint, sol);
        true
    }

    pub fn release_mint(&self, mint: &Pubkey) {
        lock(&self.inflight).remove(mint);
    }

    /// A copied position that is still waiting to be sold.
    pub fn is_copy_held(&self, mint: &Pubkey) -> bool {
        lock(&self.positions).iter().any(|p| {
            p.mint_key == *mint
                && p.status == PositionStatus::Held
                && matches!(p.source, PositionSource::Copy { .. })
        })
    }

    pub fn record_skipped(&self, mint: &Pubkey, reason: &str) {
        self.publish(Event::Skipped {
            mint: mint.to_string(),
            reason: reason.to_string(),
        });
    }

    pub fn balance_lamports(&self) -> Option<u64> {
        self.has_balance
            .load(Ordering::Relaxed)
            .then(|| self.balance_lamports.load(Ordering::Relaxed))
    }

    fn start_balance_lamports(&self) -> Option<u64> {
        self.has_start_balance
            .load(Ordering::Relaxed)
            .then(|| self.start_balance_lamports.load(Ordering::Relaxed))
    }

    pub fn session_spent_sol(&self) -> f64 {
        lamports_to_sol(self.spent_lamports.load(Ordering::Relaxed))
    }

    /// Positions that still hold tokens or are being sold.
    pub fn open_positions(&self) -> usize {
        lock(&self.positions)
            .iter()
            .filter(|p| matches!(p.status, PositionStatus::Held | PositionStatus::Selling))
            .count()
    }

    pub fn exposure(&self) -> Exposure {
        let (pending, pending_sol) = {
            let inflight = lock(&self.inflight);
            (inflight.len(), inflight.values().sum::<f64>())
        };
        Exposure {
            open_positions: self.open_positions() + pending,
            session_spent_sol: self.session_spent_sol() + pending_sol,
            balance_sol: self.balance_lamports().map(lamports_to_sol),
            start_balance_sol: self.start_balance_lamports().map(lamports_to_sol),
        }
    }

    /// Positions in `Held` whose sell slot has been reached.
    pub fn due_positions(&self, slot: u64) -> Vec<Pubkey> {
        lock(&self.positions)
            .iter()
            .filter(|p| p.status == PositionStatus::Held && slot >= p.sell_at_slot)
            .map(|p| p.mint_key)
            .collect()
    }

    /// Atomically claims a position for selling (`Held` or `Failed` -> `Selling`).
    /// Returns `None` if there is no such position or someone else already claimed it,
    /// which is what stops the slot timer and a manual sell from selling twice.
    pub fn begin_sell(&self, mint: &Pubkey) -> Option<Position> {
        let claimed = {
            let mut positions = lock(&self.positions);
            let position = positions.iter_mut().find(|p| {
                p.mint_key == *mint
                    && matches!(p.status, PositionStatus::Held | PositionStatus::Failed)
            })?;
            position.status = PositionStatus::Selling;
            position.error = None;
            position.clone()
        };
        self.publish(Event::SellStarted {
            mint: mint.to_string(),
        });
        Some(claimed)
    }

    pub fn record_sell_sent(&self, mint: &Pubkey, signature: &str, amount: u64) {
        self.update_position(mint, |p| {
            p.status = PositionStatus::Sold;
            p.sell_signature = Some(signature.to_string());
        });
        self.publish(Event::SellSent {
            mint: mint.to_string(),
            signature: signature.to_string(),
            amount,
        });
    }

    pub fn record_sell_failed(&self, mint: &Pubkey, error: &str) {
        let error = clip(error);
        self.update_position(mint, |p| {
            p.status = PositionStatus::Failed;
            p.error = Some(error.clone());
        });
        self.publish(Event::SellFailed {
            mint: mint.to_string(),
            error,
        });
    }

    fn update_position(&self, mint: &Pubkey, f: impl FnOnce(&mut Position)) {
        if let Some(p) = lock(&self.positions)
            .iter_mut()
            .find(|p| p.mint_key == *mint)
        {
            f(p);
        }
    }

    pub fn record_balance(&self, lamports: u64) {
        self.balance_lamports.store(lamports, Ordering::Relaxed);
        self.has_balance.store(true, Ordering::Relaxed);
        if !self.has_start_balance.swap(true, Ordering::Relaxed) {
            self.start_balance_lamports
                .store(lamports, Ordering::Relaxed);
        }
        self.publish(Event::Balance { lamports });
    }

    pub fn set_stream_connected(&self, connected: bool) {
        if self.stream_connected.swap(connected, Ordering::Relaxed) != connected {
            self.publish(Event::Stream { connected });
        }
    }

    pub fn set_paused(&self, paused: bool) {
        if self.settings.paused.swap(paused, Ordering::Relaxed) != paused {
            self.publish(Event::Paused { paused });
        }
    }

    pub fn update_settings(&self, update: &SettingsUpdate) -> Result<SettingsView, String> {
        self.settings.apply(update)?;
        let view = self.settings.view();
        self.publish(Event::SettingsChanged {
            settings: view.clone(),
        });
        Ok(view)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            wallet: self.wallet.clone(),
            slot: self.slot(),
            balance_lamports: self.balance_lamports(),
            stream_connected: self.stream_connected.load(Ordering::Relaxed),
            uptime_secs: self.started.elapsed().as_secs(),
            settings: self.settings.view(),
            risk: self.risk.view(),
            copy: self.copy.view(),
            community: self.community.view(),
            session_spent_sol: self.session_spent_sol(),
            drawdown_sol: match (self.start_balance_lamports(), self.balance_lamports()) {
                (Some(start), Some(now)) => Some(lamports_to_sol(start) - lamports_to_sol(now)),
                _ => None,
            },
            positions: lock(&self.positions).clone(),
            recent: lock(&self.recent).iter().cloned().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::new(Pubkey::new_unique(), Settings::new(0.001, 0.001, 7))
    }

    fn buy(state: &AppState, sell_at: u64) -> Pubkey {
        let mint = Pubkey::new_unique();
        state.record_buy_sent(&mint, &Pubkey::new_unique(), "sig", 0.001, sell_at);
        mint
    }

    #[test]
    fn settings_reject_bad_values_without_partial_update() {
        let s = Settings::new(0.001, 0.001, 7);
        let bad = SettingsUpdate {
            buy_amount_sol: Some(0.5),
            bribe_fee_sol: Some(f64::NAN),
            sell_delay_slots: None,
        };
        assert!(s.apply(&bad).is_err());
        assert_eq!(s.buy_amount_sol(), 0.001);

        assert!(s
            .apply(&SettingsUpdate {
                buy_amount_sol: Some(MAX_BUY_AMOUNT_SOL + 1.0),
                ..Default::default()
            })
            .is_err());
        assert!(s
            .apply(&SettingsUpdate {
                sell_delay_slots: Some(0),
                ..Default::default()
            })
            .is_err());
        assert!(s
            .apply(&SettingsUpdate {
                sell_delay_slots: Some(MAX_SELL_DELAY_SLOTS + 1),
                ..Default::default()
            })
            .is_err());

        let ok = SettingsUpdate {
            buy_amount_sol: Some(0.5),
            bribe_fee_sol: Some(0.002),
            sell_delay_slots: Some(9),
        };
        s.apply(&ok).unwrap();
        assert_eq!(
            (s.buy_amount_sol(), s.bribe_fee_sol(), s.sell_delay_slots()),
            (0.5, 0.002, 9)
        );
    }

    #[test]
    fn due_positions_respect_the_sell_slot() {
        let st = state();
        let mint = buy(&st, 100);
        assert!(st.due_positions(99).is_empty());
        assert_eq!(st.due_positions(100), vec![mint]);
        assert_eq!(st.due_positions(150), vec![mint]);
    }

    #[test]
    fn a_position_can_only_be_claimed_once() {
        let st = state();
        let mint = buy(&st, 1);
        assert!(st.begin_sell(&mint).is_some());
        assert!(st.begin_sell(&mint).is_none(), "second claim must lose");
        assert!(
            st.due_positions(10).is_empty(),
            "claimed positions are no longer due"
        );

        st.record_sell_failed(&mint, "boom");
        assert!(
            st.begin_sell(&mint).is_some(),
            "failed positions can be retried"
        );

        st.record_sell_sent(&mint, "sell-sig", 5);
        assert!(st.begin_sell(&mint).is_none(), "sold positions are final");
        assert_eq!(st.snapshot().positions[0].status, PositionStatus::Sold);
    }

    #[test]
    fn events_reach_subscribers_and_the_recent_buffer() {
        let st = state();
        let mut rx = st.subscribe();
        st.set_paused(true);
        st.set_paused(true); // no change, no second event
        let got = rx.try_recv().unwrap();
        assert!(matches!(got.event, Event::Paused { paused: true }));
        assert!(rx.try_recv().is_err());
        assert_eq!(st.snapshot().recent.len(), 1);

        for _ in 0..(MAX_RECENT_EVENTS + 10) {
            st.record_balance(1);
        }
        assert_eq!(st.snapshot().recent.len(), MAX_RECENT_EVENTS);
    }

    #[test]
    fn exposure_tracks_spend_open_positions_and_drawdown() {
        let st = state();
        assert_eq!(st.exposure().open_positions, 0);
        assert!(st.snapshot().drawdown_sol.is_none());

        st.record_balance(2_000_000_000);
        st.record_balance(1_750_000_000); // start balance stays the first one seen
        let mint = buy(&st, 1);
        buy(&st, 1);

        let e = st.exposure();
        assert_eq!(e.open_positions, 2);
        assert!((e.session_spent_sol - 0.002).abs() < 1e-9);
        assert_eq!(e.start_balance_sol, Some(2.0));
        assert_eq!(e.balance_sol, Some(1.75));
        assert!((st.snapshot().drawdown_sol.unwrap() - 0.25).abs() < 1e-9);

        st.begin_sell(&mint).unwrap();
        assert_eq!(st.open_positions(), 2, "selling still counts as open");
        st.record_sell_sent(&mint, "s", 1);
        assert_eq!(st.open_positions(), 1);
    }

    #[test]
    fn mint_claims_block_duplicates_and_respect_closed_positions() {
        let st = state();
        let m = Pubkey::new_unique();
        assert!(st.claim_mint(&m, 0.01));
        assert!(st.mint_busy(&m));
        assert!(
            !st.claim_mint(&m, 0.01),
            "a second buy for the same mint must lose"
        );
        st.release_mint(&m);
        assert!(!st.mint_busy(&m));
        assert!(st.claim_mint(&m, 0.01));
        st.release_mint(&m);

        st.record_buy_sent(&m, &Pubkey::new_unique(), "sig", 0.001, 1);
        assert!(st.has_active_position(&m));
        assert!(!st.claim_mint(&m, 0.01), "held tokens block a new buy");

        st.begin_sell(&m).unwrap();
        st.record_sell_failed(&m, "x");
        assert!(
            st.has_active_position(&m),
            "a failed sell may still hold tokens"
        );

        st.begin_sell(&m).unwrap();
        st.record_sell_sent(&m, "s", 1);
        assert!(!st.has_active_position(&m));
        assert!(
            st.claim_mint(&m, 0.01),
            "a sold position can be bought again"
        );
    }

    #[test]
    fn reserved_buys_count_towards_exposure_until_released() {
        let st = state();
        let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());
        assert!(st.claim_mint(&a, 0.25));
        assert!(st.claim_mint(&b, 0.5));
        let e = st.exposure();
        assert_eq!(e.open_positions, 2);
        assert!((e.session_spent_sol - 0.75).abs() < 1e-9);

        st.release_mint(&a);
        let e = st.exposure();
        assert_eq!(e.open_positions, 1);
        assert!((e.session_spent_sol - 0.5).abs() < 1e-9);
    }

    #[test]
    fn position_source_is_recorded_and_serialized() {
        let st = state();
        let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());
        st.record_buy_sent(&a, &Pubkey::new_unique(), "s", 0.001, 1);
        st.record_buy_sent_from(
            &b,
            &Pubkey::new_unique(),
            "s",
            0.001,
            1,
            PositionSource::Copy { leader: "L".into() },
        );
        assert!(!st.is_copy_held(&a));
        assert!(st.is_copy_held(&b));

        let json = serde_json::to_value(st.snapshot().positions).unwrap();
        assert_eq!(json[0]["source"]["kind"], "snipe");
        assert_eq!(json[1]["source"]["kind"], "copy");
        assert_eq!(json[1]["source"]["leader"], "L");

        st.begin_sell(&b).unwrap();
        assert!(
            !st.is_copy_held(&b),
            "only held copies are sold with the leader"
        );
    }

    #[test]
    fn long_errors_are_clipped() {
        let st = state();
        let mint = buy(&st, 1);
        st.begin_sell(&mint).unwrap();
        st.record_sell_failed(&mint, &"x".repeat(5000));
        let err = st.snapshot().positions[0].error.clone().unwrap();
        assert!(err.chars().count() <= 201);
    }

    #[test]
    fn event_json_has_a_flat_tagged_shape() {
        let st = state();
        st.record_balance(42);
        let json = serde_json::to_value(&st.snapshot().recent[0]).unwrap();
        assert_eq!(json["type"], "balance");
        assert_eq!(json["lamports"], 42);
        assert!(json["ts_ms"].is_u64());
    }
}
