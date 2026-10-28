use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use dotenv::dotenv;
use env_logger::Env;
use jito_protos::shredstream::{
    shredstream_proxy_client::ShredstreamProxyClient, SubscribeEntriesRequest,
};
use log::{error, info, warn};
use rustls::crypto::aws_lc_rs::default_provider;
use rustls::crypto::CryptoProvider;
use shred_pool_cache::config::Config;
use shred_pool_cache::pumpfun::PumpFunSDK;
use shred_pool_cache::pumpswap::constants::PriorityFeePump;
use shred_pool_cache::types::*;
use solana_client::rpc_client::SerializableTransaction;
use solana_sdk::instruction::CompiledInstruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

const PUMP_CREATE_DISCRIMINATOR: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];

/// How many times a sell is attempted before giving up on a position.
const MAX_SELL_ATTEMPTS: u32 = 5;
/// Pause between sell attempts (about two slots).
const SELL_RETRY_DELAY: Duration = Duration::from_millis(800);

const MIN_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
/// A stream that stayed up this long is considered healthy and resets the backoff.
const HEALTHY_STREAM_AGE: Duration = Duration::from_secs(60);

/// A position bought by the sniper that must be sold once `slot_to_exec` is reached.
struct FutureSell {
    mint: Pubkey,
    creator: Pubkey,
    slot_to_exec: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    if CryptoProvider::get_default().is_none() {
        default_provider()
            .install_default()
            .map_err(|_| anyhow::anyhow!("failed to install the default rustls crypto provider"))?;
    }

    let config = Config::from_env().context("invalid configuration (check your .env)")?;
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

    // Positions waiting to be sold. Kept outside the reconnect loop so that a dropped
    // stream never makes the bot forget what it holds.
    let mut pending: Vec<FutureSell> = Vec::new();

    let run = async {
        let mut delay = MIN_RECONNECT_DELAY;
        loop {
            let started = Instant::now();
            match run_stream(&config, &sdk, &mut pending).await {
                Ok(()) => warn!("Shredstream closed by the server"),
                Err(e) => error!("Shredstream error: {e:#}"),
            }

            if started.elapsed() >= HEALTHY_STREAM_AGE {
                delay = MIN_RECONNECT_DELAY;
            }
            warn!("Reconnecting in {:?}", delay);
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(MAX_RECONNECT_DELAY);
        }
    };

    tokio::select! {
        _ = run => {}
        _ = tokio::signal::ctrl_c() => {
            warn!("Ctrl-C received, shutting down");
        }
    }

    Ok(())
}

fn is_create_ix(account_keys: &[Pubkey], ix: &CompiledInstruction) -> bool {
    ix.data.len() >= 8
        && ix.data[0..8] == PUMP_CREATE_DISCRIMINATOR
        && account_keys.get(ix.program_id_index as usize) == Some(&PUMP_FUN_PROGRAM_ID)
}

/// Connects to the Shredstream proxy and processes slots until the stream ends or fails.
async fn run_stream(
    config: &Config,
    sdk: &PumpFunSDK,
    pending: &mut Vec<FutureSell>,
) -> Result<()> {
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

    while let Some(slot_entry) = stream.message().await.context("stream read failed")? {
        let curr_slot = slot_entry.slot;

        // Sells run in their own tasks so slow RPC calls never stall the stream.
        let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(pending)
            .into_iter()
            .partition(|p| curr_slot >= p.slot_to_exec);
        *pending = waiting;
        for position in due {
            tokio::spawn(sell_position(
                sdk.clone(),
                config.payer.insecure_clone(),
                position,
            ));
        }

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
                let Some(create_ix) = tx
                    .message
                    .instructions()
                    .iter()
                    .find(|ix| is_create_ix(account_keys, ix))
                else {
                    continue;
                };

                let signature = tx.get_signature();
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

                info!(
                    "Sniping mint {} (creator {})",
                    parsed_txn.mint, parsed_txn.creator
                );

                let swap_res = sdk
                    .clone()
                    .buy(
                        &parsed_txn.mint,
                        config.buy_amount_sol,
                        config.payer.insecure_clone(),
                        parsed_txn.blockhash,
                        buy_amount,
                        parsed_txn.creator,
                    )
                    .await;

                match swap_res {
                    Ok((buy_signature, _estimated_tokens)) => {
                        info!("Placed PumpFun buy with Signature: {}", buy_signature);
                        // The sell uses the real on-chain balance, not the buy estimate.
                        pending.push(FutureSell {
                            mint: parsed_txn.mint,
                            creator: parsed_txn.creator,
                            slot_to_exec: curr_slot + config.sell_delay_slots,
                        });
                    }
                    Err(e) => error!("Failed to place PumpFun buy: {}", e),
                }

                let curr_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or_default();
                info!("Executed TX at: {}", curr_time);

                // One snipe per transaction is enough.
                break;
            }
        }
    }

    Ok(())
}

enum SellOutcome {
    Sold(String),
    NothingToSell,
}

async fn try_sell(sdk: &PumpFunSDK, payer: &Keypair, position: &FutureSell) -> Result<SellOutcome> {
    let balance = sdk.token_balance(&payer.pubkey(), &position.mint).await?;
    if balance == 0 {
        return Ok(SellOutcome::NothingToSell);
    }

    // A fresh blockhash: the one from the create tx may be expired by now.
    let blockhash = sdk.latest_blockhash().await?;
    let signature = sdk
        .clone()
        .sell(
            &position.mint,
            balance,
            payer.insecure_clone(),
            blockhash,
            position.creator,
        )
        .await?;

    Ok(SellOutcome::Sold(signature))
}

/// Sells a position with retries. Skips the sell when there is nothing to sell, which
/// happens when the buy never landed.
async fn sell_position(sdk: PumpFunSDK, payer: Keypair, position: FutureSell) {
    for attempt in 1..=MAX_SELL_ATTEMPTS {
        match try_sell(&sdk, &payer, &position).await {
            Ok(SellOutcome::Sold(signature)) => {
                info!(
                    "Placed PumpFun sell for {} with Signature: {}",
                    position.mint, signature
                );
                return;
            }
            Ok(SellOutcome::NothingToSell) => warn!(
                "No {} balance to sell yet (attempt {}/{})",
                position.mint, attempt, MAX_SELL_ATTEMPTS
            ),
            Err(e) => error!(
                "Sell of {} failed (attempt {}/{}): {:#}",
                position.mint, attempt, MAX_SELL_ATTEMPTS, e
            ),
        }

        if attempt < MAX_SELL_ATTEMPTS {
            tokio::time::sleep(SELL_RETRY_DELAY).await;
        }
    }

    error!(
        "Giving up selling {}: either the buy never landed or the sell kept failing. Check the wallet.",
        position.mint
    );
}
