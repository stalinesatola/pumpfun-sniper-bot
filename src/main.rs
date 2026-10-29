use std::env;
use std::path::PathBuf;
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
use shred_pool_cache::community::{self, CommunityConfig, LastBuy, NewToken};
use shred_pool_cache::config::Config;
use shred_pool_cache::copy::{self, CopyConfig};
use shred_pool_cache::pumpfun::PumpFunSDK;
use shred_pool_cache::pumpswap::constants::PriorityFeePump;
use shred_pool_cache::risk::{lamports_to_sol, RiskConfig};
use shred_pool_cache::state::{AppState, Settings};
use shred_pool_cache::trader::Trader;
use solana_client::rpc_client::SerializableTransaction;
use solana_sdk::signer::Signer;

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
    let community_config =
        CommunityConfig::from_env().context("invalid community configuration")?;
    let copy_config =
        CopyConfig::from_env(config.buy_amount_sol).context("invalid copy-trade configuration")?;
    if copy_config.wallets.contains(&config.payer.pubkey()) {
        anyhow::bail!("COPY_WALLETS contains this bot's own wallet; it would copy itself");
    }
    // Where the dashboard saves the followed wallets ("off" = do not save).
    let copy_store = match env::var("COPY_STATE_FILE") {
        Ok(v) if v.trim().eq_ignore_ascii_case("off") => None,
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => Some(PathBuf::from("copy_trade.json")),
    };

    let priority_fee = PriorityFeePump {
        unit_limit: 100_000,
        unit_price: 25_000_000,
        buy_tip_fee: 0.001,
        sell_tip_fee: 0.001,
        jito_tip_fee: config.bribe_fee,
    };
    let sdk = PumpFunSDK::new(&config.rpc_url, &config.astralane_url, priority_fee).await;
    // The program's global settings are needed by every trade; read them now.
    match sdk.warm_up().await {
        Ok(()) => info!("Read the Pump.fun global settings"),
        Err(e) => warn!(
            "Could not read the Pump.fun global settings yet ({e}); will retry on the first trade"
        ),
    }

    // Where the trading mode is saved ("off" = do not save).
    let rules_store = match env::var("RULES_STATE_FILE") {
        Ok(v) if v.trim().eq_ignore_ascii_case("off") => None,
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => Some(PathBuf::from("rules.json")),
    };

    let state = Arc::new(AppState::configured(
        config.payer.pubkey(),
        Settings::from_config(&config),
        risk,
        copy_config,
        community_config,
        copy_store,
        rules_store,
    ));
    log_configuration(&config, &state);
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

/// Logs the configuration the bot is actually running with, read back from the state.
fn log_configuration(config: &Config, state: &AppState) {
    info!(
        "Wallet {} | buy {} SOL | tip {} SOL | sell after {} slots | mode {:?}",
        config.payer.pubkey(),
        state.settings.buy_amount_sol(),
        state.settings.bribe_fee_sol(),
        state.settings.sell_delay_slots(),
        state.settings.mode()
    );
    let view = state.settings.view();
    if view.cost_pct_of_buy > shred_pool_cache::costs::HIGH_COST_PCT {
        warn!(
            "Fees per buy+sell are about {:.5} SOL, {:.0}% of the {} SOL bought: a trade needs to gain more than that just to break even. Raise SOL_BUY_AMOUNT or lower BRIBE_FEE.",
            view.round_trip_cost_sol, view.cost_pct_of_buy, view.buy_amount_sol
        );
    }
    let c = &state.community.config;
    if c.require_community {
        info!(
            "Buy rule: new coins only, bought after {} slots if >= {} unique buyers and >= {} SOL bid; copy-trade follows tokens up to {} slots old (0 = no limit)",
            c.window_slots, c.min_buyers, c.min_volume_sol, c.max_token_age_slots
        );
    } else {
        info!("Buy rule: new coins only, bought as soon as they are created (community check off)");
    }
    let r = &state.risk;
    info!(
        "Risk: max {} open, spend cap {} SOL, drawdown cap {} SOL, slippage {}%, dev buy {}..{} SOL, {} blocked creators, {} blocked keywords (0 = off)",
        r.max_open_positions,
        r.max_session_spend_sol,
        r.max_session_drawdown_sol,
        r.buy_slippage_percent,
        r.min_dev_buy_sol,
        r.max_dev_buy_sol,
        r.blocked_creators.len(),
        r.blocked_keywords.len()
    );
    let f = &state.follow;
    info!(
        "Copy-trade: {} ({} wallet(s) followed, saved to disk: {}), {} SOL per copy, sell with leader: {}, max hold {} slots",
        if f.active() { "ON" } else { "off" },
        f.wallets().len(),
        f.persisted(),
        state.copy.buy_sol,
        state.copy.sell_with_leader,
        state.copy.max_hold_slots
    );
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
        community::handle_due(trader, curr_slot);
        // The dashboard can change who is followed at any time; read it once per message.
        let followed = state.follow.wallets();
        let followed_active = state.follow.active();

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
                let instructions = tx.message.instructions();
                let blockhash = *tx.message.recent_blockhash();

                // Community: count every buy of the tokens we are watching.
                for buy in community::pump_buys(account_keys, instructions) {
                    state.community.observe_buy(&buy, blockhash);
                }

                // Copy-trade: mirror what the followed wallets do on Pump.fun.
                if followed_active {
                    if let Some(trade) = copy::leader_trade(account_keys, instructions, &followed) {
                        copy::handle_leader_trade(trader, trade, curr_slot, blockhash);
                    }
                }

                let Some(create) = shred_pool_cache::pump::parse::ops(account_keys, instructions)
                    .find_map(|op| match op {
                        shred_pool_cache::pump::parse::Op::Create(c) => Some(c),
                        _ => None,
                    })
                else {
                    continue;
                };

                let signature = tx.get_signature().to_string();
                info!(
                    "Found PumpFun {} TX: {} ({})",
                    if create.v2 { "create_v2" } else { "create" },
                    signature,
                    create.symbol
                );

                // The fee payer is the wallet that launched the token.
                let creator_wallet = create.user;
                state.record_create(&create.mint, &creator_wallet, &signature, curr_slot);

                // The creator's own launch buy, if any, is the first price we have.
                let launch_buy = shred_pool_cache::pump::parse::ops(account_keys, instructions)
                    .find_map(|op| match op {
                        shred_pool_cache::pump::parse::Op::Trade(t)
                            if t.side == shred_pool_cache::pump::parse::Side::Buy
                                && t.mint == create.mint
                                && t.token_amount > 0 =>
                        {
                            Some(t)
                        }
                        _ => None,
                    });
                let lamports_if_sol = |t: &shred_pool_cache::pump::parse::Trade| {
                    if t.quote == shred_pool_cache::pump::parse::Quote::Sol {
                        lamports_to_sol(t.quote_amount)
                    } else {
                        0.0
                    }
                };
                state.community.track(NewToken {
                    mint: create.mint,
                    quote: create.quote,
                    creator_wallet,
                    creator_vault: launch_buy.and_then(|t| t.creator_vault),
                    name: Some(create.name.clone()),
                    symbol: Some(create.symbol.clone()),
                    dev_buy_sol: launch_buy.as_ref().map_or(0.0, lamports_if_sol),
                    slot: curr_slot,
                    reference: launch_buy.map(|t| LastBuy {
                        token_amount: t.token_amount,
                        sol_amount: t.quote_amount,
                        blockhash,
                    }),
                });
                // With the community rule off the decision is due right now.
                community::handle_due(trader, curr_slot);
            }
        }
    }

    Ok(())
}
