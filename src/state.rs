use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::broadcast;

use crate::config::{positive_sol, Config};

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
    },
    BuyFailed {
        mint: String,
        error: String,
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

#[derive(Clone, Debug, Serialize)]
pub struct Position {
    pub mint: String,
    #[serde(skip)]
    pub mint_key: Pubkey,
    #[serde(skip)]
    pub creator: Pubkey,
    pub status: PositionStatus,
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
    pub positions: Vec<Position>,
    pub recent: Vec<Stamped>,
}

/// Single source of truth shared by the trading loop, the web API and the TUI.
/// Every state change goes through a `record_*` method, which updates the state and
/// publishes an event; nothing here ever waits on a consumer.
pub struct AppState {
    pub settings: Settings,
    wallet: String,
    started: Instant,
    slot: AtomicU64,
    balance_lamports: AtomicU64,
    has_balance: AtomicBool,
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
            wallet: wallet.to_string(),
            started: Instant::now(),
            slot: AtomicU64::new(0),
            balance_lamports: AtomicU64::new(0),
            has_balance: AtomicBool::new(false),
            stream_connected: AtomicBool::new(false),
            positions: Mutex::new(Vec::new()),
            recent: Mutex::new(VecDeque::with_capacity(MAX_RECENT_EVENTS)),
            bus,
        }
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
                opened_ms: now_ms(),
                sol,
                sell_at_slot,
                buy_signature: Some(signature.to_string()),
                sell_signature: None,
                error: None,
            });
        }
        self.publish(Event::BuySent {
            mint: mint.to_string(),
            signature: signature.to_string(),
            sol,
            sell_at_slot,
        });
    }

    pub fn record_buy_failed(&self, mint: &Pubkey, error: &str) {
        self.publish(Event::BuyFailed {
            mint: mint.to_string(),
            error: clip(error),
        });
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
            balance_lamports: self
                .has_balance
                .load(Ordering::Relaxed)
                .then(|| self.balance_lamports.load(Ordering::Relaxed)),
            stream_connected: self.stream_connected.load(Ordering::Relaxed),
            uptime_secs: self.started.elapsed().as_secs(),
            settings: self.settings.view(),
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
