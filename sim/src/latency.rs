//! A trader who sees Binance before Pyth. At request time t they compare Binance's last price with Pyth's and open in
//! the direction of the gap; the program fills them at Pyth's first update at t + 2 s and closes them at the first
//! update 2 s after their close request. Does knowing where Binance is predict Pyth past the fill delay?
//!
//! Data: Binance 1s klines and Hermes samples for a few days (scripts/fetch_pyth.py). Liquidation is checked on
//! Binance's second-by-second path, rescaled to the Pyth entry price.

use std::collections::HashMap;

use bust::{Params, Side};
use serde::Deserialize;

use crate::{data, rules::settle};

#[derive(Deserialize)]
struct Px {
    price: i64,
    conf: u64,
    expo: i32,
}

#[derive(Deserialize)]
struct Sample {
    t: i64,
    /// offset (s) → asset → Hermes' first update in second t + offset
    pyth: HashMap<String, HashMap<String, Px>>,
}

impl Px {
    fn value(&self) -> f64 {
        self.price as f64 * 10f64.powi(self.expo)
    }
    fn conf(&self) -> f64 {
        self.conf as f64 * 10f64.powi(self.expo)
    }
}

/// (offset of the close fill, label): closes requested 0, 3 and 8 s after the open fill
const HOLDS: [(&str, &str); 3] = [("4", "2 s"), ("7", "5 s"), ("12", "10 s")];
const LEVERAGES: [u16; 2] = [100, 1000];
const THRESHOLDS_BPS: [f64; 4] = [0.0, 2.0, 5.0, 10.0];

struct Trade {
    /// Binance's last price over Pyth's at request time, minus one, less the median over the day's random and fast
    /// moments for the market: USDT trades away from USD, and that basis is no news about where Pyth goes
    gap: f64,
    fast: bool,
    market: usize,
    entry: f64,
    entry_conf: f64,
    exits: [f64; 3],
    /// lowest and highest price between the open fill and each close fill, on the Binance path
    path: [(f64, f64); 3],
}

/// One file of samples: `DAY`, or `DAY-top` for the day's sharpest moments. The basis comes from `DAY` in both cases,
/// since the sharpest moments are a one-sided sample of the gap
fn load(days: &[String]) -> (Vec<Trade>, Vec<String>) {
    let mut out = Vec::new();
    let mut bases = Vec::new();
    for name in days {
        let mut trades = load_file(name);
        let usual = if name.len() > 10 { load_file(&name[..10]) } else { Vec::new() };
        for (market, asset) in data::ASSETS.iter().enumerate() {
            let reference = if usual.is_empty() { &trades } else { &usual };
            if let Some(basis) = basis_of(reference, market) {
                bases.push(format!("{name} {asset} {:+.1}", basis * 1e4));
                remove_basis(&mut trades, market, basis);
            }
        }
        out.extend(trades);
    }
    (out, bases)
}

/// The median gap of `market`, if it has any trades
fn basis_of(trades: &[Trade], market: usize) -> Option<f64> {
    let mut gaps: Vec<f64> = trades.iter().filter(|t| t.market == market).map(|t| t.gap).collect();
    gaps.sort_by(f64::total_cmp);
    gaps.get(gaps.len() / 2).copied()
}

fn remove_basis(trades: &mut [Trade], market: usize, basis: f64) {
    trades.iter_mut().filter(|t| t.market == market).for_each(|t| t.gap -= basis);
}

/// Opens toward Binance when the gap is wider than `threshold_bps`
fn follow(t: &Trade, threshold_bps: f64) -> Option<Side> {
    if t.gap.abs() * 1e4 <= threshold_bps {
        None
    } else if t.gap > 0.0 {
        Some(Side::Long)
    } else {
        Some(Side::Short)
    }
}

/// Opens against Binance when the gap is wider than `threshold_bps`
fn fade(t: &Trade, threshold_bps: f64) -> Option<Side> {
    follow(t, threshold_bps).map(|s| if s == Side::Long { Side::Short } else { Side::Long })
}

