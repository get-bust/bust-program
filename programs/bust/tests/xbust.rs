mod common;

use common::*;

pub fn bust_units(x: u64) -> u64 {
    x * 1_000_000
}

/// A trader who opens 10× long at 100 and closes at `exit`
fn trade_and_close(env: &mut Env, margin: f64, exit: f64) -> Trader {
    env.set_price(100.0);
    let t = env.trader(usdc(1_000.0));
    let o = t.kp.pubkey();
    let pos = env.open(&o, &t.kp, Side::Long, usdc(margin), 10).unwrap();
    env.close_at(&pos, &t.kp, exit).unwrap();
    t
}

fn epoch_loss(env: &Env, owner: &Pubkey, epoch: u64) -> bust::EpochLoss {
    env.account(&epoch_loss_pda(owner, epoch))
}

/// One epoch of xBUST: the trader loses, `pool` $BUST enters the treasury
fn xbust_holder(env: &mut Env, pool: u64) -> (Trader, Pubkey) {
    env.seed(usdc(25_000.0));
    let t = trade_and_close(env, 100.0, 95.0);
    let o = t.kp.pubkey();
    env.next_epoch();
    env.distribute().unwrap();
    let epoch = env.dist_state().epoch - 1;
    env.fund_epoch(epoch, pool).unwrap();
    let x = env.xbust_account(&o);
    env.claim_xbust(&o, epoch, x).unwrap();
    (t, x)
}

#[test]
fn losses_are_netted_per_epoch() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = trade_and_close(&mut env, 100.0, 95.0); // −50
    let o = t.kp.pubkey();
    assert_eq!(epoch_loss(&env, &o, 0).net, -(usdc(50.0) as i64));
    assert_eq!(env.dist_state().epoch_loss, usdc(50.0));

    // A win in the same epoch reduces the loss
    let pos = env.open(&o, &t.kp, Side::Long, usdc(100.0), 10).unwrap();
    env.close_at(&pos, &t.kp, 102.0).unwrap();
    let net = epoch_loss(&env, &o, 0).net;
    assert!(net > -(usdc(50.0) as i64));
    assert_eq!(env.dist_state().epoch_loss, (-net).max(0) as u64);

    // A liquidation counts too
    let other = env.trader(usdc(1_000.0));
    let ok = other.kp.pubkey();
    let pos = env.open(&ok, &other.kp, Side::Long, usdc(10.0), 100).unwrap();
    env.set_price(98.0);
    env.liquidate(&pos).unwrap();
    assert_eq!(epoch_loss(&env, &ok, 0).net, -(usdc(10.0) as i64));
    assert_eq!(env.dist_state().epoch_loss, (-net).max(0) as u64 + usdc(10.0));
}

#[test]
fn xbust_is_shared_by_net_loss() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let a = trade_and_close(&mut env, 100.0, 95.0); // −50
    let b = trade_and_close(&mut env, 300.0, 95.0); // −150 (10×, −5%)
    let (ka, kb) = (a.kp.pubkey(), b.kp.pubkey());
    env.next_epoch();
    env.distribute().unwrap();
    let rec: bust::EpochRecord = env.account(&epoch_pda(0));
    assert_eq!(rec.total_loss, usdc(200.0));
    assert_eq!(env.dist_state().epoch_loss, 0, "the new epoch starts from zero");

    let (xa, xb) = (env.xbust_account(&ka), env.xbust_account(&kb));
    expect_err(env.claim_xbust(&ka, 0, xa), "EpochNotFunded");
    let pool = bust_units(1_000);
    env.fund_epoch(0, pool).unwrap();
    expect_err(env.fund_epoch(0, pool), "EpochFunded");

    let keeper_before = env.svm.get_balance(&env.keeper.pubkey()).unwrap();
    env.claim_xbust(&ka, 0, xa).unwrap();
    env.claim_xbust(&kb, 0, xb).unwrap();
    assert_eq!(env.token_balance(&xa), pool / 4);
    assert_eq!(env.token_balance(&xb), pool * 3 / 4);
    assert!(env.svm.get_account(&epoch_loss_pda(&ka, 0)).is_none_or(|a| a.lamports == 0), "the record closes");
    assert!(env.svm.get_balance(&env.keeper.pubkey()).unwrap() > keeper_before, "the rent goes back to the keeper");
    let rec: bust::EpochRecord = env.account(&epoch_pda(0));
    assert_eq!(rec.claimed, pool);
}

