//! Kani proofs for the settlement and accounting rules: they hold for every input, not for a sample.
//! `cargo kani -p bust --lib`. Wide multiplications and divisions are out of a solver's reach, so where a proof bounds
//! its inputs the bound sits above anything the vault can hold: AMOUNT is about 280 million USDC.

use crate::constants::PPM;
use crate::math::*;
use crate::state::{DistParams, Params, Side, Ticket};

const AMOUNT: u64 = 1 << 48;

fn any_side() -> Side {
    if kani::any() {
        Side::Long
    } else {
        Side::Short
    }
}

/// Valid leverage and any entry a Pyth price can scale to
fn valid_leverage() -> u16 {
    let leverage: u16 = kani::any();
    kani::assume(leverage >= 1 && leverage <= Params::DEFAULT.max_leverage);
    leverage
}

#[kani::proof]
fn bust_price_never_panics_and_sits_on_the_losing_side() {
    let entry: u64 = kani::any();
    kani::assume(entry > 0 && entry <= 1 << 54);
    let leverage = valid_leverage();
    kani::assume(leverage >= 2);
    let buffer = Params::DEFAULT.liq_buffer_ppm;
    let long = bust_price(Side::Long, entry, leverage, buffer);
    let short = bust_price(Side::Short, entry, leverage, buffer);
    assert!(long <= entry);
    assert!(short >= entry);
}

#[kani::proof]
fn a_price_clearly_bust_is_bust() {
    let (price, conf, bust): (u64, u64, u64) = (kani::any(), kani::any(), kani::any());
    let side = any_side();
    if is_clearly_bust(side, price, conf, bust) {
        assert!(is_bust(side, price, bust));
    }
}

#[kani::proof]
fn the_split_hands_out_exactly_the_pool() {
    let (profit, fees): (u64, u64) = (kani::any(), kani::any());
    kani::assume(profit <= AMOUNT && fees <= AMOUNT);
    let s = split(profit, fees, 200_000, &DistParams::DEFAULT);
    assert!(s.from_cash <= profit);
    assert!(s.burn as u128 + s.staker as u128 + s.xbust as u128 + s.dev as u128 == s.pool as u128);
    assert!(s.pool as u128 == s.from_cash as u128 + fees as u128);
}

#[kani::proof]
fn utilization_stays_within_a_million() {
    let (long, short, cap): (u64, u64, u64) = (kani::any(), kani::any(), kani::any());
    let u = utilization(long, short, cap);
    assert!(u as u128 <= PPM);
    if long == 0 && short == 0 {
        assert!(u == 0);
    }
}

#[kani::proof]
fn the_cap_never_falls_below_its_floor() {
    // the net vault is cash minus debt, two u64
    let (cash, debt, ratio, floor): (u64, u64, u32, u64) = (kani::any(), kani::any(), kani::any(), kani::any());
    kani::assume(ratio as u128 <= PPM);
    let net = cash as i128 - debt as i128;
    let cap = oi_cap(net, ratio, floor);
    assert!(cap >= floor);
    if net <= 0 {
        assert!(cap == floor);
    }
}

#[kani::proof]
fn a_fee_is_never_more_than_the_amount() {
    let amount: u64 = kani::any();
    kani::assume(amount <= AMOUNT);
    assert!(fee_of(amount, Params::DEFAULT.win_fee_ppm) <= amount);
}

#[kani::proof]
fn a_ticket_is_paid_in_order_and_never_beyond_its_amount() {
    let t = Ticket { amount: kani::any(), start: kani::any(), ..ticket() };
    let (earlier, later): (u64, u64) = (kani::any(), kani::any());
    kani::assume(earlier <= later);
    assert!(t.paid(later) <= t.amount);
    assert!(t.paid(earlier) <= t.paid(later));
    if later <= t.start {
        assert!(t.paid(later) == 0);
    }
}

fn ticket() -> Ticket {
    Ticket { owner: Default::default(), id: 0, start: 0, amount: 0, claimed: 0, rent_payer: Default::default(), bump: 0 }
}
