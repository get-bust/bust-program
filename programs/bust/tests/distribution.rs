mod common;

use common::*;

/// A losing close: the vault takes the loss, the distribution takes the fee
fn losing_trade(env: &mut Env) {
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    let pos = env.open(&o, &t.kp, Side::Long, usdc(100.0), 10).unwrap();
    env.close_at(&pos, &t.kp, 95.0).unwrap(); // 1000 notional, −5%: 50 loss, 1 fee
}

#[test]
fn distribute_splits_fees_and_profit() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    losing_trade(&mut env);
    let v = env.vault_state();
    let d = env.dist_state();
    let profit = v.cash - d.cash_mark;
    assert_eq!(profit, usdc(49.0), "seeded capital is not profit");

    expect_err(env.distribute(), "EpochNotOver");
    env.next_epoch();
    env.distribute().unwrap();

    // No utilisation recorded: the vault keeps the minimum share (20%). No stakers: their share is burned
    let want = math::split(profit, v.fees_accrued, DistParams::DEFAULT.keep_min_ppm, &DistParams::DEFAULT);
    let v2 = env.vault_state();
    let d2 = env.dist_state();
    assert_eq!(v.cash - v2.cash, want.from_cash);
    assert_eq!(v2.fees_accrued, 0);
    assert_eq!((d2.burn_usdc, d2.staker_usdc, d2.xbust_usdc, d2.dev_usdc), (want.burn + want.staker, 0, want.xbust, want.dev));
    assert_eq!(d2.buckets(), want.pool);
    assert_eq!(d2.cash_mark, v2.cash);
    assert_eq!(d2.epoch, 1);
    env.assert_invariant();

    // Not twice in the same epoch
    expect_err(env.distribute(), "EpochNotOver");
}

#[test]
fn distribute_waits_for_empty_queue() {
    let mut env = Env::new();
    env.seed(usdc(100.0));
    let winner = env.trader(usdc(1_000.0));
    let w = winner.kp.pubkey();
    let pos = env.open(&w, &winner.kp, Side::Long, usdc(30.0), 2).unwrap();
    env.close_at(&pos, &winner.kp, 400.0).unwrap(); // the vault falls short, the queue fills
    assert!(env.vault_state().queue_total > 0);
    env.next_epoch();
    expect_err(env.distribute(), "QueueNotEmpty");

    // Once the queue is paid the distribution runs; seeded money is not profit, so the pool is empty
    env.seed(usdc(1_000.0));
    env.harvest_and_claim(&[w]);
    env.distribute().unwrap();
    assert_eq!(env.dist_state().buckets(), 0);
    env.assert_invariant();
}

#[test]
fn high_water_mark_blocks_profit_until_recovered() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let winner = env.trader(usdc(1_000.0));
    let w = winner.kp.pubkey();
    let pos = env.open(&w, &winner.kp, Side::Long, usdc(100.0), 10).unwrap();
    env.close_at(&pos, &winner.kp, 110.0).unwrap(); // the vault pays and drops below the mark
    let v = env.vault_state();
    let mark = env.dist_state().cash_mark;
    assert!(v.cash < mark);

    env.next_epoch();
    env.distribute().unwrap();
    // Only the win fee is distributed; no cash leaves the vault and the mark stays
    assert_eq!(env.vault_state().cash, v.cash);
    assert_eq!(env.dist_state().cash_mark, mark);
    assert_eq!(env.dist_state().buckets(), v.fees_accrued);
    env.assert_invariant();
}

#[test]
fn sustained_utilization_raises_vault_share() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(2_000.0));
    let o = t.kp.pubkey();
    // 1000 × 10 = 10,000 notional against a 15,000 cap: ~66.7% utilisation, held for two days
    let pos = env.open(&o, &t.kp, Side::Long, usdc(1_000.0), 10).unwrap();
    let market = env.market;
    env.record_utilization(&[market]).unwrap();
    env.set_time(env.now + 2 * bust::DAY_SEC);
    env.set_price(100.0);
    env.close_at(&pos, &t.kp, 99.0).unwrap(); // −1%: a 100 loss
    env.record_utilization(&[market]).unwrap(); // back to zero

    // The day fully inside the two days averages ~66.7%
    let today = env.now.div_euclid(bust::DAY_SEC);
    let util = env.dist_state().peak_util(today);
    assert!(util > 660_000 && util < 670_000, "util {util}");

    let v = env.vault_state();
    let profit = v.cash - env.dist_state().cash_mark;
    env.next_epoch();
    env.distribute().unwrap();
    let want = math::split(profit, v.fees_accrued, util, &DistParams::DEFAULT);
    assert_eq!(v.cash - env.vault_state().cash, want.from_cash, "the vault keeps a share equal to utilisation");
    env.assert_invariant();

    // Days older than 7 do not count
    for _ in 0..8 {
        env.next_epoch();
    }
    env.record_utilization(&[market]).unwrap();
    let today = env.now.div_euclid(bust::DAY_SEC);
    assert_eq!(env.dist_state().peak_util(today), 0);
}