fn load_file(name: &str) -> Vec<Trade> {
    let day = &name[..10];
    let mut out = Vec::new();
    let path = data::data_dir().join("pyth").join(format!("samples-{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e} (run scripts/fetch_pyth.py)", path.display()));
    let samples: Vec<Sample> = serde_json::from_str(&text).unwrap();
    for (market, asset) in data::ASSETS.iter().enumerate() {
        let secs: HashMap<i64, data::Candle> = data::seconds(asset, day).into_iter().map(|c| (c.time / 1000, c)).collect();
        for s in &samples {
            // a feed with no update within 10 s of one of the moments is left out of this sample
            if !s.pyth.values().all(|m| m.contains_key(*asset)) {
                continue;
            }
            let px = |o: &str| &s.pyth[o][*asset];
            let (Some(seen), Some(before)) = (secs.get(&(s.t - 1)), secs.get(&(s.t - 4))) else { continue };
            let entry = px("2").value();
            let Some(at_entry) = secs.get(&(s.t + 2)) else { continue };
            let scale = entry / at_entry.open;
            let mut path = [(0.0, 0.0); 3];
            let mut exits = [0.0; 3];
            let mut complete = true;
            for (n, (offset, _)) in HOLDS.iter().enumerate() {
                exits[n] = px(offset).value();
                let end = s.t + offset.parse::<i64>().unwrap();
                let (mut lo, mut hi) = (f64::MAX, f64::MIN);
                for sec in (s.t + 2)..=end {
                    match secs.get(&sec) {
                        Some(c) => {
                            lo = lo.min(c.low * scale);
                            hi = hi.max(c.high * scale);
                        }
                        None => complete = false,
                    }
                }
                path[n] = (lo.min(entry).min(exits[n]), hi.max(entry).max(exits[n]));
            }
            if !complete {
                continue;
            }
            out.push(Trade {
                gap: seen.close / px("0").value() - 1.0,
                fast: (seen.close / before.close - 1.0).abs() * 1e4 > 3.0,
                market,
                entry,
                entry_conf: px("2").conf(),
                exits,
                path,
            });
        }
    }
    out
}

#[derive(Default)]
struct Tally {
    trades: usize,
    skipped: usize,
    user: f64,
    user_sq: f64,
    notional: f64,
    margin: f64,
}

fn run_strategy(trades: &[&Trade], leverage: u16, hold: usize, mut side_of: impl FnMut(&Trade) -> Option<Side>) -> Tally {
    let notional = 1_000.0;
    let max_conf = Params::DEFAULT.max_conf_margin_ppm as f64 / 1e6;
    let mut t = Tally::default();
    for tr in trades {
        let Some(side) = side_of(tr) else { continue };
        // the open is held to the leverage confidence bound; one that fails it cancels
        if tr.entry_conf / tr.entry * leverage as f64 > max_conf {
            t.skipped += 1;
            continue;
        }
        let (lo, hi) = tr.path[hold];
        let o = settle(tr.market, side, tr.entry, tr.exits[hold], lo, hi, notional, leverage, false);
        t.trades += 1;
        t.user += o.user;
        t.user_sq += o.user * o.user;
        t.notional += notional;
        t.margin += notional / leverage as f64;
    }
    t
}

fn cell(t: &Tally) -> String {
    if t.trades == 0 {
        return "—".into();
    }
    let n = t.trades as f64;
    let mean = t.user / n;
    let variance = (t.user_sq - n * mean * mean) / (n - 1.0).max(1.0);
    let stderr = (variance / n).sqrt();
    format!(
        "{:+.1} ± {:.1} bps / {:+.1}% ({})",
        t.user / t.notional * 1e4,
        stderr / (t.notional / n) * 1e4,
        t.user / t.margin * 100.0,
        t.trades
    )
}

