//! Open interest caps: the floors that keep an empty or indebted vault trading, and the cap over all markets

mod common;

use common::*;

const FLOOR: f64 = 1_000.0;

fn floors(market: f64, total: f64) -> Params {
    Params { oi_floor: usdc(market), oi_total_floor: usdc(total), ..Params::DEFAULT }
}

#[test]
fn an_empty_vault_trades_up_to_the_floor() {
    let mut env = Env::with_params(floors(FLOOR, 2.0 * FLOOR));
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    // No capital at all: positions open up to the floor and no further
    expect_err(env.open(&o, &t.kp, Side::Long, usdc(101.0), 10).map(|_| ()), "OpenInterestCap");
    let pos = env.open(&o, &t.kp, Side::Long, usdc(100.0), 10).unwrap();
    // A loss is how the vault starts to fill
    env.close_at(&pos, &t.kp, 99.0).unwrap();
    assert!(env.vault_state().cash > 0);
    env.assert_invariant();
}

#[test]
fn a_vault_in_debt_keeps_trading_at_the_floor() {
    let mut env = Env::with_params(floors(FLOOR, 2.0 * FLOOR));
    env.seed(usdc(100.0));
    let winner = env.trader(usdc(1_000.0));
    let w = winner.kp.pubkey();
    let pos = env.open(&w, &winner.kp, Side::Long, usdc(30.0), 2).unwrap();
    env.close_at(&pos, &winner.kp, 400.0).unwrap();
    assert!(env.vault_state().queue_total > env.vault_state().cash, "the vault owes more than it holds");

    // Trading goes on, within the floor, and a loss pays the queue down
    env.set_price(100.0);
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    expect_err(env.open(&o, &t.kp, Side::Long, usdc(101.0), 10).map(|_| ()), "OpenInterestCap");
    let pos = env.open(&o, &t.kp, Side::Long, usdc(100.0), 10).unwrap();
    let debt = env.vault_state().queue_total;
    env.close_at(&pos, &t.kp, 95.0).unwrap();
    env.harvest().unwrap();
    assert!(env.vault_state().queue_total < debt);
    env.assert_invariant();
}

#[test]
fn the_vault_cap_takes_over_once_it_is_larger_than_the_floor() {
    let mut env = Env::with_params(floors(FLOOR, 2.0 * FLOOR));
    env.seed(usdc(25_000.0)); // 0.6 × 25,000 = 15,000 per market, above the 1,000 floor
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    env.open(&o, &t.kp, Side::Long, usdc(150.0), 100).unwrap();
    expect_err(env.open(&o, &t.kp, Side::Long, usdc(1.0), 10).map(|_| ()), "OpenInterestCap");
}

#[test]
fn all_markets_on_one_side_share_one_cap() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0)); // 15,000 per market, and 15,000 over all markets on a side
    let admin = env.admin.insecure_clone();
    env.add_market(&admin, 1, [9u8; 32], ETH_MULT).unwrap();
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    env.open(&o, &t.kp, Side::Long, usdc(100.0), 100).unwrap(); // 10,000 long on market 0

    // Market 1 has room of its own, but longs over both markets would pass the total
    env.market = market_pda(1);
    env.write_price(env.price, [9u8; 32], 100_0000_0000, -8, 10_000, env.now, pyth_solana_receiver_sdk::ID);
    let order = env.request_open(&o, &t.kp, Side::Long, usdc(60.0), 100).unwrap();
    env.set_time(env.now + FILL_DELAY);
    let (price, now) = (env.price, env.now);
    env.write_price(price, [9u8; 32], 100_0000_0000, -8, 10_000, now, pyth_solana_receiver_sdk::ID);
    let market = env.market;
    expect_err(env.execute_open_with(&order, price, market).map(|_| ()), "OpenInterestCap");
    let v = env.vault_state();
    assert_eq!((v.oi_long_total, v.oi_short_total), (usdc(10_000.0), 0));

    // The other side has its own total
    let order = env.request_open(&o, &t.kp, Side::Short, usdc(140.0), 100).unwrap();
    env.set_time(env.now + FILL_DELAY);
    let now = env.now;
    env.write_price(price, [9u8; 32], 100_0000_0000, -8, 10_000, now, pyth_solana_receiver_sdk::ID);
    env.execute_open_with(&order, price, market).unwrap();
    assert_eq!(env.vault_state().oi_short_total, usdc(14_000.0));
    env.assert_invariant();
}

#[test]
fn closing_frees_the_total() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    let a = env.open(&o, &t.kp, Side::Long, usdc(100.0), 100).unwrap();
    let b = env.open(&o, &t.kp, Side::Long, usdc(10.0), 100).unwrap();
    env.close_at(&a, &t.kp, 100.0).unwrap();
    env.set_price(100.0);
    env.set_price(99.0);
    env.liquidate(&b).unwrap();
    assert_eq!(env.vault_state().oi_long_total, 0);
}

#[test]
fn past_the_debt_limit_the_floors_shrink_but_trading_goes_on() {
    let params = Params { oi_floor: usdc(1_000.0), oi_total_floor: usdc(2_000.0), max_debt: usdc(10.0), ..Params::DEFAULT };
    let mut env = Env::with_params(params);
    env.seed(usdc(100.0));
    let winner = env.trader(usdc(1_000.0));
    let w = winner.kp.pubkey();
    let pos = env.open(&w, &winner.kp, Side::Long, usdc(30.0), 2).unwrap();
    env.close_at(&pos, &winner.kp, 400.0).unwrap();
    let debt = env.vault_state().queue_total;
    assert!(debt > usdc(50.0), "~60 USDC queued, past the 10 USDC limit");

    // The floor is now 1,000 × 10 / debt (~160): smaller positions still open, the full floor does not
    let (floor, _) = params.floors(debt);
    assert!(floor > usdc(100.0) && floor < usdc(200.0), "floor {floor}");
    env.set_price(100.0);
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    expect_err(env.open(&o, &t.kp, Side::Long, usdc(100.0), 2).map(|_| ()), "OpenInterestCap");
    let small = env.open(&o, &t.kp, Side::Long, usdc(50.0), 2).unwrap();
    // Its loss pays the queue down, and the floor grows back with it
    env.close_at(&small, &t.kp, 90.0).unwrap();
    env.harvest().unwrap();
    assert!(env.vault_state().queue_total < debt);
    assert!(params.floors(env.vault_state().queue_total).0 > floor);

    // Once the debt is back under the limit, the floor is whole again
    env.seed(usdc(1_000.0));
    env.harvest_and_claim(&[w]);
    env.set_price(100.0);
    env.open(&o, &t.kp, Side::Long, usdc(400.0), 2).unwrap();
}
