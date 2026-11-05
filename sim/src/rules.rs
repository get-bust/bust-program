//! A position's settlement, computed by the program's own `bust::math` with `Params::DEFAULT`.

use bust::{math, Params, Side};

/// Impact size parameter × 1000 per market, as the markets are added on chain (SOL uses ETH's)
pub const MULTIPLIER_MILLI: [u64; 3] = [483_979, 814_598, 483_979];

const PRICE: f64 = 1e8;
const USDC: f64 = 1e6;

fn e8(p: f64) -> u64 {
    (p * PRICE).round() as u64
}

/// Settlement in USDC: what the trader gains or loses, what the vault's net position (cash − debt) moves by, and the
/// fee taken
#[derive(Clone, Copy, Debug, Default)]
pub struct Outcome {
    pub user: f64,
    pub vault: f64,
    pub fee: f64,
    pub liquidated: bool,
}

/// A position opened at `entry` and closed at `exit`, liquidated if `low`/`high` over its life reached the bust
/// price. `in_debt`: the vault owes the queue, so there are no fees and a win is owed (net) rather than paid (gain)
#[allow(clippy::too_many_arguments)]
pub fn settle(
    market: usize,
    side: Side,
    entry: f64,
    exit: f64,
    low: f64,
    high: f64,
    notional: f64,
    leverage: u16,
    in_debt: bool,
) -> Outcome {
    let p = Params::DEFAULT;
    let (entry, exit, notional_units) = (e8(entry), e8(exit), (notional * USDC).round() as u64);
    let margin = notional_units / leverage as u64;
    let bust = math::bust_price(side, entry, leverage, p.liq_buffer_ppm);
    let fee = |x: u64| if in_debt { 0 } else { math::fee_of(x, p.loss_fee_ppm) };
    let touched = match side {
        Side::Long => e8(low) <= bust,
        Side::Short => e8(high) >= bust,
    };
    if touched || math::is_bust(side, exit, bust) {
        let f = fee(margin);
        return Outcome { user: -(margin as f64), vault: (margin - f) as f64, fee: f as f64, liquidated: true }.scaled();
    }
    let raw = math::raw_pnl(side, entry, exit, notional_units);
    if raw < 0 {
        let loss = ((-raw) as u64).min(margin);
        let f = fee(loss);
        return Outcome { user: -(loss as f64), vault: (loss - f) as f64, fee: f as f64, liquidated: false }.scaled();
    }
    let w = math::win_breakdown(entry, exit, notional_units, MULTIPLIER_MILLI[market], &p);
    let owed = if in_debt { w.net } else { w.gain };
    Outcome { user: w.net as f64, vault: -(owed as f64), fee: if in_debt { 0.0 } else { w.fee as f64 }, liquidated: false }
        .scaled()
}

/// What the vault pays a winning position of `notional` USDC on a move of `|move|` (fee included), per the program
pub fn win_cost(market: usize, mv: f64, notional: f64) -> f64 {
    let entry = 100_000_000_000u64; // 1,000 at 1e-8, so the move is resolved finely
    let exit = (entry as f64 * (1.0 + mv.abs())).round() as u64;
    math::win_breakdown(entry, exit, (notional * USDC) as u64, MULTIPLIER_MILLI[market], &Params::DEFAULT).gain as f64 / USDC
}

impl Outcome {
    fn scaled(self) -> Self {
        Outcome { user: self.user / USDC, vault: self.vault / USDC, fee: self.fee / USDC, liquidated: self.liquidated }
    }
}