#[test]
fn a_brief_spike_does_not_move_the_vault_share() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let attacker = env.trader(usdc(2_000.0));
    let o = attacker.kp.pubkey();
    let market = env.market;
    // The whole cap for a couple of seconds, sampled fifty times while it lasts
    let pos = env.open(&o, &attacker.kp, Side::Long, usdc(1_500.0), 10).unwrap();
    for _ in 0..50 {
        env.record_utilization(&[market]).unwrap();
    }
    env.close_at(&pos, &attacker.kp, 99.0).unwrap();
    env.record_utilization(&[market]).unwrap();

    let today = env.now.div_euclid(bust::DAY_SEC);
    let util = env.dist_state().peak_util(today);
    assert!(util < 100, "a two-second spike weighs two seconds of the day: {util} ppm");

    // The distribution keeps only the minimum share
    let v = env.vault_state();
    let profit = v.cash - env.dist_state().cash_mark;
    env.next_epoch();
    env.distribute().unwrap();
    let want = math::split(profit, v.fees_accrued, DistParams::DEFAULT.keep_min_ppm, &DistParams::DEFAULT);
    assert_eq!(v.cash - env.vault_state().cash, want.from_cash);
    env.assert_invariant();
}

#[test]
fn record_utilization_rejects_fake_market() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    // An account owned by another program cannot pose as a market
    let fake = env.buyback_tokens;
    expect_err(env.record_utilization(&[fake]), "AccountOwnedByWrongProgram");
    // Nor can a program account of another type
    expect_err(env.record_utilization(&[vault()]), "AccountDiscriminatorMismatch");
}

#[test]
fn buckets_go_only_to_registered_wallets() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    losing_trade(&mut env);
    env.next_epoch();
    env.distribute().unwrap();
    let d = env.dist_state();
    let attacker = env.caller();
    let stranger = env.token_account(&attacker.pubkey(), 0);

    expect_err(env.withdraw_bucket(Bucket::Burn, stranger), "InvalidDestination");
    expect_err(env.withdraw_bucket(Bucket::Dev, env.buyback_tokens), "InvalidDestination");
    // The staker share never leaves through here
    expect_err(env.withdraw_bucket(Bucket::Staker, env.buyback_tokens), "InvalidDestination");

    let (bb, dev) = (env.buyback_tokens, env.dev_tokens);
    env.withdraw_bucket(Bucket::Burn, bb).unwrap();
    env.withdraw_bucket(Bucket::Xbust, bb).unwrap();
    env.withdraw_bucket(Bucket::Dev, dev).unwrap();
    assert_eq!(env.token_balance(&bb), d.burn_usdc + d.xbust_usdc);
    assert_eq!(env.token_balance(&dev), d.dev_usdc);
    let d2 = env.dist_state();
    assert_eq!((d2.burn_usdc, d2.xbust_usdc, d2.dev_usdc, d2.staker_usdc), (0, 0, 0, d.staker_usdc));
    env.assert_invariant();

    // A second withdrawal finds nothing
    expect_err(env.withdraw_bucket(Bucket::Dev, dev), "NothingToWithdraw");
}

#[test]
fn only_admin_updates_distributor() {
    let mut env = Env::new();
    let attacker = env.caller();
    let mine = env.token_account(&attacker.pubkey(), 0);
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::UpdateDistributor { params: DistParams::DEFAULT }.data(),
        bust::accounts::UpdateDistributor {
            admin: attacker.pubkey(),
            config: config(),
            distributor: distributor(),
            buyback_tokens: mine,
            dev_tokens: mine,
            usdc_mint: env.mint,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&attacker]), "ConstraintHasOne");

    // The shares must add up to 1e6
    let mut bad = DistParams::DEFAULT;
    bad.dev_ppm += 1;
    let admin = env.admin.insecure_clone();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::UpdateDistributor { params: bad }.data(),
        bust::accounts::UpdateDistributor {
            admin: admin.pubkey(),
            config: config(),
            distributor: distributor(),
            buyback_tokens: env.buyback_tokens,
            dev_tokens: env.dev_tokens,
            usdc_mint: env.mint,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&admin]), "InvalidConfig");
}