pub fn run(days: &[String]) {
    let (trades, bases) = load(days);
    let fast: Vec<&Trade> = trades.iter().filter(|t| t.fast).collect();
    let all: Vec<&Trade> = trades.iter().collect();
    println!("## Latency: a trader who sees Binance before Pyth\n");
    println!(
        "{} moments × markets over {} ({} after a 3 bps move on Binance in the last 3 s). Each cell: the trader's mean result per trade ± one standard error, in bps of notional / % of margin (trades). Positive means the vault pays.\n",
        all.len(),
        days.join(", "),
        fast.len()
    );
    let gaps: Vec<f64> = all.iter().map(|t| t.gap.abs() * 1e4).collect();
    let mut sorted = gaps.clone();
    sorted.sort_by(f64::total_cmp);
    println!(
        "Binance − Pyth gap at request time, less the median for the day and market: median {:.1} bps, 90th percentile {:.1}, 99th {:.1}. The medians taken out, in bps: {}.\n",
        sorted[sorted.len() / 2],
        sorted[sorted.len() * 9 / 10],
        sorted[sorted.len() * 99 / 100],
        bases.join(", ")
    );
    let mut rng: u64 = 7;
    let mut coin = move || {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        if rng >> 63 == 0 {
            Side::Long
        } else {
            Side::Short
        }
    };
    for &leverage in &LEVERAGES {
        println!("**{leverage}×**\n");
        println!("| strategy | {} |", HOLDS.map(|h| format!("hold {}", h.1)).join(" | "));
        println!("|---|{}|", HOLDS.map(|_| "---").join("|"));
        let sides: Vec<Side> = (0..all.len()).map(|_| coin()).collect();
        let random: Vec<String> = (0..HOLDS.len())
            .map(|h| {
                let mut i = 0;
                cell(&run_strategy(&all, leverage, h, |_| {
                    i += 1;
                    Some(sides[i - 1])
                }))
            })
            .collect();
        println!("| random side, all moments | {} |", random.join(" | "));
        for &th in &THRESHOLDS_BPS {
            for (name, set) in [("all moments", &all), ("after a fast move", &fast)] {
                let row: Vec<String> =
                    (0..HOLDS.len()).map(|h| cell(&run_strategy(set, leverage, h, |t| follow(t, th)))).collect();
                println!("| follow the gap > {th} bps, {name} | {} |", row.join(" | "));
            }
            let row: Vec<String> = (0..HOLDS.len()).map(|h| cell(&run_strategy(&all, leverage, h, |t| fade(t, th)))).collect();
            println!("| fade the gap > {th} bps, all moments | {} |", row.join(" | "));
        }
        for (name, set, end) in [("all moments", &all, ""), ("after a fast move", &fast, "\n")] {
            let oracle: Vec<String> = (0..HOLDS.len())
                .map(|h| {
                    cell(&run_strategy(set, leverage, h, |t| Some(if t.exits[h] >= t.entry { Side::Long } else { Side::Short })))
                })
                .collect();
            println!("| knows the close price (upper bound), {name} | {} |{end}", oracle.join(" | "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(gap_bps: f64, market: usize, exit: f64) -> Trade {
        let entry = 100.0;
        Trade {
            gap: gap_bps / 1e4,
            fast: true,
            market,
            entry,
            entry_conf: 0.0,
            exits: [exit; 3],
            path: [(entry.min(exit), entry.max(exit)); 3],
        }
    }

    fn gaps(trades: &[Trade]) -> Vec<f64> {
        trades.iter().map(|t| (t.gap * 1e4 * 1e6).round() / 1e6).collect()
    }

    #[test]
    fn basis_is_the_median_of_the_market() {
        let mut trades = vec![trade(10.0, 0, 100.0), trade(12.0, 0, 100.0), trade(20.0, 0, 100.0), trade(-3.0, 1, 100.0)];
        let basis = basis_of(&trades, 0).unwrap();
        assert!((basis * 1e4 - 12.0).abs() < 1e-9);
        remove_basis(&mut trades, 0, basis);
        assert_eq!(gaps(&trades), vec![-2.0, 0.0, 8.0, -3.0]); // the other market keeps its gap
    }

    #[test]
    fn sharpest_moments_take_the_basis_from_the_usual_sample() {
        let mut sharp = vec![trade(30.0, 0, 100.0), trade(40.0, 0, 100.0)];
        let basis = basis_of(&[trade(10.0, 0, 100.0)], 0).unwrap();
        remove_basis(&mut sharp, 0, basis);
        assert_eq!(gaps(&sharp), vec![20.0, 30.0]);
    }

    #[test]
    fn no_samples_no_basis() {
        assert!(basis_of(&[trade(1.0, 1, 100.0)], 0).is_none());
    }

    #[test]
    fn follow_and_fade_take_opposite_sides_past_the_threshold() {
        let (up, down, small) = (trade(6.0, 0, 100.0), trade(-6.0, 0, 100.0), trade(4.0, 0, 100.0));
        assert!(follow(&up, 5.0) == Some(Side::Long) && fade(&up, 5.0) == Some(Side::Short));
        assert!(follow(&down, 5.0) == Some(Side::Short) && fade(&down, 5.0) == Some(Side::Long));
        assert!(follow(&small, 5.0).is_none() && fade(&small, 5.0).is_none());
    }

    #[test]
    fn a_trader_who_wins_shows_as_positive() {
        let up = trade(0.0, 0, 100.3); // 30 bps
        let long = run_strategy(&[&up], 100, 0, |_| Some(Side::Long));
        let short = run_strategy(&[&up], 100, 0, |_| Some(Side::Short));
        assert!(long.user > 0.0);
        assert!(short.user < 0.0);
    }

    #[test]
    fn leverage_confidence_bound_cancels_the_open() {
        let mut wide = trade(0.0, 0, 100.0);
        wide.entry_conf = 0.04; // 4 bps: 40% of the margin's move at 1000×, 4% at 100×
        let high = run_strategy(&[&wide], 1000, 0, |_| Some(Side::Long));
        let low = run_strategy(&[&wide], 100, 0, |_| Some(Side::Long));
        assert_eq!((high.trades, high.skipped), (0, 1));
        assert_eq!((low.trades, low.skipped), (1, 0));
    }

    #[test]
    fn cell_gives_the_mean_and_its_standard_error() {
        // two trades of +1 and +3 on 1,000 each: mean 2, standard error 1, so 20 ± 10 bps, 2% at 10×
        let t = Tally { trades: 2, skipped: 0, user: 4.0, user_sq: 10.0, notional: 2_000.0, margin: 200.0 };
        assert_eq!(cell(&t), "+20.0 ± 10.0 bps / +2.0% (2)");
        assert_eq!(cell(&Tally::default()), "—");
    }
}
