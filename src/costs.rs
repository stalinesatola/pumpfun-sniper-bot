//! What a trade costs in fees, so the dashboard can show it. Every transaction the bot sends
//! pays the landing tip, a priority fee and the network base fee, and the bot sends two per
//! round trip (a buy and a sell). On small positions that can be most of the stake.

/// Compute-unit limit set on every buy and sell transaction.
pub const COMPUTE_UNIT_LIMIT: u32 = 150_000;
/// Priority fee price (micro-lamports per compute unit) set on every transaction.
pub const CU_PRICE_MICROLAMPORTS: u64 = 1_000_000;
/// Solana's fee per signature.
pub const BASE_FEE_LAMPORTS: u64 = 5_000;

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Fees of one transaction besides the tip, in SOL.
pub fn fixed_fee_per_tx_sol() -> f64 {
    let priority_lamports = COMPUTE_UNIT_LIMIT as u64 * CU_PRICE_MICROLAMPORTS / 1_000_000;
    (priority_lamports + BASE_FEE_LAMPORTS) as f64 / LAMPORTS_PER_SOL
}

/// Fees of a whole round trip (buy + sell) with the given landing tip, in SOL. The rent of
/// the token account (about 0.002 SOL) is paid on the buy but refunded when the position
/// is closed, so it is not counted.
pub fn round_trip_cost_sol(tip_sol: f64) -> f64 {
    2.0 * (tip_sol + fixed_fee_per_tx_sol())
}

/// Round-trip fees as a percentage of the amount bought.
pub fn cost_pct_of_buy(buy_sol: f64, tip_sol: f64) -> f64 {
    if buy_sol <= 0.0 {
        return 0.0;
    }
    round_trip_cost_sol(tip_sol) / buy_sol * 100.0
}

/// Above this share of the stake the fees alone make a profit unlikely.
pub const HIGH_COST_PCT: f64 = 20.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fees_match_the_transactions_the_bot_builds() {
        // 150,000 CU x 1 lamport/CU = 150,000 lamports priority, plus 5,000 base.
        assert!((fixed_fee_per_tx_sol() - 0.000155).abs() < 1e-12);
        // Tip 0.001: 2 x (0.001 + 0.000155)
        assert!((round_trip_cost_sol(0.001) - 0.00231).abs() < 1e-12);
    }

    #[test]
    fn small_buys_are_eaten_by_fees() {
        // The example from the dashboard: buy 0.001 SOL with a 0.001 SOL tip.
        assert!(cost_pct_of_buy(0.001, 0.001) > 200.0);
        // A 0.1 SOL buy with the default 0.0001 tip is a few percent.
        assert!(cost_pct_of_buy(0.1, 0.0001) < HIGH_COST_PCT);
        assert_eq!(cost_pct_of_buy(0.0, 0.001), 0.0);
    }
}
