//! Reads Pump.fun instructions from transactions on the feed. Never panics on malformed
//! input: anything that does not fit returns `None`.
//!
//! Limits: only top-level instructions are visible on the feed (trades made through another
//! program via CPI are not), and accounts that live in an address lookup table cannot be
//! resolved without an RPC call, so an instruction that needs one is skipped.
use solana_sdk::message::compiled_instruction::CompiledInstruction;
use solana_sdk::pubkey::Pubkey;

use super::{disc, USDC_MINT, WSOL_MINT};
use crate::types::PUMP_FUN_PROGRAM_ID;

/// What a token is priced in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quote {
    Sol,
    Usdc,
    /// Any other quote mint the program may allow in the future.
    Other(Pubkey),
}

impl Quote {
    pub fn from_mint(mint: Pubkey) -> Self {
        if mint == WSOL_MINT || mint == Pubkey::default() {
            Quote::Sol
        } else if mint == USDC_MINT {
            Quote::Usdc
        } else {
            Quote::Other(mint)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

/// A buy or sell of a Pump.fun token, whatever generation of instruction carried it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trade {
    pub side: Side,
    pub mint: Pubkey,
    pub user: Pubkey,
    pub quote: Quote,
    /// Tokens (raw units): exact for `buy`/`sell`, a minimum for the "exact quote in" buys.
    pub token_amount: u64,
    /// Quote units (lamports for SOL, 1e-6 for USDC): the spend cap of a buy, the spend of
    /// an "exact in" buy, the minimum output of a sell.
    pub quote_amount: u64,
    /// The creator-fee vault carried by the older instructions; the v3 ones do not have it.
    pub creator_vault: Option<Pubkey>,
}

/// A token launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Create {
    pub mint: Pubkey,
    pub bonding_curve: Pubkey,
    pub associated_bonding_curve: Pubkey,
    /// Who signed (and paid) the launch.
    pub user: Pubkey,
    /// The creator named in the instruction (receives the creator fees).
    pub creator: Pubkey,
    pub quote: Quote,
    pub name: String,
    pub symbol: String,
    /// `create_v2`: Token-2022 mint. The old `create` made classic SPL tokens.
    pub v2: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Create(Create),
    Trade(Trade),
}

struct Reader<'a> {
    keys: &'a [Pubkey],
    ix: &'a CompiledInstruction,
}

impl Reader<'_> {
    fn key(&self, n: usize) -> Option<Pubkey> {
        self.ix
            .accounts
            .get(n)
            .and_then(|i| self.keys.get(*i as usize))
            .copied()
    }
    fn u64_at(&self, offset: usize) -> Option<u64> {
        let end = offset.checked_add(8)?;
        Some(u64::from_le_bytes(
            self.ix.data.get(offset..end)?.try_into().ok()?,
        ))
    }
}

fn read_string(data: &[u8], offset: usize) -> Option<(String, usize)> {
    let len_end = offset.checked_add(4)?;
    let len = u32::from_le_bytes(data.get(offset..len_end)?.try_into().ok()?) as usize;
    let end = len_end.checked_add(len)?;
    let text = std::str::from_utf8(data.get(len_end..end)?).ok()?;
    Some((text.to_string(), end))
}

fn parse_create(r: &Reader, v2: bool) -> Option<Create> {
    let data = &r.ix.data;
    let (name, next) = read_string(data, 8)?;
    let (symbol, next) = read_string(data, next)?;
    let (_uri, next) = read_string(data, next)?;
    let creator = Pubkey::new_from_array(data.get(next..next.checked_add(32)?)?.try_into().ok()?);
    let user = r.key(if v2 { 5 } else { 7 })?;
    // create_v2 accounts: 0 mint, 2 bonding curve, 3 its token account; the legacy create
    // has the same first four. A USDC token adds the quote mint as the 17th account.
    let quote = if v2 {
        r.key(16).map_or(Quote::Sol, Quote::from_mint)
    } else {
        Quote::Sol
    };
    Some(Create {
        mint: r.key(0)?,
        bonding_curve: r.key(2)?,
        associated_bonding_curve: r.key(3)?,
        user,
        creator,
        quote,
        name,
        symbol,
        v2,
    })
}

