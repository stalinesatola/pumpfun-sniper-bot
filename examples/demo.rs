//! Runs the dashboard (web API and optional `--tui`) against simulated activity, so the
//! interface can be tried without a wallet, an RPC or a Shredstream connection.
//!
//!     cargo run --example demo            # then open http://127.0.0.1:8787, token printed below
//!     cargo run --example demo -- --tui
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use shred_pool_cache::api::{self, ApiConfig};
use shred_pool_cache::pumpfun::PumpFunSDK;
use shred_pool_cache::pumpswap::constants::PriorityFeePump;
use shred_pool_cache::state::{AppState, Settings};
use shred_pool_cache::trader::Trader;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tui = std::env::args().any(|a| a == "--tui");
    let payer = Keypair::new();
    let state = Arc::new(AppState::new(
        payer.pubkey(),
        Settings::new(0.001, 0.001, 7),
    ));
    // Nothing listens here: simulated positions fail to sell, which shows the failure paths too.
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
        let (state, trader) = (state.clone(), trader.clone());
        async move {
            state.set_stream_connected(true);
            let mut lamports = 2_000_000_000u64;
            state.record_balance(lamports);
            let mut slot = 300_000_000u64;
            for tick in 0u64.. {
                slot += 1;
                state.set_slot(slot);
                trader.sell_due(slot);
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
