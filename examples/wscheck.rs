//! Counts what the RPC logs feed delivers for a while (nothing is sent or bought):
//!     WS_URL=wss://... cargo run --example wscheck
use futures_util::StreamExt;
use shred_pool_cache::pump::events::parse_logs;
use shred_pool_cache::pump::parse::{Op, Side};
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::rpc_config::{RpcTransactionLogsConfig, RpcTransactionLogsFilter};
use solana_commitment_config::CommitmentConfig;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let url = std::env::var("WS_URL").unwrap_or_else(|_| "wss://solana-rpc.publicnode.com".into());
    let secs: u64 = std::env::var("SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let client = PubsubClient::new(&url).await?;
    let (mut stream, _u) = client
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(vec![
                "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".into()
            ]),
            RpcTransactionLogsConfig {
                commitment: Some(CommitmentConfig::processed()),
            },
        )
        .await?;
    let (mut txs, mut creates, mut buys, mut sells, mut empty, mut last_slot) = (0, 0, 0, 0, 0, 0);
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        let Ok(Some(m)) = tokio::time::timeout(Duration::from_secs(5), stream.next()).await else {
            break;
        };
        txs += 1;
        last_slot = m.context.slot;
        let ops = parse_logs(&m.value.logs);
        if ops.is_empty() {
            empty += 1;
        }
        for op in ops {
            match op {
                Op::Create(c) => {
                    creates += 1;
                    println!("create {} {} quote {:?}", c.symbol, c.mint, c.quote)
                }
                Op::Trade(t) if t.side == Side::Buy => buys += 1,
                Op::Trade(_) => sells += 1,
            }
        }
    }
    println!("in {secs}s: {txs} txs, {creates} creates, {buys} buys, {sells} sells, {empty} txs without events, last slot {last_slot}");
    Ok(())
}
