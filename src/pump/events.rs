//! Reads Pump.fun events (`CreateEvent`, `TradeEvent`) out of a transaction's logs, so the bot
//! can follow the program with nothing but an RPC WebSocket (`logsSubscribe`).
//!
//! The program prints each event as a `Program data: <base64>` log line. Only lines printed
//! while the Pump.fun program itself is running are trusted: any other program in the same
//! transaction could print bytes that look like a trade.
use base64::Engine;
use solana_sdk::pubkey::Pubkey;

use super::parse::{Create, Op, Quote, Side, Trade};
use super::TOKEN_2022_PROGRAM;
use crate::types::PUMP_FUN_PROGRAM_ID_STR;

const CREATE_EVENT: [u8; 8] = [27, 114, 169, 77, 222, 235, 99, 118];
const TRADE_EVENT: [u8; 8] = [189, 219, 127, 211, 78, 230, 97, 238];

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.data.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn bool(&mut self) -> Option<bool> {
        Some(self.take(1)?[0] != 0)
    }
    fn key(&mut self) -> Option<Pubkey> {
        Some(Pubkey::new_from_array(self.take(32)?.try_into().ok()?))
    }
    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}

fn parse_create(data: &[u8]) -> Option<Create> {
    let mut r = Reader { data, at: 8 };
    let name = r.string()?;
    let symbol = r.string()?;
    let _uri = r.string()?;
    let mint = r.key()?;
    let bonding_curve = r.key()?;
    let user = r.key()?;
    let creator = r.key()?;
    r.take(8)?; // timestamp
    r.take(4 * 8)?; // reserves and supply
    let token_program = r.key()?;
    // Older events end here; later ones add the flags and the quote mint.
    let quote = (|| {
        r.take(2)?; // is_mayhem_mode, is_cashback_enabled
        r.key()
    })()
    .map_or(Quote::Sol, Quote::from_mint);
    Some(Create {
        mint,
        bonding_curve,
        associated_bonding_curve: Pubkey::default(),
        user,
        creator,
        quote,
        name,
        symbol,
        v2: token_program == TOKEN_2022_PROGRAM,
    })
}

fn parse_trade(data: &[u8]) -> Option<Trade> {
    let mut r = Reader { data, at: 8 };
    let mint = r.key()?;
    let sol_amount = r.u64()?;
    let token_amount = r.u64()?;
    let is_buy = r.bool()?;
    let user = r.key()?;
    // The quote asset and its amount sit after a run of fee fields, a string and a list;
    // events from before USDC support stop earlier, and those are SOL.
    let (quote, quote_amount) = (|| {
        r.take(8)?; // timestamp
        r.take(4 * 8)?; // reserves
        r.take(32)?; // fee recipient
        r.take(2 * 8)?; // fee bps, fee
        r.take(32)?; // creator
        r.take(2 * 8)?; // creator fee bps, creator fee
        r.take(1)?; // track_volume
        r.take(4 * 8)?; // unclaimed, claimed, volume, last update
        r.string()?; // ix_name
        r.take(1)?; // mayhem_mode
        r.take(4 * 8)?; // cashback and buyback bps and amounts
        let shareholders = r.u32()? as usize;
        r.take(shareholders.checked_mul(34)?)?;
        let quote_mint = r.key()?;
        let amount = r.u64()?;
        Some((Quote::from_mint(quote_mint), amount))
    })()
    .unwrap_or((Quote::Sol, sol_amount));
    Some(Trade {
        side: if is_buy { Side::Buy } else { Side::Sell },
        mint,
        user,
        quote,
        token_amount,
        quote_amount: if quote == Quote::Sol {
            sol_amount
        } else {
            quote_amount
        },
        creator_vault: None,
    })
}

