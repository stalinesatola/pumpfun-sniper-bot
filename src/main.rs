use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dotenvy::dotenv;
use env_logger::Env;
use jito_protos::shredstream::{
    shredstream_proxy_client::ShredstreamProxyClient, SubscribeEntriesRequest,
};
use log::{error, info, warn};
use rustls::crypto::aws_lc_rs::default_provider;
use rustls::crypto::CryptoProvider;
use shred_pool_cache::api::{self, ApiConfig};
use shred_pool_cache::config::Config;
use shred_pool_cache::copy::{self, CopyConfig};
use shred_pool_cache::pumpfun::PumpFunSDK;
use shred_pool_cache::pumpswap::constants::PriorityFeePump;
use shred_pool_cache::risk::{lamports_to_sol, Candidate, RiskConfig};
use shred_pool_cache::state::{AppState, Settings};
use shred_pool_cache::trader::Trader;
use shred_pool_cache::types::*;
use solana_client::rpc_client::SerializableTransaction;
use solana_sdk::message::compiled_instruction::CompiledInstruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signer::Signer;

const PUMP_CREATE_DISCRIMINATOR: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];

const MIN_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
/// A stream that stayed up this long is considered healthy and resets the backoff.
const HEALTHY_STREAM_AGE: Duration = Duration::from_secs(60);
const BALANCE_REFRESH: Duration = Duration::from_secs(10);

fn init_logging(tui: bool) -> Result<()> {
    let mut builder = env_logger::Builder::from_env(Env::default().default_filter_or("info"));
    if tui {
        // The terminal belongs to the dashboard, so logs go to a file instead.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("sniper.log")
            .context("could not open sniper.log")?;
        builder.target(env_logger::Target::Pipe(Box::new(file)));
    }
    builder.init();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    let tui = std::env::args().any(|a| a == "--tui");
    #[cfg(not(feature = "tui"))]
    if tui {
        anyhow::bail!("this build has no terminal UI; rebuild with `--features tui`");
    }
    init_logging(tui)?;

    if CryptoProvider::get_default().is_none() {
        default_provider()
            .install_default()
            .map_err(|_| anyhow::anyhow!("failed to install the default rustls crypto provider"))?;
    }

    let config = Config::from_env().context("invalid configuration (check your .env)")?;
    let api_config = ApiConfig::from_env().context("invalid API configuration")?;
    let risk = RiskConfig::from_env().context("invalid risk configuration")?;
    let copy_config =
        CopyConfig::from_env(config.buy_amount_sol).context("invalid copy-trade configuration")?;
    if copy_config.wallets.contains(&config.payer.pubkey()) {
        anyhow::bail!("COPY_WALLETS contains this bot's own wallet; it would copy itself");
    }
    if copy_config.enabled() {
        info!(
            "Copy-trade: following {} wallet(s), {} SOL per copy, sell with leader: {}, max hold {} slots",
            copy_config.wallets.len(),
            copy_config.buy_sol,
            copy_config.sell_with_leader,
            copy_config.max_hold_slots
        );
    }
    info!(
        "Risk: max {} open, spend cap {} SOL, drawdown cap {} SOL, slippage {}%, dev buy {}..{} SOL, {} blocked creators, {} blocked keywords (0 = off)",
        risk.max_open_positions,
        risk.max_session_spend_sol,
        risk.max_session_drawdown_sol,
        risk.buy_slippage_percent,
        risk.min_dev_buy_sol,
        risk.max_dev_buy_sol,
        risk.blocked_creators.len(),
        risk.blocked_keywords.len()
    );
    info!(
        "Wallet {} | buy {} SOL | tip {} SOL | sell after {} slots",
        config.payer.pubkey(),
        config.buy_amount_sol,
        config.bribe_fee,
        config.sell_delay_slots
    );

    let priority_fee = PriorityFeePump {
        unit_limit: 100_000,
        unit_price: 25_000_000,
        buy_tip_fee: 0.001,
        sell_tip_fee: 0.001,
        jito_tip_fee: config.bribe_fee,
    };
    let sdk = PumpFunSDK::new(&config.rpc_url, &config.astralane_url, priority_fee).await;

    let state = Arc::new(AppState::new(
        config.payer.pubkey(),
        Settings::from_config(&config),
    ));
    let trader = Trader::new(sdk, config.payer.insecure_clone(), state.clone());

    if let Some(api_config) = api_config {
        let (state, trader) = (state.clone(), trader.clone());
        tokio::spawn(async move {
            if let Err(e) = api::serve(api_config, state, trader).await {
                error!("Dashboard API stopped: {e:#}");
            }
        });
    }

    tokio::spawn(refresh_balance(trader.clone()));

    let run = async {
        let mut delay = MIN_RECONNECT_DELAY;
        loop {
            let started = Instant::now();
            match run_stream(&config, &trader).await {
                Ok(()) => warn!("Shredstream closed by the server"),
                Err(e) => error!("Shredstream error: {e:#}"),
            }
            state.set_stream_connected(false);

            if started.elapsed() >= HEALTHY_STREAM_AGE {
                delay = MIN_RECONNECT_DELAY;
            }
            warn!("Reconnecting in {:?}", delay);
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(MAX_RECONNECT_DELAY);
        }
    };

    #[cfg(feature = "tui")]
    let ui = {
        let (state, trader) = (state.clone(), trader.clone());
        async move {
            if tui {
                match tokio::task::spawn_blocking(move || shred_pool_cache::tui::run(state, trader))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => error!("Terminal UI failed: {e}"),
                    Err(e) => error!("Terminal UI crashed: {e}"),
                }
            } else {
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(feature = "tui"))]
    let ui = std::future::pending::<()>();

    tokio::select! {
        _ = run => {}
        _ = ui => info!("Terminal UI closed, shutting down"),
        _ = tokio::signal::ctrl_c() => warn!("Ctrl-C received, shutting down"),
    }

    Ok(())
}