#[test]
fn utilization_needs_every_market_in_order() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let admin = env.admin.insecure_clone();
    env.add_market(&admin, 1, [9u8; 32], ETH_MULT).unwrap();
    let t = env.trader(usdc(2_000.0));
    let o = t.kp.pubkey();
    env.open(&o, &t.kp, Side::Long, usdc(1_000.0), 10).unwrap(); // market 0 is the busy one
    let (m0, m1) = (market_pda(0), market_pda(1));

    // Leaving out the busy market would lower the sample, and so the vault's share
    expect_err(env.record_utilization(&[m1]), "MarketsMismatch");
    expect_err(env.record_utilization(&[m1, m0]), "MarketsMismatch");
    expect_err(env.record_utilization(&[m0, m0]), "MarketsMismatch");
    expect_err(env.record_utilization(&[m0, m1, m1]), "MarketsMismatch");
    env.record_utilization(&[m0, m1]).unwrap();
    assert!(env.dist_state().util_last > 600_000);
}

// ---------- profit that open positions are sitting on

#[test]
fn profit_open_positions_are_up_is_not_distributed() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    // A loss puts 49 USDC of profit in the vault
    let loser = env.trader(usdc(1_000.0));
    let l = loser.kp.pubkey();
    let pos = env.open(&l, &loser.kp, Side::Long, usdc(100.0), 10).unwrap();
    env.close_at(&pos, &loser.kp, 95.0).unwrap();
    // Meanwhile a long opened at 95 is up 31.58 USDC at 98 (1,000 × 3/95, before haircuts)
    let winner = env.trader(usdc(1_000.0));
    let w = winner.kp.pubkey();
    env.open(&w, &winner.kp, Side::Long, usdc(100.0), 10).unwrap();
    env.next_epoch();
    env.set_price(98.0);

    let v = env.vault_state();
    let above_mark = v.cash - env.dist_state().cash_mark;
    let m: bust::Market = env.account(&env.market);
    let up = math::side_pnl(Side::Long, 98_0000_0000, m.oi_long, m.long_units);
    assert!(up > usdc(30.0) as i128 && up < usdc(32.0) as i128, "{up}");
    env.distribute().unwrap();
    // Only the profit beyond what the open long is up gets split
    let want = math::split(above_mark - up as u64, v.fees_accrued, DistParams::DEFAULT.keep_min_ppm, &DistParams::DEFAULT);
    assert_eq!(v.cash - env.vault_state().cash, want.from_cash);
    env.assert_invariant();
}

#[test]
fn a_side_that_is_down_does_not_count_against_one_that_is_up() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let loser = env.trader(usdc(1_000.0));
    let l = loser.kp.pubkey();
    let pos = env.open(&l, &loser.kp, Side::Long, usdc(100.0), 10).unwrap();
    env.close_at(&pos, &loser.kp, 95.0).unwrap();
    // A long and a short of the same size at 95: at 96 the long is up as much as the short is down
    let a = env.trader(usdc(1_000.0));
    let ka = a.kp.pubkey();
    env.open(&ka, &a.kp, Side::Long, usdc(10.0), 10).unwrap();
    env.open(&ka, &a.kp, Side::Short, usdc(10.0), 10).unwrap();
    env.next_epoch();
    env.set_price(96.0);
    let m: bust::Market = env.account(&env.market);
    let long_up = math::side_pnl(Side::Long, 96_0000_0000, m.oi_long, m.long_units);
    assert!(math::side_pnl(Side::Short, 96_0000_0000, m.oi_short, m.short_units) < 0);

    let v = env.vault_state();
    let above_mark = v.cash - env.dist_state().cash_mark;
    env.distribute().unwrap();
    let want = math::split(above_mark - long_up as u64, v.fees_accrued, DistParams::DEFAULT.keep_min_ppm, &DistParams::DEFAULT);
    assert_eq!(v.cash - env.vault_state().cash, want.from_cash);
}

#[test]
fn distribution_needs_every_market_and_a_fresh_price() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    env.next_epoch();
    let price = env.price;
    expect_err(env.distribute_with(&[]), "MarketsMismatch");
    // A price older than the liquidation age is refused
    let now = env.now;
    env.set_price_full(100.0, 0.01, now - 60);
    assert!(env.distribute_with(&[price]).is_err());
    env.set_price(100.0);
    env.distribute_with(&[price]).unwrap();
}
