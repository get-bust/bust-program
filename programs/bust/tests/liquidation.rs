//! Which prices may liquidate a position, and how the market's clock keeps fills and liquidations on the Pyth timeline

mod common;

use common::*;

const PYTH: Pubkey = pyth_solana_receiver_sdk::ID;

/// A 100× long at 100: bust at 99.05
fn long_100x(env: &mut Env) -> (Pubkey, i64) {
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let pos = env.open(&t.kp.pubkey(), &t.kp, Side::Long, usdc(10.0), 100).unwrap();
    let p: bust::Position = env.account(&pos);
    (pos, p.fill_publish_time)
}

#[test]
fn a_price_from_before_the_fill_cannot_liquidate() {
    let mut env = Env::new();
    let (pos, filled) = long_100x(&mut env);
    // Still inside the 5 s age limit, past the bust price, but published before the fill
    let price = env.price;
    env.write_price(price, FEED, 98_0000_0000, -8, 10_000, filled - 1, PYTH);
    expect_err(env.liquidate(&pos), "PriceTooOld");
    // The same price after the fill does liquidate
    env.write_price(price, FEED, 98_0000_0000, -8, 10_000, filled, PYTH);
    env.liquidate(&pos).unwrap();
}

#[test]
fn a_liquidation_cannot_go_back_in_time() {
    let mut env = Env::new();
    let (pos, filled) = long_100x(&mut env);
    env.set_time(filled + 10);
    // A keeper records the price at +10: still above the bust price
    env.set_price(99.5);
    let (market, price) = (env.market, env.price);
    env.observe_price(market, price).unwrap();
    // A wick at +8 was past bust; it is newer than the fill but older than what the market has seen
    env.write_price(price, FEED, 98_0000_0000, -8, 10_000, filled + 8, PYTH);
    expect_err(env.liquidate(&pos), "PriceTooOld");
    let m: bust::Market = env.account(&market);
    assert_eq!(m.last_publish_time, filled + 10);
}

#[test]
fn an_uncertain_price_does_not_liquidate() {
    let mut env = Env::new();
    let (pos, _) = long_100x(&mut env);
    // 99.00 is past the 99.05 bust price, but ±0.10 reaches back above it
    let now = env.now;
    env.set_price_full(99.0, 0.1, now);
    expect_err(env.liquidate(&pos), "NotLiquidatable");
    env.set_price_full(99.0, 0.01, now);
    env.liquidate(&pos).unwrap();
}

#[test]
fn a_lagging_chain_clock_does_not_give_away_the_fill_price() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();
    // The chain clock is 10 s behind Pyth: a keeper has already recorded a price published at now + 10
    let (market, price, now) = (env.market, env.price, env.now);
    env.write_price(price, FEED, 100_0000_0000, -8, 10_000, now + 10, PYTH);
    env.observe_price(market, price).unwrap();

    let order = env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    let o: bust::OrderRequest = env.account(&order);
    assert_eq!(o.fill_at, now + 10 + FILL_DELAY, "the fill time follows Pyth, not the lagging clock");
    // What the clock alone would have made the fill price is one the user could already see
    env.write_price_after(price, 100_0000_0000, now + FILL_DELAY, now + FILL_DELAY - 1, PYTH);
    expect_err(env.execute_open(&order).map(|_| ()), "PriceBeforeRequest");
    env.write_price_after(price, 100_0000_0000, now + 10 + FILL_DELAY, now + 10 + FILL_DELAY - 1, PYTH);
    env.execute_open(&order).unwrap();
}

#[test]
fn fills_and_liquidations_move_the_market_clock_forward_only() {
    let mut env = Env::new();
    let (pos, filled) = long_100x(&mut env);
    let market = env.market;
    let m: bust::Market = env.account(&market);
    assert_eq!(m.last_publish_time, filled);
    // An older price observed later changes nothing
    let price = env.price;
    env.write_price(price, FEED, 100_0000_0000, -8, 10_000, filled - 3, PYTH);
    env.observe_price(market, price).unwrap();
    assert_eq!(env.account::<bust::Market>(&market).last_publish_time, filled);
    env.set_time(filled + 4);
    env.set_price(98.0);
    env.liquidate(&pos).unwrap();
    assert_eq!(env.account::<bust::Market>(&market).last_publish_time, filled + 4);
}

#[test]
fn observing_needs_a_verified_fresh_price() {
    let mut env = Env::new();
    let market = env.market;
    let (price, now) = (env.price, env.now);
    env.write_price(price, FEED, 100_0000_0000, -8, 10_000, now - 60, PYTH);
    assert!(env.observe_price(market, price).is_err());
    let fake = Pubkey::new_unique();
    env.write_price(fake, FEED, 100_0000_0000, -8, 10_000, now + 100, Pubkey::new_unique());
    expect_err(env.observe_price(market, fake), "AccountOwnedByWrongProgram");
    assert_eq!(env.account::<bust::Market>(&market).last_publish_time, 0);
}
