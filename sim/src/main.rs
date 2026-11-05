//! Runs the program's settlement rules over Binance minute candles and prints the tables in the README's Economics
//! section. `cargo run --release -- all` after `python3 scripts/fetch_binance.py minute`.
//!
//! - edge:  what the vault keeps from traders taking random sides, and what hedged two-account xBUST farming costs
//! - debt:  the debt an empty vault can run up under the open interest floors, for several kinds of trader
//! - latency: the edge of a trader who sees Binance before Pyth (latency.rs); `-- latency DAY ...`
//! - caps:  the vault's loss with every cap full on one side, per-market caps alone against the total cap,
//!   over the last 12 months and each crisis month

mod data;
mod latency;
mod rules;

use bust::{Params, Side};
use data::{Candle, Extremes, ASSETS};
use rules::{settle, win_cost};

const LAST_12: [&str; 12] = [
    "2025-11", "2025-12", "2026-01", "2026-02", "2026-03", "2026-04", "2026-05", "2026-06", "2026-07", "2026-08", "2026-09",
    "2026-10",
];
const CRISES: [(&str, &str); 6] = [
    ("2020-03", "COVID crash, 12 March 2020"),
    ("2021-05", "19 May 2021"),
    ("2022-05", "LUNA, May 2022"),
    ("2022-11", "FTX, November 2022"),
    ("2024-08", "5 August 2024"),
    ("2025-10", "10 October 2025"),
];
const SIDES: [Side; 2] = [Side::Long, Side::Short];

struct Markets {
    /// Indexes into ASSETS of the markets in this data set (SOL was not listed in March 2020)
    ids: Vec<usize>,
    candles: Vec<Vec<Candle>>,
    extremes: Vec<Extremes>,
}

impl Markets {
    fn load(months: &[&str]) -> Self {
        let (ids, series): (Vec<usize>, Vec<Vec<Candle>>) =
            ASSETS.iter().enumerate().filter_map(|(i, a)| data::minutes(a, months).map(|c| (i, c))).unzip();
        let candles = data::align(series);
        let extremes = candles.iter().map(|c| Extremes::new(c)).collect();
        Markets { ids, candles, extremes }
    }
    fn len(&self) -> usize {
        self.candles[0].len()
    }
    /// A position on market k from candle i for `hold` candles
    #[allow(clippy::too_many_arguments)]
    fn trade(&self, k: usize, i: usize, hold: usize, side: Side, notional: f64, leverage: u16, in_debt: bool) -> rules::Outcome {
        let c = &self.candles[k];
        let (low, high) = self.extremes[k].range(i, i + hold);
        settle(self.ids[k], side, c[i].open, c[i + hold - 1].close, low, high, notional, leverage, in_debt)
    }
}

