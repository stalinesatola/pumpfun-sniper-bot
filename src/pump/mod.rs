//! What the Pump.fun program looks like today: instruction discriminators, account positions
//! and the parsers for the transactions seen on the feed.
//!
//! Everything here comes from the official IDL (github.com/pump-fun/pump-public-docs). New
//! tokens are created with `create_v2` (Token-2022) and can be paired with SOL or USDC, and
//! trades come in several generations of instructions; the parser understands all of them.
pub mod accounts;
pub mod events;
pub mod ix;
pub mod market;
pub mod parse;

use solana_sdk::pubkey;
use solana_sdk::pubkey::Pubkey;

/// The wrapped SOL mint, the quote of SOL-paired tokens.
pub const WSOL_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
/// USDC, the quote of USDC-paired tokens.
pub const USDC_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
pub const TOKEN_2022_PROGRAM: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

/// Instruction discriminators: the first 8 bytes of sha256("global:<name>").
pub mod disc {
    pub const CREATE: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];
    pub const CREATE_V2: [u8; 8] = [214, 144, 76, 236, 95, 139, 49, 180];
    pub const BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
    pub const BUY_EXACT_SOL_IN: [u8; 8] = [56, 252, 116, 8, 158, 223, 205, 95];
    pub const BUY_V2: [u8; 8] = [184, 23, 238, 97, 103, 197, 211, 61];
    pub const BUY_V3: [u8; 8] = [7, 5, 29, 196, 245, 23, 101, 80];
    pub const BUY_EXACT_QUOTE_IN_V2: [u8; 8] = [194, 171, 28, 70, 104, 77, 91, 47];
    pub const BUY_EXACT_QUOTE_IN_V3: [u8; 8] = [225, 247, 80, 30, 213, 179, 132, 136];
    pub const SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
    pub const SELL_V2: [u8; 8] = [93, 246, 130, 60, 231, 233, 64, 178];
    pub const SELL_V3: [u8; 8] = [28, 146, 222, 119, 38, 196, 105, 213];
}
