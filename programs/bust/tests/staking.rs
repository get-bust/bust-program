mod common;

use common::*;

fn bust_units(x: u64) -> u64 {
    x * 1_000_000
}

/// A losing close plus the epoch distribution; returns the staker share to be streamed
fn fund_stakers(env: &mut Env) -> u64 {
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    let pos = env.open(&o, &t.kp, Side::Long, usdc(100.0), 10).unwrap();
    env.close_at(&pos, &t.kp, 95.0).unwrap();
    env.next_epoch();
    env.distribute().unwrap();
    let staker = env.dist_state().staker_usdc;
    assert!(staker > 0);
    staker
}

fn claimed(env: &Env, st: &Staker) -> u64 {
    env.token_balance(&st.usdc)
}

#[test]
fn stakers_earn_pro_rata_over_the_epoch() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let a = env.staker(bust_units(100));
    let b = env.staker(bust_units(300));
    env.stake(&a, bust_units(100)).unwrap();
    env.stake(&b, bust_units(300)).unwrap();

    let pot = fund_stakers(&mut env);
    env.next_epoch(); // the stream completes
    env.claim(&a).unwrap();
    env.claim(&b).unwrap();
    let (ga, gb) = (claimed(&env, &a), claimed(&env, &b));
    assert!(ga + gb <= pot, "never pays more than was streamed");
    assert!(pot - (ga + gb) <= 2, "rounding loses at most a few base units");
    assert!((gb as i64 - 3 * ga as i64).abs() <= 3, "1:3 stake, 1:3 reward ({ga} / {gb})");
    assert_eq!(env.dist_state().staker_usdc, pot - ga - gb);
    env.assert_invariant();

    // a second claim finds nothing
    expect_err(env.claim(&a), "NothingToWithdraw");
}

#[test]
fn late_staker_earns_only_while_staked() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let a = env.staker(bust_units(100));
    let b = env.staker(bust_units(100));
    env.stake(&a, bust_units(100)).unwrap();
    let pot = fund_stakers(&mut env);
    let start = env.now;

    // Joining half an epoch after the distribution shares only what is left of the stream
    env.set_time(start + DistParams::DEFAULT.epoch_sec / 2);
    env.stake(&b, bust_units(100)).unwrap();
    env.set_time(start + DistParams::DEFAULT.epoch_sec);
    env.claim(&a).unwrap();
    env.claim(&b).unwrap();
    let (ga, gb) = (claimed(&env, &a), claimed(&env, &b));
    assert!((ga as i64 - (pot * 3 / 4) as i64).abs() <= 3, "early staker ~75% ({ga} / {pot})");
    assert!((gb as i64 - (pot / 4) as i64).abs() <= 3, "late staker ~25% ({gb} / {pot})");
    env.assert_invariant();
}

#[test]
fn staking_after_distribution_without_stakers_earns_nothing() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    let pos = env.open(&o, &t.kp, Side::Long, usdc(100.0), 10).unwrap();
    env.close_at(&pos, &t.kp, 95.0).unwrap();
    env.next_epoch();
    env.distribute().unwrap();
    assert_eq!(env.dist_state().staker_usdc, 0, "with no stakers the share is burned");
    let a = env.staker(bust_units(100));
    env.stake(&a, bust_units(100)).unwrap();
    env.next_epoch();
    expect_err(env.claim(&a), "NothingToWithdraw");
}

#[test]
fn unstake_stops_rewards_and_waits() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let a = env.staker(bust_units(100));
    env.stake(&a, bust_units(100)).unwrap();
    let pot = fund_stakers(&mut env);
    let start = env.now;

    env.set_time(start + DistParams::DEFAULT.epoch_sec / 2);
    expect_err(env.request_unstake(&a, bust_units(101)), "InsufficientBalance");
    env.request_unstake(&a, bust_units(100)).unwrap();
    expect_err(env.withdraw_unstaked(&a), "UnstakeNotReady");

    env.set_time(start + DistParams::DEFAULT.epoch_sec);
    env.claim(&a).unwrap();
    let ga = claimed(&env, &a);
    assert!((ga as i64 - (pot / 2) as i64).abs() <= 2, "rewards stop at the unstake ({ga} / {pot})");

    env.set_time(start + DistParams::DEFAULT.epoch_sec / 2 + UNSTAKE_SEC);
    env.withdraw_unstaked(&a).unwrap();
    assert_eq!(env.token_balance(&a.tokens), bust_units(100));
    assert_eq!(env.account::<bust::StakePool>(&stake_pool()).total_staked, 0);
    expect_err(env.withdraw_unstaked(&a), "NothingToWithdraw");
    env.assert_invariant();
}

#[test]
fn nobody_claims_someone_elses_rewards() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let victim = env.staker(bust_units(100));
    env.stake(&victim, bust_units(100)).unwrap();
    fund_stakers(&mut env);
    env.next_epoch();

    // The attacker cannot point at the victim's stake account with their own signature
    let attacker = env.staker(0);
    let akp = attacker.kp.insecure_clone();
    expect_err(env.claim_as(&akp, stake_pda(&victim.kp.pubkey()), attacker.usdc), "ConstraintSeeds");
    // Nor send the victim's rewards to their own USDC account (it must belong to the owner)
    let vkp = victim.kp.insecure_clone();
    expect_err(env.claim_as(&vkp, stake_pda(&victim.kp.pubkey()), attacker.usdc), "ConstraintTokenOwner");
    // Their own empty account has nothing to claim
    expect_err(env.claim(&attacker), "NothingToWithdraw");
    env.claim(&victim).unwrap();
    env.assert_invariant();
}