fn usd(x: f64) -> String {
    let s = format!("{:.0}", x.abs());
    let mut out = String::new();
    for (n, ch) in s.chars().enumerate() {
        if n > 0 && (s.len() - n) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if x < -0.5 {
        format!("−{out}")
    } else {
        out
    }
}

// ---------- edge and farming

/// Which (leverage, hold) cells a population trades
type Population = fn(u16, usize) -> bool;

const LEVERAGES: [u16; 7] = [2, 10, 50, 100, 250, 500, 1000];
const HOLDS: [usize; 5] = [1, 5, 15, 60, 240];

#[derive(Default, Clone, Copy)]
struct Cell {
    notional: f64,
    user: f64,
    loss: f64,
    fees: f64,
    vault: f64,
    trades: f64,
    liquidated: f64,
    farm_cost: f64,
    farm_counted: f64,
}

fn edge(m: &Markets) {
    let notional = 1_000.0;
    let mut cells = Vec::new();
    for &leverage in &LEVERAGES {
        for &hold in &HOLDS {
            let mut c = Cell::default();
            for k in 0..m.ids.len() {
                for i in (0..m.len() - hold).step_by(7) {
                    let long = m.trade(k, i, hold, Side::Long, notional, leverage, false);
                    let short = m.trade(k, i, hold, Side::Short, notional, leverage, false);
                    for o in [long, short] {
                        c.notional += notional;
                        c.user += o.user;
                        c.loss += (-o.user).max(0.0);
                        c.fees += o.fee;
                        c.vault += o.vault;
                        c.trades += 1.0;
                        c.liquidated += o.liquidated as u8 as f64;
                    }
                    c.farm_cost += -(long.user + short.user);
                    c.farm_counted += (-long.user).max(0.0) + (-short.user).max(0.0);
                }
            }
            cells.push((leverage, hold, c));
        }
    }

    println!("## Edge: traders taking random sides\n");
    println!("Last 12 months of minute candles, an entry every 7 minutes on each market, long and short. Values are what the vault and fees keep, in bps of volume; then the share of positions liquidated.\n");
    println!("| leverage \\ hold | {} |", HOLDS.map(|h| format!("{h} min")).join(" | "));
    println!("|---|{}|", HOLDS.map(|_| "---").join("|"));
    for &l in &LEVERAGES {
        let row: Vec<String> = cells
            .iter()
            .filter(|(lv, _, _)| *lv == l)
            .map(|(_, _, c)| format!("{:.1} / {:.0}%", -c.user / c.notional * 1e4, c.liquidated / c.trades * 100.0))
            .collect();
        println!("| {l}× | {} |", row.join(" | "));
    }

    // Farming: the losing account's loss earns xBUST. A dollar of loss earns xbust × pool / total loss, with
    // pool = fees + (1 − keep) × vault profit; it pays when that beats c = the pair's loss per dollar counted
    let p = bust::DistParams::DEFAULT;
    let xbust = p.xbust_ppm as f64 / 1e6;
    let keep = p.keep_min_ppm as f64 / 1e6;
    let populations: [(&str, Population); 3] = [
        ("mixed (every leverage and hold)", |_, _| true),
        ("degen (100×+, ≤15 min)", |l, h| l >= 100 && h <= 15),
        ("calm (≤50×, ≥15 min)", |l, h| l <= 50 && h >= 15),
    ];
    let reward = |f: Population| {
        let (mut loss, mut fees, mut vault) = (0.0, 0.0, 0.0);
        for (l, h, c) in &cells {
            if f(*l, *h) {
                loss += c.loss;
                fees += c.fees;
                vault += c.vault;
            }
        }
        (fees + (1.0 - keep) * vault.max(0.0)) / loss
    };
    let mut farm: Vec<(u16, usize, f64)> = cells.iter().map(|(l, h, c)| (*l, *h, c.farm_cost / c.farm_counted)).collect();
    farm.sort_by(|a, b| a.2.total_cmp(&b.2));
    println!("\n## Hedged xBUST farming\n");
    println!("xBUST share {:.0}%, the vault keeping its minimum {:.0}% of profit.\n", xbust * 100.0, keep * 100.0);
    println!("| population | xBUST earned per dollar of loss |");
    println!("|---|---|");
    for (name, f) in &populations {
        println!("| {name} | {:.2} cents |", xbust * reward(*f) * 100.0);
    }
    println!("\n| cheapest strategies | cost per dollar of loss | break-even xBUST share (mixed / degen / calm) |");
    println!("|---|---|---|");
    for (l, h, c) in farm.iter().take(4) {
        let shares: Vec<String> = populations.iter().map(|(_, f)| format!("{:.0}%", c / reward(*f) * 100.0)).collect();
        println!("| {l}× / {h} min | {:.1} cents | {} |", c * 100.0, shares.join(" / "));
    }
}

// ---------- debt under the floors

const FLOOR: f64 = 25_000.0;
const MARKETS_AT_FLOOR: usize = 2; // the total floor fills two markets
const WINDOWS: [(usize, &str); 4] = [(1440, "24 h"), (4320, "72 h"), (10080, "7 days"), (43200, "30 days")];

/// Worst fall of a path within `w` steps: max over j of (max E[j−w..j] − E[j]), with a sliding maximum
fn worst_fall(path: &[f64], w: usize) -> f64 {
    let mut q: std::collections::VecDeque<usize> = Default::default();
    let mut worst: f64 = 0.0;
    for j in 0..path.len() {
        while q.front().is_some_and(|&i| i + w < j) {
            q.pop_front();
        }
        while q.back().is_some_and(|&i| path[i] <= path[j]) {
            q.pop_back();
        }
        q.push_back(j);
        worst = worst.max(path[*q.front().unwrap()] - path[j]);
    }
    worst
}

/// Knows each window's net direction in advance and holds the floor on the two markets that move most
fn trend_debt(m: &Markets, w: usize) -> f64 {
    let mut worst: f64 = 0.0;
    for i in 0..m.len().saturating_sub(w) {
        let mut gains: Vec<f64> = (0..m.ids.len())
            .map(|k| win_cost(m.ids[k], m.candles[k][i + w - 1].close / m.candles[k][i].open - 1.0, FLOOR))
            .collect();
        gains.sort_by(|a, b| b.total_cmp(a));
        worst = worst.max(gains.iter().take(MARKETS_AT_FLOOR).sum());
    }
    worst
}

/// Starting from an empty vault at the start of each day, the worst debt (−equity) reached within each window by a
/// trader who, every `step` minutes, holds the floor on the two markets that moved most in the last `step`, in that
/// direction. With `brake`, the floor shrinks as the program does past max_debt: floor × max_debt / debt
fn momentum_debt(m: &Markets, step: usize, leverage: u16, brake: bool) -> Vec<f64> {
    let max_debt = Params::DEFAULT.max_debt as f64 / 1e6;
    let mut worst = vec![0.0f64; WINDOWS.len()];
    for start in (step..m.len()).step_by(1440) {
        let mut equity: f64 = 0.0;
        let mut lowest: f64 = 0.0;
        let mut i = start;
        while i + step <= m.len() && i - start < WINDOWS[WINDOWS.len() - 1].0 {
            let mut picks: Vec<(usize, f64)> =
                (0..m.ids.len()).map(|k| (k, m.candles[k][i - 1].close / m.candles[k][i - step].open - 1.0)).collect();
            picks.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
            let floor = if brake && -equity > max_debt { FLOOR * max_debt / -equity } else { FLOOR };
            for &(k, mv) in picks.iter().take(MARKETS_AT_FLOOR) {
                let side = if mv >= 0.0 { Side::Long } else { Side::Short };
                equity += m.trade(k, i, step, side, floor, leverage, equity < 0.0).vault;
            }
            lowest = lowest.min(equity);
            i += step;
            for (n, &(w, _)) in WINDOWS.iter().enumerate() {
                if i - start <= w {
                    worst[n] = worst[n].max(-lowest);
                }
            }
        }
    }
    // within a window means within any shorter one too
    for n in 1..worst.len() {
        worst[n] = worst[n].max(worst[n - 1]);
    }
    worst
}

/// Each minute, four traders open about 1,235 USDC between them on a random market and side, at 10× to 1000×, for
/// 1 to 60 minutes: about half the total floor open on average
fn random_path(m: &Markets, seed: u64) -> Vec<f64> {
    let mut s = seed;
    let mut rand = move || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (s >> 33) as usize
    };
    let n = m.len();
    let mut due = vec![Vec::<(usize, usize, usize, Side, u16)>::new(); n + 61];
    let mut equity = 0.0;
    let mut path = Vec::with_capacity(n);
    for i in 0..n {
        for &(k, from, hold, side, leverage) in &due[i] {
            equity += m.trade(k, from, hold, side, 1_235.0 / 4.0, leverage, equity < 0.0).vault;
        }
        path.push(equity);
        for _ in 0..4 {
            let hold = [1, 5, 15, 60][rand() % 4];
            if i + hold < n {
                due[i + hold].push((rand() % m.ids.len(), i, hold, SIDES[rand() % 2], [10, 100, 500, 1000][rand() % 4]));
            }
        }
    }
    path
}

