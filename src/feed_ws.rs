//! Follows Pump.fun through the RPC's `logsSubscribe` WebSocket: no shred feed needed, only an
//! RPC with WebSocket support. The events in each transaction's logs give the launches and
//! trades (see `pump::events`). Slower than a shred feed by about a second, and a busy
//! program can overflow a free plan's limits, so some events may be missed.
use anyhow::{Context, Result};
use futures_util::StreamExt;
use log::{info, warn};
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::rpc_config::{RpcTransactionLogsConfig, RpcTransactionLogsFilter};
use solana_commitment_config::CommitmentConfig;

use crate::ingest::{self, Activity};
use crate::pump::events;
use crate::trader::Trader;
use crate::types::PUMP_FUN_PROGRAM_ID_STR;

/// Runs until the WebSocket closes or fails.
pub async fn run(ws_url: &str, trader: &Trader) -> Result<()> {
    let state = &trader.state;
    info!("Connecting to the RPC WebSocket for Pump.fun logs");
    let client = PubsubClient::new(ws_url)
        .await
        .context("could not connect to the RPC WebSocket (WS_URL)")?;
    let (mut stream, _unsubscribe) = client
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(vec![PUMP_FUN_PROGRAM_ID_STR.to_string()]),
            RpcTransactionLogsConfig {
                commitment: Some(CommitmentConfig::processed()),
            },
        )
        .await
        .context("could not subscribe to the Pump.fun logs")?;
    state.set_stream_connected(true);
    info!("Following Pump.fun through the RPC logs");

    let mut empty_streak = 0u32;
    while let Some(message) = stream.next().await {
        let slot = message.context.slot;
        state.set_slot(slot);
        trader.sell_due(slot);
        crate::community::handle_due(trader, slot);

        let logs = message.value;
        if logs.err.is_some() {
            continue;
        }
        let ops = events::parse_logs(&logs.logs);
        if ops.is_empty() {
            // Logs cut short by the node ("Log truncated") carry no events; say so once in a while.
            if logs.logs.iter().any(|l| l.contains("Log truncated")) {
                empty_streak += 1;
                if empty_streak % 100 == 1 {
                    warn!("The RPC truncated the logs of some Pump.fun transactions; their events are lost");
                }
            }
            continue;
        }
        let followed = state.follow.wallets();
        let activity = Activity {
            slot,
            signature: &logs.signature,
            blockhash: None,
            ops: &ops,
        };
        ingest::process(trader, &activity, &followed, state.follow.active());
    }
    Ok(())
}
