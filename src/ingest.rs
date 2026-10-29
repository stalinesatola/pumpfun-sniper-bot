//! What the bot does with the Pump.fun activity of one transaction, whichever feed it came
//! from (the shred stream or the RPC logs): count community buys, mirror followed wallets
//! and start watching new launches.
use std::collections::HashSet;

use log::info;
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;

use crate::community::{self, LastBuy, NewToken};
use crate::copy;
use crate::pump::parse::{Op, Quote, Side, Trade};
use crate::risk::lamports_to_sol;
use crate::trader::Trader;

/// One transaction's Pump.fun operations. `blockhash` is known on the shred feed only; the
/// buy path fetches a fresh one when it is missing.
pub struct Activity<'a> {
    pub slot: u64,
    pub signature: &'a str,
    pub blockhash: Option<Hash>,
    pub ops: &'a [Op],
}

pub fn process(
    trader: &Trader,
    activity: &Activity,
    followed: &HashSet<Pubkey>,
    followed_active: bool,
) {
    let state = &trader.state;
    let (slot, blockhash, ops) = (activity.slot, activity.blockhash, activity.ops);

    // Community: count every buy of the tokens we are watching.
    for buy in community::buys_from_ops(ops) {
        state.community.observe_buy(&buy, blockhash);
    }

    // Copy-trade: mirror what the followed wallets do on Pump.fun.
    if followed_active {
        if let Some(trade) = copy::leader_trade_from_ops(ops, followed) {
            copy::handle_leader_trade(trader, trade, slot, blockhash);
        }
    }

    let Some(create) = ops.iter().find_map(|op| match op {
        Op::Create(c) => Some(c),
        _ => None,
    }) else {
        return;
    };
    info!(
        "Found PumpFun {} TX: {} ({})",
        if create.v2 { "create_v2" } else { "create" },
        activity.signature,
        create.symbol
    );

    // The fee payer is the wallet that launched the token.
    let creator_wallet = create.user;
    state.record_create(&create.mint, &creator_wallet, activity.signature, slot);

    // The creator's own launch buy, if any, is the first price we have.
    let launch_buy: Option<&Trade> = ops.iter().find_map(|op| match op {
        Op::Trade(t) if t.side == Side::Buy && t.mint == create.mint && t.token_amount > 0 => {
            Some(t)
        }
        _ => None,
    });
    state.community.track(NewToken {
        mint: create.mint,
        quote: create.quote,
        creator_wallet,
        creator_vault: launch_buy.and_then(|t| t.creator_vault),
        name: Some(create.name.clone()),
        symbol: Some(create.symbol.clone()),
        dev_buy_sol: launch_buy
            .filter(|t| t.quote == Quote::Sol)
            .map_or(0.0, |t| lamports_to_sol(t.quote_amount)),
        slot,
        reference: launch_buy.map(|t| LastBuy {
            token_amount: t.token_amount,
            sol_amount: t.quote_amount,
            blockhash,
        }),
    });
    // With the community rule off the decision is due right now.
    community::handle_due(trader, slot);
}