fn debt(m: &Markets, title: &str) {
    println!("\n## Debt under the floors: {title}\n");
    println!(
        "From an empty vault, floors of 25,000 USDC per market and 50,000 in total, worst debt within each window (USDC).\n"
    );
    println!("| trader | {} |", WINDOWS.map(|w| w.1).join(" | "));
    println!("|---|{}|", WINDOWS.map(|_| "---").join("|"));
    // "within" the window: the worst over every shorter window too; a window longer than the data is left out
    let fits = |w: usize| w < m.len();
    let mut worst: f64 = 0.0;
    let trend: Vec<String> = WINDOWS
        .iter()
        .map(|&(w, _)| {
            if !fits(w) {
                return "—".into();
            }
            worst = worst.max(trend_debt(m, w));
            usd(worst)
        })
        .collect();
    println!("| knows each window's direction in advance, no brake | {} |", trend.join(" | "));
    for (step, leverage, brake) in [(60, 10, false), (60, 10, true), (60, 100, true), (5, 100, true)] {
        let d = momentum_debt(m, step, leverage, brake);
        let cells: Vec<String> = WINDOWS.iter().zip(&d).map(|(&(w, _), x)| if fits(w) { usd(*x) } else { "—".into() }).collect();
        let how = if brake { "with the debt brake" } else { "no brake" };
        println!("| momentum every {step} min at {leverage}×, {how} | {} |", cells.join(" | "));
    }
    let randoms: Vec<Vec<f64>> = [1, 7, 42].iter().map(|&s| random_path(m, s)).collect();
    println!(
        "| random flow, worst fall (worst of 3 seeds) | {} |",
        WINDOWS
            .map(|(w, _)| if fits(w) { usd(randoms.iter().map(|p| worst_fall(p, w)).fold(0.0, f64::max)) } else { "—".into() })
            .join(" | ")
    );
    let ends: Vec<String> = randoms.iter().map(|p| usd(*p.last().unwrap())).collect();
    let in_debt: Vec<String> = randoms
        .iter()
        .map(|p| format!("{:.1}%", p.iter().filter(|e| **e < 0.0).count() as f64 / p.len() as f64 * 100.0))
        .collect();
    println!("\nRandom flow ends at {} USDC, in debt {} of the time.", ends.join(", "), in_debt.join(", "));
}

