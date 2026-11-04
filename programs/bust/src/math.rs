//! Settlement rules in fixed point, matching the off-chain engine to the cent.
//! Prices are 1e-8, amounts USDC base units, rates ppm; intermediate values are u128 / SCALE (1e12).

use crate::constants::{PPM, SCALE};
use crate::state::{Params, Side};

const PPM_TO_SCALE: u128 = SCALE / PPM;

pub fn bust_price(side: Side, entry: u64, leverage: u16, liq_buffer_ppm: u32) -> u64 {
    let entry = entry as u128;
    let inv_lev = SCALE / leverage as u128;
    let buffer = liq_buffer_ppm as u128 * PPM_TO_SCALE;
    let factor = match side {
        Side::Long => SCALE - inv_lev + buffer,
        Side::Short => SCALE + inv_lev - buffer,
    };
    (entry * factor / SCALE) as u64
}

pub fn is_bust(side: Side, price: u64, bust: u64) -> bool {
    match side {
        Side::Long => price <= bust,
        Side::Short => price >= bust,
    }
}

/// PnL before haircuts (profit +, loss −)
pub fn raw_pnl(side: Side, entry: u64, exit: u64, notional: u64) -> i128 {
    let diff = exit as i128 - entry as i128;
    let pnl = notional as i128 * diff / entry as i128;
    match side {
        Side::Long => pnl,
        Side::Short => -pnl,
    }
}

/// Share held back inside the small-move band: 3.597 · width · u²(1−u)²
fn holdback(price_move: u128, p: &Params) -> u128 {
    let start = p.holdback_start_ppm as u128 * PPM_TO_SCALE;
    let end = p.holdback_end_ppm as u128 * PPM_TO_SCALE;
    if price_move <= start || price_move >= end {
        return 0;
    }
    let width = end - start;
    let u = (price_move - start) * SCALE / width;
    let v = SCALE - u;
    let t = (u * u / SCALE) * (v * v / SCALE) / SCALE;
    3597 * width / 1000 * t / SCALE
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinBreakdown {
    /// After haircuts, what leaves the vault (fee included)
    pub gain: u64,
    pub fee: u64,
    /// Profit left to the user
    pub net: u64,
}

/// The part of a winning close that the user keeps (deadband → holdback → impact → fee)
pub fn win_breakdown(entry: u64, exit: u64, notional: u64, position_multiplier_milli: u64, p: &Params) -> WinBreakdown {
    let zero = WinBreakdown { gain: 0, fee: 0, net: 0 };
    let entry_u = entry as u128;
    let price_move = (exit as i128 - entry as i128).unsigned_abs() * SCALE / entry_u;
    let deadband = p.deadband_ppm as u128 * PPM_TO_SCALE;
    let cut = deadband + holdback(price_move, p);
    if price_move <= cut {
        return zero;
    }
    let mv = price_move - cut;

    // scale = (1 − base)·move / (move + 1/rate + ref·1000/(1e6·mult_milli)), multiplied through by move
    let c1 = SCALE / p.impact_rate_multiplier as u128;
    let c2 = p.impact_reference_notional as u128 * SCALE * 1000 / (1_000_000 * position_multiplier_milli as u128);
    let keep = SCALE - p.impact_base_ppm as u128 * PPM_TO_SCALE;

    let mut gain = notional as u128 * mv / SCALE;
    gain = gain * keep / SCALE;
    gain = gain * mv / (mv + c1 + c2);
    let fee = gain * p.win_fee_ppm as u128 / PPM;
    WinBreakdown { gain: gain as u64, fee: fee as u64, net: (gain - fee) as u64 }
}

/// Open interest cap per market and side: max(0, net vault) × ratio
pub fn oi_cap(net_equity: i128, ratio_ppm: u32) -> u64 {
    if net_equity <= 0 {
        return 0;
    }
    (net_equity as u128 * ratio_ppm as u128 / PPM) as u64
}

pub fn fee_of(amount: u64, fee_ppm: u32) -> u64 {
    (amount as u128 * fee_ppm as u128 / PPM) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const BTC: u64 = 814_598;
    const ETH: u64 = 483_979;

    fn px(x: f64) -> u64 {
        (x * 1e8).round() as u64
    }
    fn usdc(x: f64) -> u64 {
        (x * 1e6).round() as u64
    }
    /// Compared with the off-chain engine: within one cent or 0.01%
    fn close(actual: u64, expected_usd: f64) {
        let expected = expected_usd * 1e6;
        let diff = (actual as f64 - expected).abs();
        assert!(diff <= 10_000f64.max(expected * 1e-4), "actual {actual} expected {expected}");
    }

    #[test]
    fn bust_prices_match_the_offchain_engine() {
        let p = Params::DEFAULT;
        assert_eq!(bust_price(Side::Long, px(100.0), 100, p.liq_buffer_ppm), px(99.05));
        assert_eq!(bust_price(Side::Short, px(100.0), 100, p.liq_buffer_ppm), px(100.95));
        assert_eq!(bust_price(Side::Long, px(100.0), 1000, p.liq_buffer_ppm), px(99.95));
        assert_eq!(bust_price(Side::Long, px(100.0), 2, p.liq_buffer_ppm), px(50.05));
        // 100.07870013 off-chain; 1/777 rounds in fixed point
        assert!((bust_price(Side::Short, px(100.0), 777, p.liq_buffer_ppm) as i64 - px(100.07870013) as i64).abs() <= 1);
    }

    #[test]
    fn win_breakdown_matches_the_offchain_engine() {
        let p = Params::DEFAULT;
        let cases: [(u64, f64, f64, f64, f64, f64); 9] = [
            // (multiplier, entry, exit, notional, expected gain, expected net)
            (BTC, 100.0, 101.0, 1000.0, 8.814692, 8.638398),
            (BTC, 100.0, 100.1, 1000.0, 0.739131, 0.724349),
            (ETH, 100.0, 101.0, 1000.0, 8.742597, 8.567745),
            (ETH, 100.0, 100.1, 50000.0, 34.483716, 33.794042),
            (ETH, 100.0, 99.0, 1000.0, 8.742597, 8.567745),
            (BTC, 80000.0, 80002.4, 100000.0, 0.045129, 0.044227),
            (BTC, 80000.0, 80016.0, 100000.0, 4.068226, 3.986862),
            (ETH, 2000.0, 2000.8, 12345.0, 1.532264, 1.501619),
            (BTC, 80000.0, 84000.0, 250000.0, 11203.039940, 10978.979141),
        ];
        for (mult, entry, exit, notional, gain, net) in cases {
            let w = win_breakdown(px(entry), px(exit), usdc(notional), mult, &p);
            close(w.gain, gain);
            close(w.net, net);
            assert_eq!(w.gain - w.fee, w.net);
        }
        // Nothing is paid inside the dead band
        assert_eq!(win_breakdown(px(100.0), px(100.001), usdc(1000.0), ETH, &p).gain, 0);
    }

    #[test]
    fn raw_pnl_signs() {
        assert_eq!(raw_pnl(Side::Long, px(100.0), px(101.0), usdc(1000.0)), usdc(10.0) as i128);
        assert_eq!(raw_pnl(Side::Short, px(100.0), px(101.0), usdc(1000.0)), -(usdc(10.0) as i128));
    }

    #[test]
    fn oi_cap_tracks_equity() {
        assert_eq!(oi_cap(usdc(25_000.0) as i128, 600_000), usdc(15_000.0));
        assert_eq!(oi_cap(-5, 600_000), 0);
    }
}
