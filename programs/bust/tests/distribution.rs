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
    env.harvest(&[w]).unwrap();
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
fn utilization_raises_vault_share() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(2_000.0));
    let o = t.kp.pubkey();
    // 1000 × 10 = 10,000 notional against a 15,000 cap: ~66.7% utilisation
    let pos = env.open(&o, &t.kp, Side::Long, usdc(1_000.0), 10).unwrap();
    let market = env.market;
    env.record_utilization(&[market]).unwrap();
    let today = env.now.div_euclid(bust::DAY_SEC);
    let util = env.dist_state().peak_util(today);
    assert!(util > 660_000 && util < 670_000, "util {util}");

    env.close_at(&pos, &t.kp, 99.0).unwrap(); // −1%: a 100 loss
    let v = env.vault_state();
    let profit = v.cash - env.dist_state().cash_mark;
    env.next_epoch();
    env.distribute().unwrap();
    let want = math::split(profit, v.fees_accrued, util, &DistParams::DEFAULT);
    assert_eq!(v.cash - env.vault_state().cash, want.from_cash, "the vault keeps a share equal to utilisation");
    env.assert_invariant();

    // Utilisation older than 7 days does not count
    for _ in 0..7 {
        env.next_epoch();
    }
    let today = env.now.div_euclid(bust::DAY_SEC);
    assert_eq!(env.dist_state().peak_util(today), 0);
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