/// Keeps the wallet balance on the dashboard fresh without ever touching the trading path.
async fn refresh_balance(trader: Trader) {
    let owner = trader.payer.pubkey();
    loop {
        match trader.sdk.client.get_balance(&owner).await {
            Ok(lamports) => trader.state.record_balance(lamports),
            Err(e) => warn!("Could not fetch the wallet balance: {e}"),
        }
        tokio::time::sleep(BALANCE_REFRESH).await;
    }
}

fn is_create_ix(account_keys: &[Pubkey], ix: &CompiledInstruction) -> bool {
    ix.data.len() >= 8
        && ix.data[0..8] == PUMP_CREATE_DISCRIMINATOR
        && account_keys.get(ix.program_id_index as usize) == Some(&PUMP_FUN_PROGRAM_ID)
}

/// Connects to the Shredstream proxy and processes slots until the stream ends or fails.
// `Entry` is flagged as unstable Agave API in newer releases; tests/golden_tx.rs pins its wire format.
#[allow(deprecated)]
async fn run_stream(config: &Config, trader: &Trader) -> Result<()> {
    let state = &trader.state;
    info!(
        "Connecting to ShredstreamProxy at: {}",
        config.shredstream_url
    );

    let mut client = ShredstreamProxyClient::connect(config.shredstream_url.clone())
        .await
        .context("could not connect to the Shredstream proxy")?;
    let mut stream = client
        .subscribe_entries(SubscribeEntriesRequest {})
        .await
        .context("could not subscribe to entries")?
        .into_inner();
    state.set_stream_connected(true);

    while let Some(slot_entry) = stream.message().await.context("stream read failed")? {
        let curr_slot = slot_entry.slot;
        state.set_slot(curr_slot);
        trader.sell_due(curr_slot);

        let entries =
            match bincode::deserialize::<Vec<solana_entry::entry::Entry>>(&slot_entry.entries) {
                Ok(e) => e,
                Err(e) => {
                    warn!("Deserialization failed with err: {e}");
                    continue;
                }
            };

        for entry in &entries {
            for tx in &entry.transactions {
                let account_keys = tx.message.static_account_keys();

                // Copy-trade: mirror what the followed wallets do on Pump.fun.
                if state.copy.enabled() {
                    if let Some(trade) = copy::leader_trade(
                        account_keys,
                        tx.message.instructions(),
                        &state.copy.wallets,
                    ) {
                        copy::handle_leader_trade(
                            trader,
                            trade,
                            curr_slot,
                            *tx.message.recent_blockhash(),
                        );
                    }
                }

                let Some(create_ix) = tx
                    .message
                    .instructions()
                    .iter()
                    .find(|ix| is_create_ix(account_keys, ix))
                else {
                    continue;
                };

                let signature = tx.get_signature().to_string();
                info!("Found PumpFun Create TX: {}", signature);

                let parsed_txn = match PumpFunTXInfo::from_create_tx(tx, create_ix) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!("Could not parse create TX {}: {:?}", signature, e);
                        continue;
                    }
                };

                let buy_amount = match parsed_txn.amounts {
                    Amounts::BuyAmounts(b) if b.base_amount_out > 0 => b,
                    _ => {
                        info!(
                            "Create TX {} has no usable creator buy, skipping",
                            signature
                        );
                        continue;
                    }
                };

                state.record_create(&parsed_txn.mint, &parsed_txn.creator, &signature, curr_slot);

                if state.settings.paused() {
                    info!("Paused: not buying {}", parsed_txn.mint);
                    continue;
                }

                // Settings can change from the dashboard at any time; read them per buy.
                let buy_amount_sol = state.settings.buy_amount_sol();
                let tip_sol = state.settings.bribe_fee_sol();

                let meta = parse_create_metadata(&create_ix.data);
                let candidate = Candidate {
                    creator: &parsed_txn.creator,
                    name: meta.as_ref().map(|m| m.0.as_str()),
                    symbol: meta.as_ref().map(|m| m.1.as_str()),
                    dev_buy_sol: lamports_to_sol(buy_amount.max_quote_amount_in),
                };
                if let Err(reason) =
                    state
                        .risk
                        .check(&candidate, &state.exposure(), buy_amount_sol, tip_sol)
                {
                    info!("Skipping {}: {}", parsed_txn.mint, reason);
                    state.record_skipped(&parsed_txn.mint, &reason);
                    continue;
                }

                // Another signal (a followed wallet buying the same token) may already own it.
                if !state.claim_mint(&parsed_txn.mint, buy_amount_sol) {
                    state.record_skipped(&parsed_txn.mint, "already holding or buying this token");
                    continue;
                }

                let mut sdk = trader.sdk.clone();
                sdk.priority_fee.jito_tip_fee = tip_sol;

                info!(
                    "Sniping mint {} (creator {})",
                    parsed_txn.mint, parsed_txn.creator
                );

                let swap_res = sdk
                    .buy(
                        &parsed_txn.mint,
                        buy_amount_sol,
                        trader.payer.insecure_clone(),
                        parsed_txn.blockhash,
                        buy_amount,
                        parsed_txn.creator,
                        state.risk.buy_slippage_percent,
                    )
                    .await;

                match swap_res {
                    Ok((buy_signature, _estimated_tokens)) => {
                        info!("Placed PumpFun buy with Signature: {}", buy_signature);
                        // The sell uses the real on-chain balance, not the buy estimate.
                        state.record_buy_sent(
                            &parsed_txn.mint,
                            &parsed_txn.creator,
                            &buy_signature,
                            buy_amount_sol,
                            curr_slot + state.settings.sell_delay_slots(),
                        );
                    }
                    Err(e) => {
                        error!("Failed to place PumpFun buy: {}", e);
                        state.record_buy_failed(&parsed_txn.mint, &e.to_string());
                    }
                }
                state.release_mint(&parsed_txn.mint);

                // One snipe per transaction is enough.
                break;
            }
        }
    }

    Ok(())
}