/// Account positions of a trade instruction generation.
struct Layout {
    side: Side,
    mint: usize,
    user: usize,
    /// None for SOL-only generations.
    quote_mint: Option<usize>,
    creator_vault: Option<usize>,
}

const fn layout(
    side: Side,
    mint: usize,
    user: usize,
    q: Option<usize>,
    v: Option<usize>,
) -> Layout {
    Layout {
        side,
        mint,
        user,
        quote_mint: q,
        creator_vault: v,
    }
}

fn trade_layout(d: &[u8]) -> Option<Layout> {
    Some(match d {
        x if x == disc::BUY || x == disc::BUY_EXACT_SOL_IN => {
            layout(Side::Buy, 2, 6, None, Some(9))
        }
        x if x == disc::SELL => layout(Side::Sell, 2, 6, None, Some(8)),
        x if x == disc::BUY_V3 || x == disc::BUY_EXACT_QUOTE_IN_V3 => {
            layout(Side::Buy, 1, 8, Some(2), None)
        }
        x if x == disc::SELL_V3 => layout(Side::Sell, 1, 8, Some(2), None),
        x if x == disc::BUY_V2 || x == disc::BUY_EXACT_QUOTE_IN_V2 => {
            layout(Side::Buy, 1, 13, Some(2), Some(16))
        }
        x if x == disc::SELL_V2 => layout(Side::Sell, 1, 13, Some(2), Some(16)),
        _ => return None,
    })
}

fn parse_trade(r: &Reader) -> Option<Trade> {
    let d = r.ix.data.get(0..8)?;
    let l = trade_layout(d)?;
    let (a, b) = (r.u64_at(8)?, r.u64_at(16)?);
    // buy_exact_*_in carry (spend, min tokens); every other instruction (tokens, quote).
    let exact_in = d == disc::BUY_EXACT_SOL_IN
        || d == disc::BUY_EXACT_QUOTE_IN_V2
        || d == disc::BUY_EXACT_QUOTE_IN_V3;
    let (token_amount, quote_amount) = if exact_in { (b, a) } else { (a, b) };
    Some(Trade {
        side: l.side,
        mint: r.key(l.mint)?,
        user: r.key(l.user)?,
        quote: match l.quote_mint {
            Some(n) => Quote::from_mint(r.key(n)?),
            None => Quote::Sol,
        },
        token_amount,
        quote_amount,
        creator_vault: match l.creator_vault {
            Some(n) => Some(r.key(n)?),
            None => None,
        },
    })
}

/// Parses one instruction if it is a Pump.fun launch or trade.
pub fn parse_instruction(keys: &[Pubkey], ix: &CompiledInstruction) -> Option<Op> {
    if keys.get(ix.program_id_index as usize) != Some(&PUMP_FUN_PROGRAM_ID) {
        return None;
    }
    let r = Reader { keys, ix };
    let d = ix.data.get(0..8)?;
    if d == disc::CREATE_V2 {
        parse_create(&r, true).map(Op::Create)
    } else if d == disc::CREATE {
        parse_create(&r, false).map(Op::Create)
    } else {
        parse_trade(&r).map(Op::Trade)
    }
}

