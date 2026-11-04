mod common;

use common::*;

#[test]
fn queue_when_vault_short_then_harvest() {
    let mut env = Env::new();
    env.seed(usdc(100.0)); // a small vault: cap 60
    let winner = env.trader(usdc(1_000.0));
    let loser = env.trader(usdc(1_000.0));
    let (w, l) = (winner.kp.pubkey(), loser.kp.pubkey());

    let wpos = env.open(&w, &winner.kp, Side::Long, usdc(30.0), 2).unwrap(); // 60 notional, the whole cap
    env.close_at(&wpos, &winner.kp, 400.0).unwrap(); // +300%: ~160 profit, 100 in the vault
    let ws = env.user_state(&w);
    let v = env.vault_state();
    assert_eq!(v.cash, 0, "the vault pays what it has");
    assert!(ws.queued > 0 && v.queue_total == ws.queued);
    assert_eq!(v.fees_accrued, 0, "no fee on a partial payout");
    env.assert_invariant();

    // With the queue outstanding the cap is zero: no new positions
    expect_err(env.open(&l, &loser.kp, Side::Long, usdc(10.0), 2).map(|_| ()), "OpenInterestCap");

    // Once cash comes in, the queue gets paid
    env.seed(usdc(1_000.0));
    let queued = ws.queued;
    env.harvest(&[w]).unwrap();
    let ws2 = env.user_state(&w);
    assert_eq!(ws2.queued, 0);
    assert_eq!(ws2.balance, ws.balance + queued);
    assert_eq!(env.vault_state().queue_total, 0);
    env.assert_invariant();

    // The vault is positive again: trading resumes
    env.open(&l, &loser.kp, Side::Long, usdc(10.0), 2).unwrap();
}

#[test]
fn harvest_rejects_wrong_user() {
    let mut env = Env::new();
    env.seed(usdc(100.0));
    let winner = env.trader(usdc(1_000.0));
    let other = env.trader(usdc(1_000.0));
    let w = winner.kp.pubkey();
    let pos = env.open(&w, &winner.kp, Side::Long, usdc(30.0), 2).unwrap();
    env.close_at(&pos, &winner.kp, 400.0).unwrap();
    env.seed(usdc(1_000.0));
    expect_err(env.harvest(&[other.kp.pubkey()]), "QueueAccountMismatch");
}

#[test]
fn harvest_pays_in_order_and_stops_when_cash_runs_out() {
    let mut env = Env::new();
    env.seed(usdc(100.0)); // cap 60
    let a = env.trader(usdc(1_000.0));
    let b = env.trader(usdc(1_000.0));
    let (ka, kb) = (a.kp.pubkey(), b.kp.pubkey());

    let pa = env.open(&ka, &a.kp, Side::Long, usdc(15.0), 2).unwrap();
    let pb = env.open(&kb, &b.kp, Side::Long, usdc(15.0), 2).unwrap();
    env.close_at(&pa, &a.kp, 1_000.0).unwrap(); // takes the vault's 100 and queues the rest
    env.close_at(&pb, &b.kp, 1_000.0).unwrap(); // the queue is not empty: the whole profit is queued
    let (qa, qb) = (env.user_state(&ka).queued, env.user_state(&kb).queued);
    assert!(qa > 0 && qb > qa);
    assert_eq!(env.vault_state().queue_total, qa + qb);

    // Enough for the first entry and part of the second
    env.seed(qa + 1);
    env.harvest(&[ka, kb]).unwrap();
    assert_eq!(env.user_state(&ka).queued, 0);
    assert_eq!(env.user_state(&kb).queued, qb - 1);
    assert_eq!(env.vault_state().cash, 0);
    env.assert_invariant();

    env.seed(usdc(10_000.0));
    env.harvest(&[kb]).unwrap();
    assert_eq!(env.vault_state().queue_total, 0);
    env.assert_invariant();
}
