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
use shred_pool_cache::pumpfun::PumpFunSDK;
use shred_pool_cache::pumpswap::constants::PriorityFeePump;
use shred_pool_cache::state::{AppState, PositionSource, Settings};
use shred_pool_cache::trader::Trader;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tui = std::env::args().any(|a| a == "--tui");
    let payer = Keypair::new();
    let leader = Pubkey::new_unique();
    let copy = CopyConfig {
        wallets: [leader].into_iter().collect(),
        ..CopyConfig::default()
    };
    let state =
        Arc::new(AppState::new(payer.pubkey(), Settings::new(0.001, 0.001, 7)).with_copy(copy));
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
            state.set_stream_connected(true);
            let mut lamports = 2_000_000_000u64;
            state.record_balance(lamports);
            let mut slot = 300_000_000u64;
            for tick in 0u64.. {
                slot += 1;
                state.set_slot(slot);
                // Simulated exits: most sell, now and then one fails, so every state shows up.
                for (n, mint) in state.due_positions(slot).into_iter().enumerate() {
                    if state.begin_sell(&mint).is_none() {
                        continue;
                    }
                    if (tick + n as u64) % 5 == 0 {
                        state.record_sell_failed(&mint, "RPC node returned 429 Too Many Requests");
                    } else {
                        state.record_sell_sent(&mint, "demo-sell-signature", 1_234_567);
                    }
                }
                if tick % 6 == 0 && !state.settings.paused() {
                    let (mint, creator) = (Pubkey::new_unique(), Pubkey::new_unique());
                    state.record_create(&mint, &creator, "demo-signature", slot);
                    if tick % 12 == 0 {
                        let sol = state.settings.buy_amount_sol();
                        state.record_buy_sent(
                            &mint,
                            &creator,
                            "demo-buy-signature",
                            sol,
                            slot + state.settings.sell_delay_slots(),
                        );
                        lamports = lamports.saturating_sub((sol * 1e9) as u64);
                    }
                }
                // A followed wallet buys now and then; we mirror it.
                if tick % 14 == 5 && !state.settings.paused() {
                    let mint = Pubkey::new_unique();
                    let trade = LeaderTrade {
                        side: Side::Buy,
                        leader,
                        mint,
                        creator_vault: Pubkey::new_unique(),
                        token_amount: 1_000_000,
                        sol_amount: 1_500_000_000,
                    };
                    state.record_copy_signal(&trade, slot);
                    state.record_buy_sent_from(
                        &mint,
                        &trade.creator_vault,
                        "demo-copy-signature",
                        0.001,
                        slot + 12,
                        PositionSource::Copy {
                            leader: leader.to_string(),
                        },
                    );
                    lamports = lamports.saturating_sub(1_000_000);
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