/// The launches and trades a transaction's logs report, in order.
pub fn parse_logs(logs: &[String]) -> Vec<Op> {
    let mut stack: Vec<&str> = Vec::new();
    let mut ops = Vec::new();
    for line in logs {
        let Some(rest) = line.strip_prefix("Program ") else {
            continue;
        };
        if let Some(b64) = rest.strip_prefix("data: ") {
            if stack.last() != Some(&PUMP_FUN_PROGRAM_ID_STR) {
                continue;
            }
            let Ok(data) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
                continue;
            };
            match data.get(..8) {
                Some(d) if d == CREATE_EVENT => ops.extend(parse_create(&data).map(Op::Create)),
                Some(d) if d == TRADE_EVENT => ops.extend(parse_trade(&data).map(Op::Trade)),
                _ => {}
            }
        } else if let Some((id, tail)) = rest.split_once(' ') {
            if tail.starts_with("invoke [") {
                stack.push(id);
            } else if tail == "success" || tail.starts_with("failed") {
                stack.pop();
            }
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Logs of a real simulated buy and sell (`buy_exact_quote_in_v3` then `sell_v3`) on
    /// mainnet, as returned by `simulateTransaction`.
    fn real_logs() -> Vec<String> {
        [
        "Program ComputeBudget111111111111111111111111111111 invoke [1]",
        "Program ComputeBudget111111111111111111111111111111 success",
        "Program ComputeBudget111111111111111111111111111111 invoke [1]",
        "Program ComputeBudget111111111111111111111111111111 success",
        "Program ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL invoke [1]",
        "Program ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL success",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
        "Program log: Instruction: BuyExactQuoteInV3",
        "Program 11111111111111111111111111111111 invoke [2]",
        "Program 11111111111111111111111111111111 success",
        "Program TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb invoke [2]",
        "Program log: Instruction: TransferChecked",
        "Program TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb success",
        "Program 11111111111111111111111111111111 invoke [2]",
        "Program 11111111111111111111111111111111 success",
        "Program data: vdt/007mYe590RBGpp4BOiTcOxJbTOdC6gKTI0E6OpvToLxtZiYzbz60lgAAAAAAOFcdAjkAAAABYIzMHfzpYbQ7d5wZFQWm4tO/RdWk20YYrXbILWF1RTViAONqAAAAACOgGGQIAAAAr1cpr1ssAwAj9PRnAQAAAK+/FmPKLQIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABfAAAAAAAAAIRuAQAAAAAAqSD3gq9dWwK4Uxl1vut2HoSCaXV9o3T34lkJny3xMw8eAAAAAAAAAL5zAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEgAAAGJ1eV9leGFjdF9xdW90ZV9pbgAAAAAAAAAAAAAAAAAAAAAAiBMAAAAAAABCtwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD60lgAAAAAAI6AYZAgAAAAj9PRnAQAAAB4AAAAAAAAAvnMAAAAAAACHhQIAAAAAAAAAAAAAAAAA",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [2]",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
        "Program 11111111111111111111111111111111 invoke [1]",
        "Program 11111111111111111111111111111111 success",
        "Program 11111111111111111111111111111111 invoke [1]",
        "Program 11111111111111111111111111111111 success",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
        "Program log: Instruction: SellV3",
        "Program TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb invoke [2]",
        "Program log: Instruction: TransferChecked",
        "Program TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb success",
        "Program data: vdt/007mYe590RBGpp4BOiTcOxJbTOdC6gKTI0E6OpvToLxtZiYzb9NNTAAAAAAAljI/3BwAAAAAYIzMHfzpYbQ7d5wZFQWm4tO/RdWk20YYrXbILWF1RTViAONqAAAAAFBSzGMIAAAARYpoi3gsAwBQpqhnAQAAAEXyVT/nLQIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABfAAAAAAAAAJO5AAAAAAAAqSD3gq9dWwK4Uxl1vut2HoSCaXV9o3T34lkJny3xMw8eAAAAAAAAAJo6AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABAAAAHNlbGwAAAAAAAAAAAAAAAAAAAAAAIgTAAAAAAAAyVwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADTTUwAAAAAAFBSzGMIAAAAUKaoZwEAAAAeAAAAAAAAAJo6AAAAAAAAIcACAAAAAAAAAAAAAAAAAA==",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [2]",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn reads_the_real_buy_and_sell_events() {
        let ops = parse_logs(&real_logs());
        let trades: Vec<_> = ops
            .iter()
            .filter_map(|o| match o {
                Op::Trade(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(trades.len(), 2, "{ops:?}");
        assert_eq!(trades[0].side, Side::Buy);
        assert_eq!(trades[1].side, Side::Sell);
        // The simulation spent 0.01 SOL; the event reports the amount net of the program's fees.
        assert_eq!(trades[0].quote_amount, 9_876_542);
        assert_eq!(trades[0].quote, Quote::Sol);
        assert_eq!(
            trades[0].mint.to_string(),
            "9U8rq3Moj4vEk7WVkhxwuqfQbj9MJC7F9xSNrkaDpump"
        );
        assert_eq!(
            trades[0].user.to_string(),
            "7VtfL8fvgNfhz17qKRMjzQEXgbdpnHHHQRh54R9jP2RJ"
        );
        assert!(trades[0].token_amount > 0 && trades[1].token_amount > 0);
    }

    #[test]
    fn events_printed_by_other_programs_are_ignored() {
        // The same event line, but printed by a program that is not Pump.fun.
        let logs: Vec<String> = real_logs()
            .into_iter()
            .map(|l| {
                l.replace(
                    PUMP_FUN_PROGRAM_ID_STR,
                    "EvilProgram1111111111111111111111111111111",
                )
            })
            .collect();
        assert!(parse_logs(&logs).is_empty());
    }

    #[test]
    fn bad_input_is_ignored() {
        let pump = PUMP_FUN_PROGRAM_ID_STR;
        let wrap = |data: &str| {
            vec![
                format!("Program {pump} invoke [1]"),
                format!("Program data: {data}"),
                format!("Program {pump} success"),
            ]
        };
        assert!(parse_logs(&wrap("!!!not base64!!!")).is_empty());
        assert!(parse_logs(&wrap("AAAA")).is_empty());
        let mut short = TRADE_EVENT.to_vec();
        short.extend_from_slice(&[1; 10]);
        let b64 = base64::engine::general_purpose::STANDARD.encode(short);
        assert!(parse_logs(&wrap(&b64)).is_empty());
        assert!(parse_logs(&[]).is_empty());
    }

    #[test]
    fn an_old_trade_event_without_the_quote_fields_is_sol() {
        let mut data = TRADE_EVENT.to_vec();
        data.extend_from_slice(&[7; 32]); // mint
        data.extend_from_slice(&5u64.to_le_bytes()); // sol
        data.extend_from_slice(&9u64.to_le_bytes()); // tokens
        data.push(1); // buy
        data.extend_from_slice(&[8; 32]); // user
        data.extend_from_slice(&[0; 50]); // a short, cut-off tail
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let pump = PUMP_FUN_PROGRAM_ID_STR;
        let logs = vec![
            format!("Program {pump} invoke [1]"),
            format!("Program data: {b64}"),
            format!("Program {pump} success"),
        ];
        let ops = parse_logs(&logs);
        let [Op::Trade(t)] = ops.as_slice() else {
            panic!("{ops:?}")
        };
        assert_eq!(
            (t.quote, t.quote_amount, t.token_amount, t.side),
            (Quote::Sol, 5, 9, Side::Buy)
        );
    }
}
