//! On-chain accounts of the Pump.fun program, decoded by hand from the IDL layouts (so no
//! Anchor dependency). Decoders never panic: short or foreign data returns `None`.
use solana_sdk::pubkey::Pubkey;

use super::parse::Quote;

pub const BONDING_CURVE_DISCRIMINATOR: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
pub const GLOBAL_DISCRIMINATOR: [u8; 8] = [167, 232, 232, 177, 200, 108, 114, 127];

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8], at: usize) -> Self {
        Self { data, at }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.data.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn bool(&mut self) -> Option<bool> {
        Some(self.take(1)?[0] != 0)
    }
    fn key(&mut self) -> Option<Pubkey> {
        Some(Pubkey::new_from_array(self.take(32)?.try_into().ok()?))
    }
}

/// A token's bonding curve: its reserves, creator and what it is priced in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BondingCurve {
    pub virtual_token_reserves: u64,
    /// Virtual reserves of the quote asset (lamports for SOL, millionths for USDC). Signed:
    /// since 30 Sep 2026 the fees kept on the curve are subtracted from it, so it can be
    /// negative (the IDL still calls it u64).
    pub virtual_quote_reserves: i64,
    pub real_token_reserves: u64,
    pub real_quote_reserves: u64,
    pub token_total_supply: u64,
    /// The curve is full and the token moved (or is moving) to PumpSwap.
    pub complete: bool,
    pub creator: Pubkey,
    pub quote: Quote,
    pub quote_mint: Pubkey,
}

impl BondingCurve {
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.get(..8)? != BONDING_CURVE_DISCRIMINATOR {
            return None;
        }
        let mut c = Cursor::new(data, 8);
        let virtual_token_reserves = c.u64()?;
        let virtual_quote_reserves = c.u64()? as i64;
        let real_token_reserves = c.u64()?;
        let real_quote_reserves = c.u64()?;
        let token_total_supply = c.u64()?;
        let complete = c.bool()?;
        let creator = c.key()?;
        // Curves created before USDC support end here; later fields default to SOL.
        let quote_mint = (|| {
            let _is_mayhem = c.bool()?;
            let _is_cashback = c.bool()?;
            c.key()
        })()
        .unwrap_or_default();
        Some(Self {
            virtual_token_reserves,
            virtual_quote_reserves,
            real_token_reserves,
            real_quote_reserves,
            token_total_supply,
            complete,
            creator,
            quote: Quote::from_mint(quote_mint),
            quote_mint: if quote_mint == Pubkey::default() {
                super::WSOL_MINT
            } else {
                quote_mint
            },
        })
    }

    /// Price of one whole token (6 decimals) in whole quote units (9 decimals for SOL, 6 for
    /// USDC), from the virtual reserves. `None` when the curve is empty.
    pub fn price(&self, quote_decimals: u32) -> Option<f64> {
        if self.virtual_token_reserves == 0 || self.virtual_quote_reserves <= 0 {
            return None;
        }
        let per_raw = self.virtual_quote_reserves as f64 / self.virtual_token_reserves as f64;
        Some(per_raw * 10f64.powi(6 - quote_decimals as i32))
    }

    /// Tokens (raw units) that `quote_in` buys right now, ignoring fees (a slight
    /// overestimate; the program takes its fee out of the spend).
    pub fn tokens_for_quote(&self, quote_in: u64) -> Option<u64> {
        if self.virtual_quote_reserves <= 0 {
            return None;
        }
        let (vt, vq) = (
            self.virtual_token_reserves as u128,
            self.virtual_quote_reserves as u128,
        );
        if vt == 0 {
            return None;
        }
        let out = vt - vt * vq / (vq + quote_in as u128);
        u64::try_from(out.min(self.real_token_reserves as u128)).ok()
    }
}

/// The parts of the program's global settings the bot needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Global {
    pub fee_recipient: Pubkey,
    pub fee_recipients: [Pubkey; 7],
    pub buyback_fee_recipients: [Pubkey; 8],
    pub whitelisted_quote_mints: Vec<Pubkey>,
}