// ---------- caps

const CAP_WINDOWS: [(usize, &str); 4] = [(60, "1 h"), (240, "4 h"), (1440, "24 h"), (4320, "72 h")];

/// Worst loss as a share of the vault with every cap full on the winning side, closed at the best moment of the
/// window: per-market caps alone (each market at 0.6 of the vault) and with the total cap (0.6 over all markets)
fn cap_losses(m: &Markets, w: usize) -> (f64, f64) {
    let ratio = Params::DEFAULT.oi_cap_ratio_ppm as f64 / 1e6;
    let (mut per_market, mut total): (f64, f64) = (0.0, 0.0);
    for i in 0..m.len().saturating_sub(w) {
        let losses: Vec<f64> = (0..m.ids.len())
            .map(|k| {
                let open = m.candles[k][i].open;
                let (low, high) = m.extremes[k].range(i, i + w);
                win_cost(m.ids[k], (high / open - 1.0).max(1.0 - low / open), ratio)
            })
            .collect();
        per_market = per_market.max(losses.iter().sum());
        total = total.max(losses.iter().copied().fold(0.0, f64::max));
    }
    (per_market, total)
}

fn caps(sets: &[(String, Markets)]) {
    println!("\n## Caps with every position on the winning side\n");
    println!("Vault loss as a share of the vault, per-market caps alone → with the 0.6 total cap. The 0.6 was chosen so that this stays near half the vault.\n");
    println!("| period | {} |", CAP_WINDOWS.map(|w| w.1).join(" | "));
    println!("|---|{}|", CAP_WINDOWS.map(|_| "---").join("|"));
    for (name, m) in sets {
        let cells: Vec<String> = CAP_WINDOWS
            .iter()
            .map(|(w, _)| {
                let (a, b) = cap_losses(m, *w);
                format!("{:.2} → {:.2}", a, b)
            })
            .collect();
        println!("| {name} | {} |", cells.join(" | "));
    }
}

fn main() {
    let what = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    if what == "latency" {
        return latency::run(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    let year = Markets::load(&LAST_12);
    eprintln!("last 12 months: {} aligned minutes on {} markets", year.len(), year.ids.len());
    if what == "edge" || what == "all" {
        edge(&year);
    }
    if what == "debt" || what == "all" {
        debt(&year, "last 12 months");
    }
    if what == "caps" || what == "all" || what == "debt" {
        let mut sets = vec![("last 12 months".to_string(), year)];
        for (month, name) in CRISES {
            let m = Markets::load(&[month]);
            let label = if m.ids.len() < ASSETS.len() { format!("{name} (no SOL)") } else { name.to_string() };
            sets.push((label, m));
        }
        if what != "caps" {
            for (name, m) in &sets[1..] {
                debt(m, name);
            }
        }
        caps(&sets);
    }
}