#[test]
fn only_buyback_authority_funds_and_claims_go_to_the_loser() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let a = trade_and_close(&mut env, 100.0, 95.0);
    let ka = a.kp.pubkey();
    env.next_epoch();
    env.distribute().unwrap();

    // An attacker cannot fund the epoch with one unit and lock the pool
    let attacker = env.caller();
    expect_err(env.fund_epoch_as(&attacker, 0, 1), "NotBuybackAuthority");
    env.fund_epoch(0, bust_units(100)).unwrap();

    // xBUST is only minted to the loser's account
    let theirs = env.xbust_account(&attacker.pubkey());
    expect_err(env.claim_xbust(&ka, 0, theirs), "ConstraintTokenOwner");
    let mine = env.xbust_account(&ka);
    env.claim_xbust(&ka, 0, mine).unwrap();
    assert_eq!(env.token_balance(&mine), bust_units(100));
    // The same record cannot be claimed twice (it is closed)
    expect_err(env.claim_xbust_closed(&ka, 0, mine), "AccountNotInitialized");
}

#[test]
fn locked_xbust_unlocks_daily_over_30_days() {
    let mut env = Env::new();
    let pool = bust_units(300);
    let (t, x) = xbust_holder(&mut env, pool);
    let mint = env.bust_mint;
    let out = env.token_account_of(&mint, &t.kp.pubkey(), 0);
    let start = env.now;
    env.lock_xbust(&t.kp, x, pool).unwrap();
    assert_eq!(env.token_balance(&x), 0, "locked xBUST is burned");
    expect_err(env.convert(&t.kp, out), "NothingToWithdraw");

    // Day 10: a third, at rate 1
    env.set_time(start + 10 * 86_400);
    env.convert(&t.kp, out).unwrap();
    assert_eq!(env.token_balance(&out), pool / 3);
    // After day 30: everything
    env.set_time(start + 40 * 86_400);
    env.convert(&t.kp, out).unwrap();
    assert_eq!(env.token_balance(&out), pool);
    assert_eq!(env.token_balance(&treasury_tokens()), 0);
    assert_eq!(env.account::<bust::Treasury>(&treasury()).total_locked, 0);
}

#[test]
fn burned_xbust_raises_the_rate_for_those_who_wait() {
    let mut env = Env::new();
    let pool = bust_units(200);
    let (a, xa) = xbust_holder(&mut env, pool); // a single loser: 200 xBUST
                                                // Half goes to someone else and is burned, as if sold into the wall
    let b = env.caller();
    let xb = env.xbust_account(&b.pubkey());
    let ixs = [
        spl_token::instruction::transfer(&spl_token::ID, &xa, &xb, &a.kp.pubkey(), &[], pool / 2).unwrap(),
        spl_token::instruction::burn(&spl_token::ID, &xb, &xbust_mint(), &b.pubkey(), &[], pool / 2).unwrap(),
    ];
    let akp = a.kp.insecure_clone();
    env.send(&ixs, &[&akp, &b]).unwrap();

    // The remaining 100 xBUST turn into all 200 $BUST in the treasury (rate 2)
    let mint = env.bust_mint;
    let out = env.token_account_of(&mint, &a.kp.pubkey(), 0);
    env.lock_xbust(&a.kp, xa, pool / 2).unwrap();
    env.set_time(env.now + VEST_SEC);
    env.convert(&a.kp, out).unwrap();
    assert_eq!(env.token_balance(&out), pool);
}

#[test]
fn adding_to_a_lock_keeps_what_already_unlocked() {
    let mut env = Env::new();
    let pool = bust_units(300);
    let (t, x) = xbust_holder(&mut env, pool);
    let mint = env.bust_mint;
    let out = env.token_account_of(&mint, &t.kp.pubkey(), 0);
    let start = env.now;
    env.lock_xbust(&t.kp, x, bust_units(150)).unwrap();
    env.set_time(start + 10 * 86_400); // 50 unlocked
    env.lock_xbust(&t.kp, x, bust_units(150)).unwrap(); // the remaining 100 + the new 150, clock restarted
    let v: bust::Vest = env.account(&vest_pda(&t.kp.pubkey()));
    assert_eq!((v.amount, v.carried, v.converted), (bust_units(250), bust_units(50), 0));
    env.convert(&t.kp, out).unwrap();
    assert_eq!(env.token_balance(&out), bust_units(50), "the unlocked part is kept");
    env.set_time(start + 10 * 86_400 + VEST_SEC);
    env.convert(&t.kp, out).unwrap();
    assert_eq!(env.token_balance(&out), pool);

    // Nobody else can convert this lock (the vest is found from the signer)
    let attacker = env.caller();
    let theirs = env.token_account_of(&mint, &attacker.pubkey(), 0);
    expect_err(env.convert(&attacker, theirs), "AccountNotInitialized");
}