impl Global {
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.get(..8)? != GLOBAL_DISCRIMINATOR {
            return None;
        }
        let mut c = Cursor::new(data, 8);
        c.take(1)?; // initialized
        c.take(32)?; // authority
        let fee_recipient = c.key()?;
        c.take(5 * 8)?; // initial reserves, supply, fee bps
        c.take(32)?; // withdraw authority
        c.take(1)?; // enable_migrate
        c.take(2 * 8)?; // pool migration fee, creator fee bps
        let mut fee_recipients = [Pubkey::default(); 7];
        for k in &mut fee_recipients {
            *k = c.key()?;
        }
        c.take(2 * 32)?; // set_creator_authority, admin_set_creator_authority
        c.take(1)?; // create_v2_enabled
        c.take(32)?; // whitelist_pda
        c.take(32)?; // reserved_fee_recipient
        c.take(1)?; // mayhem_mode_enabled
        c.take(7 * 32)?; // reserved_fee_recipients
        c.take(1)?; // is_cashback_enabled
        let mut buyback_fee_recipients = [Pubkey::default(); 8];
        for k in &mut buyback_fee_recipients {
            *k = c.key()?;
        }
        c.take(8)?; // buyback_basis_points
        c.take(8)?; // initial_virtual_quote_reserves
        let whitelisted_quote_mints = vec![c.key()?];
        Some(Self {
            fee_recipient,
            fee_recipients,
            buyback_fee_recipients,
            whitelisted_quote_mints,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve_bytes(complete: bool, quote: Option<Pubkey>) -> Vec<u8> {
        let mut v = BONDING_CURVE_DISCRIMINATOR.to_vec();
        for n in [
            1_000_000_000_000u64,
            30_000_000_000,
            800_000_000_000,
            5,
            10u64.pow(15),
        ] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        v.push(complete as u8);
        v.extend_from_slice(&[7u8; 32]); // creator
        if let Some(q) = quote {
            v.extend_from_slice(&[0, 0]);
            v.extend_from_slice(q.as_ref());
            v.extend_from_slice(&[0u8; 60]); // the rest of the account
        }
        v
    }

    #[test]
    fn decodes_curves_old_and_new() {
        let old = BondingCurve::decode(&curve_bytes(false, None)).unwrap();
        assert_eq!(old.quote, Quote::Sol);
        assert_eq!(old.quote_mint, super::super::WSOL_MINT);
        assert_eq!(old.creator, Pubkey::new_from_array([7; 32]));
        assert!(!old.complete);
        assert_eq!(old.virtual_quote_reserves, 30_000_000_000);

        let usdc = BondingCurve::decode(&curve_bytes(true, Some(super::super::USDC_MINT))).unwrap();
        assert_eq!(usdc.quote, Quote::Usdc);
        assert!(usdc.complete);
    }

    #[test]
    fn rejects_foreign_or_short_data() {
        assert!(BondingCurve::decode(&[]).is_none());
        assert!(BondingCurve::decode(&[0; 100]).is_none());
        let mut short = curve_bytes(false, None);
        short.truncate(60);
        assert!(BondingCurve::decode(&short).is_none());
        assert!(Global::decode(&curve_bytes(false, None)).is_none());
    }

    #[test]
    fn price_and_quote_math() {
        let c = BondingCurve::decode(&curve_bytes(false, None)).unwrap();
        // 30 SOL / 1,000,000 tokens (6 decimals) = 0.00003 SOL per token.
        let p = c.price(9).unwrap();
        assert!((p - 0.00003).abs() < 1e-12, "{p}");
        // 1 SOL against the 30 SOL virtual reserve buys ~3.2% of the virtual tokens.
        let out = c.tokens_for_quote(1_000_000_000).unwrap();
        assert_eq!(out, 32_258_064_517);
        // Never more than the real reserve.
        assert_eq!(c.tokens_for_quote(u64::MAX / 2).unwrap(), 800_000_000_000);
        let empty = BondingCurve {
            virtual_token_reserves: 0,
            ..c
        };
        assert!(empty.price(9).is_none() && empty.tokens_for_quote(1).is_none());
        // Negative virtual quote reserves (fees kept on the curve) are not priced, not
        // read as a huge positive number.
        let negative = BondingCurve {
            virtual_quote_reserves: -5,
            ..c
        };
        assert!(negative.price(9).is_none() && negative.tokens_for_quote(1).is_none());
        let mut raw = curve_bytes(false, None);
        raw[16..24].copy_from_slice(&(-5i64).to_le_bytes());
        assert_eq!(
            BondingCurve::decode(&raw).unwrap().virtual_quote_reserves,
            -5
        );
    }
}
