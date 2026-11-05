//! Binance spot klines from sim/data/binance (see scripts/fetch_binance.py), aligned across markets.

use std::{fs, path::PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct Candle {
    /// Open time, unix milliseconds
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

pub const ASSETS: [&str; 3] = ["SOL", "BTC", "ETH"];

pub fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
}

/// One CSV of klines. Binance switched spot timestamps from milliseconds to microseconds in 2025
fn read(path: &PathBuf) -> Vec<Candle> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e} (run scripts/fetch_binance.py)", path.display()));
    text.lines()
        .filter(|l| !l.is_empty() && l.as_bytes()[0].is_ascii_digit())
        .map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            let t: i64 = f[0].parse().unwrap();
            Candle {
                time: if t > 100_000_000_000_000 { t / 1000 } else { t },
                open: f[1].parse().unwrap(),
                high: f[2].parse().unwrap(),
                low: f[3].parse().unwrap(),
                close: f[4].parse().unwrap(),
            }
        })
        .collect()
}

/// Minute candles for `months` (YYYY-MM) of one asset, or None if Binance did not list it then
pub fn minutes(asset: &str, months: &[&str]) -> Option<Vec<Candle>> {
    let mut out = Vec::new();
    for m in months {
        let path = data_dir().join("binance/1m").join(format!("{asset}USDT-1m-{m}.csv"));
        if !path.exists() {
            return None;
        }
        out.extend(read(&path));
    }
    Some(out)
}

/// Second candles of one asset for one day
pub fn seconds(asset: &str, day: &str) -> Vec<Candle> {
    read(&data_dir().join("binance/1s").join(format!("{asset}USDT-1s-{day}.csv")))
}

/// Keeps only the times every series has, so index i is the same minute in all of them
pub fn align(series: Vec<Vec<Candle>>) -> Vec<Vec<Candle>> {
    use std::collections::HashSet;
    let mut common: HashSet<i64> = series[0].iter().map(|c| c.time).collect();
    for s in &series[1..] {
        let times: HashSet<i64> = s.iter().map(|c| c.time).collect();
        common.retain(|t| times.contains(t));
    }
    series
        .into_iter()
        .map(|s| {
            let mut v: Vec<Candle> = s.into_iter().filter(|c| common.contains(&c.time)).collect();
            v.sort_by_key(|c| c.time);
            v
        })
        .collect()
}

/// O(1) min of lows and max of highs over any range, for liquidation checks
pub struct Extremes {
    lows: Vec<Vec<f64>>,
    highs: Vec<Vec<f64>>,
}

impl Extremes {
    pub fn new(c: &[Candle]) -> Self {
        let mut lows = vec![c.iter().map(|x| x.low).collect::<Vec<_>>()];
        let mut highs = vec![c.iter().map(|x| x.high).collect::<Vec<_>>()];
        let mut k = 1;
        while (1 << k) <= c.len() {
            let half = 1 << (k - 1);
            let (pl, ph) = (&lows[k - 1], &highs[k - 1]);
            let n = c.len() + 1 - (1 << k);
            lows.push((0..n).map(|i| pl[i].min(pl[i + half])).collect());
            highs.push((0..n).map(|i| ph[i].max(ph[i + half])).collect());
            k += 1;
        }
        Self { lows, highs }
    }

    /// (lowest low, highest high) over candles [from, to)
    pub fn range(&self, from: usize, to: usize) -> (f64, f64) {
        let k = (usize::BITS - 1 - (to - from).leading_zeros()) as usize;
        let j = to - (1 << k);
        (self.lows[k][from].min(self.lows[k][j]), self.highs[k][from].max(self.highs[k][j]))
    }
}