/// Every Pump.fun launch and trade of a transaction, in order.
pub fn ops<'a>(
    keys: &'a [Pubkey],
    instructions: &'a [CompiledInstruction],
) -> impl Iterator<Item = Op> + 'a {
    instructions
        .iter()
        .filter_map(move |ix| parse_instruction(keys, ix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(s: &str) -> Vec<u8> {
        let mut v = (s.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(s.as_bytes());
        v
    }

    fn keys(n: usize) -> Vec<Pubkey> {
        std::iter::once(PUMP_FUN_PROGRAM_ID)
            .chain((1..n).map(|_| Pubkey::new_unique()))
            .collect()
    }

    fn ix(data: Vec<u8>, accounts: Vec<u8>) -> CompiledInstruction {
        CompiledInstruction::new_from_raw_parts(0, data, accounts)
    }

    fn args(d: [u8; 8], a: u64, b: u64) -> Vec<u8> {
        let mut v = d.to_vec();
        v.extend_from_slice(&a.to_le_bytes());
        v.extend_from_slice(&b.to_le_bytes());
        v
    }

    fn all(n: usize) -> Vec<u8> {
        (1..=n as u8).collect()
    }

    #[test]
    fn discriminators_match_the_anchor_rule() {
        use sha2::{Digest, Sha256};
        for (name, d) in [
            ("create", disc::CREATE),
            ("create_v2", disc::CREATE_V2),
            ("buy", disc::BUY),
            ("buy_exact_sol_in", disc::BUY_EXACT_SOL_IN),
            ("buy_v2", disc::BUY_V2),
            ("buy_v3", disc::BUY_V3),
            ("buy_exact_quote_in_v2", disc::BUY_EXACT_QUOTE_IN_V2),
            ("buy_exact_quote_in_v3", disc::BUY_EXACT_QUOTE_IN_V3),
            ("sell", disc::SELL),
            ("sell_v2", disc::SELL_V2),
            ("sell_v3", disc::SELL_V3),
        ] {
            let h = Sha256::digest(format!("global:{name}"));
            assert_eq!(h[..8], d, "{name}");
        }
    }

    #[test]
    fn create_v2_with_sol_and_usdc_quote() {
        let k = keys(20);
        let creator = Pubkey::new_unique();
        let mut data = disc::CREATE_V2.to_vec();
        data.extend(string("Moon Cat"));
        data.extend(string("MCAT"));
        data.extend(string("https://x/m.json"));
        data.extend_from_slice(creator.as_ref());
        data.push(0); // is_mayhem_mode

        let Some(Op::Create(c)) = parse_instruction(&k, &ix(data.clone(), all(16))) else {
            panic!("create_v2 not parsed")
        };
        assert_eq!(
            (c.mint, c.bonding_curve, c.associated_bonding_curve),
            (k[1], k[3], k[4])
        );
        assert_eq!((c.user, c.creator), (k[6], creator));
        assert_eq!((c.name.as_str(), c.symbol.as_str()), ("Moon Cat", "MCAT"));
        assert_eq!(c.quote, Quote::Sol);
        assert!(c.v2);

        let mut k2 = k.clone();
        k2[17] = USDC_MINT;
        let Some(Op::Create(c)) = parse_instruction(&k2, &ix(data, all(19))) else {
            panic!()
        };
        assert_eq!(c.quote, Quote::Usdc);
    }

    #[test]
    fn legacy_create_is_still_understood() {
        let k = keys(12);
        let mut data = disc::CREATE.to_vec();
        for s in ["a", "b", "c"] {
            data.extend(string(s));
        }
        data.extend_from_slice(Pubkey::new_unique().as_ref());
        let Some(Op::Create(c)) = parse_instruction(&k, &ix(data, all(11))) else {
            panic!()
        };
        assert!(!c.v2);
        assert_eq!(c.user, k[8]);
    }

    #[test]
    fn every_trade_generation_gives_mint_user_and_amounts() {
        let k = keys(30);
        let cases: [([u8; 8], usize, usize, usize, Side, Option<usize>); 9] = [
            (disc::BUY, 16, 3, 7, Side::Buy, Some(10)),
            (disc::BUY_EXACT_SOL_IN, 16, 3, 7, Side::Buy, Some(10)),
            (disc::SELL, 14, 3, 7, Side::Sell, Some(9)),
            (disc::BUY_V3, 17, 2, 9, Side::Buy, None),
            (disc::BUY_EXACT_QUOTE_IN_V3, 17, 2, 9, Side::Buy, None),
            (disc::SELL_V3, 17, 2, 9, Side::Sell, None),
            (disc::BUY_V2, 27, 2, 14, Side::Buy, Some(17)),
            (disc::BUY_EXACT_QUOTE_IN_V2, 27, 2, 14, Side::Buy, Some(17)),
            (disc::SELL_V2, 26, 2, 14, Side::Sell, Some(17)),
        ];
        for (d, n, mint, user, side, vault) in cases {
            let accounts = all(n);
            let Some(Op::Trade(t)) = parse_instruction(&k, &ix(args(d, 111, 222), accounts)) else {
                panic!("{d:?} not parsed")
            };
            assert_eq!(t.side, side, "{d:?}");
            assert_eq!(t.mint, k[mint], "{d:?}");
            assert_eq!(t.user, k[user], "{d:?}");
            assert_eq!(t.creator_vault, vault.map(|v| k[v]), "{d:?}");
        }
    }

    #[test]
    fn amounts_follow_each_instruction_meaning() {
        let k = keys(30);
        let t = |d, n| match parse_instruction(&k, &ix(args(d, 111, 222), all(n))) {
            Some(Op::Trade(t)) => t,
            _ => panic!(),
        };
        // buy: (tokens, max cost); exact-in: (spend, min tokens).
        let b = t(disc::BUY, 16);
        assert_eq!((b.token_amount, b.quote_amount), (111, 222));
        let e = t(disc::BUY_EXACT_SOL_IN, 16);
        assert_eq!((e.token_amount, e.quote_amount), (222, 111));
        let e3 = t(disc::BUY_EXACT_QUOTE_IN_V3, 17);
        assert_eq!((e3.token_amount, e3.quote_amount), (222, 111));
        let s = t(disc::SELL_V3, 17);
        assert_eq!((s.token_amount, s.quote_amount), (111, 222));
    }

    #[test]
    fn quote_comes_from_the_quote_mint_account() {
        let mut k = keys(30);
        k[3] = USDC_MINT; // account 2 of v3 = key index 3 (accounts are 1-based keys)
        let Some(Op::Trade(t)) = parse_instruction(&k, &ix(args(disc::BUY_V3, 1, 2), all(17)))
        else {
            panic!()
        };
        assert_eq!(t.quote, Quote::Usdc);
        k[3] = WSOL_MINT;
        let Some(Op::Trade(t)) = parse_instruction(&k, &ix(args(disc::BUY_V3, 1, 2), all(17)))
        else {
            panic!()
        };
        assert_eq!(t.quote, Quote::Sol);
        let other = Pubkey::new_unique();
        k[3] = other;
        let Some(Op::Trade(t)) = parse_instruction(&k, &ix(args(disc::BUY_V3, 1, 2), all(17)))
        else {
            panic!()
        };
        assert_eq!(t.quote, Quote::Other(other));
    }

    #[test]
    fn malformed_input_is_ignored() {
        let k = keys(30);
        // Too little data, too few accounts, account index out of range, other program,
        // unknown discriminator, truncated create.
        assert!(parse_instruction(&k, &ix(disc::BUY.to_vec(), all(16))).is_none());
        assert!(parse_instruction(&k, &ix(args(disc::BUY_V3, 1, 2), all(5))).is_none());
        assert!(parse_instruction(&k, &ix(args(disc::BUY, 1, 2), vec![200; 16])).is_none());
        assert!(parse_instruction(&k, &ix(args([9; 8], 1, 2), all(16))).is_none());
        assert!(parse_instruction(&k, &ix(vec![], all(16))).is_none());
        let mut create = disc::CREATE_V2.to_vec();
        create.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_instruction(&k, &ix(create, all(16))).is_none());
        let other = CompiledInstruction::new_from_raw_parts(1, args(disc::BUY, 1, 2), all(16));
        assert!(parse_instruction(&k, &other).is_none());
    }
}
