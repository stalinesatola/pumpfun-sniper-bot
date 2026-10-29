//! Checks the Pump.fun trade instructions against the real program without spending anything:
//! builds buy + sell (+ close) for a token and runs them through `simulateTransaction` with
//! signature checks off, using any funded wallet as the paying user.
//!
//!     RPC_URL=<mainnet rpc> cargo run --example probe -- <mint> <funded wallet> [spend, in quote units]
use anyhow::{Context, Result};
use shred_pool_cache::pump::market::{load_market, GlobalCache};
use shred_pool_cache::pumpfun::pumpfun::{buy_instructions, sell_instructions};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSimulateTransactionConfig;
use solana_sdk::message::Message;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::Transaction;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mint: Pubkey = args.next().context("mint")?.parse()?;
    let user: Pubkey = args.next().context("funded wallet")?.parse()?;
    let rpc = RpcClient::new(
        std::env::var("RPC_URL").unwrap_or_else(|_| "https://solana-rpc.publicnode.com".into()),
    );
    let market = load_market(&rpc, &GlobalCache::default(), &mint).await?;
    println!(
        "curve: quote {:?} complete {} reserves {}/{} tokens-program {} quote-program {}",
        market.curve.quote,
        market.curve.complete,
        market.curve.virtual_token_reserves,
        market.curve.virtual_quote_reserves,
        market.base_token_program,
        market.quote_token_program
    );
    let spend: u64 = match args.next() {
        Some(s) => s.parse()?,
        None => 10_000_000, // 0.01 SOL
    };
    let min_tokens = market.curve.tokens_for_quote(spend).unwrap_or(1) / 2;

    // The same instructions the bot sends (compute budget, tip, accounts, trade), with the
    // sell sized to what the buy is guaranteed to give.
    let tip = 100_000;
    let mut ixs = buy_instructions(&market, &user, spend, min_tokens, tip);
    if std::env::var("BUY_ONLY").is_err() {
        // (skip the second copy of the compute budget instructions)
        ixs.extend(
            sell_instructions(&market, &user, min_tokens, tip, false)
                .into_iter()
                .skip(2),
        );
    }
    let message = Message::new(&ixs, Some(&user));
    let tx = Transaction::new_unsigned(message);
    let res = rpc
        .simulate_transaction_with_config(
            &tx,
            RpcSimulateTransactionConfig {
                sig_verify: false,
                replace_recent_blockhash: true,
                ..Default::default()
            },
        )
        .await?;
    println!("err: {:?}", res.value.err);
    println!("units: {:?}", res.value.units_consumed);
    for l in res.value.logs.unwrap_or_default() {
        println!("  {l}");
    }
    Ok(())
}
