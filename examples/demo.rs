//! Runs the dashboard (web API and optional `--tui`) against simulated activity, so the
//! interface can be tried without a wallet, an RPC or a Shredstream connection.
//!
//!     cargo run --example demo            # then open http://127.0.0.1:8787, token printed below
//!     cargo run --example demo -- --tui
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use shred_pool_cache::api::{self, ApiConfig};
use shred_pool_cache::copy::{CopyConfig, LeaderTrade, Side};
use shred_pool_cache::costs;
use shred_pool_cache::pump::parse::Quote;
use shred_pool_cache::pumpfun::PumpFunSDK;
use shred_pool_cache::pumpswap::constants::PriorityFeePump;
use shred_pool_cache::state::{AppState, PositionSource, Settings, TradingMode};
use shred_pool_cache::trader::Trader;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tui = std::env::args().any(|a| a == "--tui");
    let payer = Keypair::new();
    let copy = CopyConfig {
        wallets: [Pubkey::new_unique()].into_iter().collect(),
        buy_sol: 0.02,
        ..CopyConfig::default()
    };
    let mut state = AppState::new(payer.pubkey(), Settings::new(0.02, 0.0001, 7)).with_copy(copy);
    // COPY_STATE_FILE=<path> makes the demo save the followed wallets like the real bot does.
    if let Ok(path) = std::env::var("COPY_STATE_FILE") {
        state = state.with_copy_store(path.into());
    }
    // RULES_STATE_FILE=<path> saves the trading mode like the real bot does.
    if let Ok(path) = std::env::var("RULES_STATE_FILE") {
        state = state.with_rules_store(path.into());
    }
    let state = Arc::new(state);
    // Nothing listens here; the demo simulates every trade itself.
    let sdk = PumpFunSDK::new(
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
        PriorityFeePump::default(),
    )
    .await;
    let trader = Trader::new(sdk, payer, state.clone());

    let cfg = ApiConfig {
        bind: "127.0.0.1:8787".parse()?,
        token: "demo-token-demo-token".into(),
        token_generated: false,
        ui_dir: PathBuf::from("ui/dist"),
    };
    if !tui {
        println!("Dashboard: http://127.0.0.1:8787   token: {}", cfg.token);
    }
    tokio::spawn(api::serve(cfg, state.clone(), trader.clone()));

    let sim = {
        let state = state.clone();
        async move {
            // What a sold position returns, as a multiple of what was put in. Some tokens
            // die (0x), most do little, a few run, so the balance moves both ways.
            const OUTCOMES: [f64; 7] = [0.0, 0.4, 0.9, 1.5, 3.0, 0.6, 2.2];
            let fee_lamports =
                |tip_sol: f64| ((costs::fixed_fee_per_tx_sol() + tip_sol) * 1e9) as u64;

            state.set_stream_connected(true);
            let mut lamports = 2_000_000_000u64;
            state.record_balance(lamports);
            let mut slot = 300_000_000u64;
            for tick in 0u64.. {
                slot += 1;
                state.set_slot(slot);
                let tip = state.settings.bribe_fee_sol();
                let mode = state.settings.mode();

                // Simulated exits (only when the mode sells): a failure now and then, and the
                // proceeds minus the fees go back into the balance.
                if mode.allows_sells() {
                    let positions = state.snapshot().positions;
                    for (n, mint) in state.due_positions(slot).into_iter().enumerate() {
                        let Some(stake) =
                            positions.iter().find(|p| p.mint_key == mint).map(|p| p.sol)
                        else {
                            continue;
                        };
                        if state.begin_sell(&mint).is_none() {
                            continue;
                        }
                        let k = (tick + n as u64) as usize;
                        if k % 9 == 4 {
                            state.record_sell_failed(
                                &mint,
                                "RPC node returned 429 Too Many Requests",
                            );
                        } else {
                            let proceeds = (stake * OUTCOMES[k % OUTCOMES.len()] * 1e9) as u64;
                            lamports = lamports
                                .saturating_add(proceeds)
                                .saturating_sub(fee_lamports(tip));
                            state.record_sell_sent(&mint, "demo-sell-signature", 1_234_567);
                        }
                    }
                }

                if tick % 6 == 0 && !state.settings.paused() {
                    let (mint, creator) = (Pubkey::new_unique(), Pubkey::new_unique());
                    state.record_create(&mint, &creator, "demo-signature", slot);
                    if tick % 12 == 0 {
                        if mode.allows_buys() {
                            let sol = state.settings.buy_amount_sol();
                            state.record_buy_sent(
                                &mint,
                                &creator,
                                "demo-buy-signature",
                                sol,
                                slot + state.settings.sell_delay_slots(),
                            );
                            lamports = lamports
                                .saturating_sub((sol * 1e9) as u64)
                                .saturating_sub(fee_lamports(tip));
                        } else {
                            state.record_skipped(&mint, TradingMode::BUY_REFUSED);
                        }
                    }
                }

                // A followed wallet buys now and then; we mirror it. Only while copy-trade is
                // on, and only for the wallets currently in the list (edit them in the dashboard).
                let followed = state.follow.wallets();
                let leader = followed.iter().next().copied();
                if let Some(leader) = leader.filter(|_| state.follow.active()) {
                    if tick % 14 == 5 && !state.settings.paused() {
                        let mint = Pubkey::new_unique();
                        let trade = LeaderTrade {
                            side: Side::Buy,
                            leader,
                            mint,
                            creator_vault: Some(Pubkey::new_unique()),
                            quote: Quote::Sol,
                            token_amount: 1_000_000,
                            sol_amount: 1_500_000_000,
                        };
                        state.record_copy_signal(&trade, slot);
                        if mode.allows_buys() {
                            let sol = state.copy.buy_sol;
                            state.record_buy_sent_from(
                                &mint,
                                &trade.creator_vault.unwrap(),
                                "demo-copy-signature",
                                sol,
                                slot + 12,
                                PositionSource::Copy {
                                    leader: leader.to_string(),
                                },
                            );
                            lamports = lamports
                                .saturating_sub((sol * 1e9) as u64)
                                .saturating_sub(fee_lamports(tip));
                        } else {
                            state.record_skipped(&mint, TradingMode::BUY_REFUSED);
                        }
                    }
                }
                if tick % 18 == 9 {
                    state.record_skipped(
                        &Pubkey::new_unique(),
                        "creator buy 9.000 SOL is above the maximum 5",
                    );
                }
                if tick % 20 == 0 {
                    state.record_balance(lamports);
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    };

    if tui {
        let ui = tokio::task::spawn_blocking(move || shred_pool_cache::tui::run(state, trader));
        tokio::select! { _ = sim => {}, r = ui => { r??; } }
    } else {
        tokio::select! { _ = sim => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    Ok(())
}
