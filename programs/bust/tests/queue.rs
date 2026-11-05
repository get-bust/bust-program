mod common;

use common::*;

/// A long the vault cannot pay: 30 margin × 2 against a 100 vault, closed at 4× the entry price
fn short_vault_win(env: &mut Env) -> Trader {
    let winner = env.trader(usdc(1_000.0));
    let w = winner.kp.pubkey();
    let pos = env.open(&w, &winner.kp, Side::Long, usdc(30.0), 2).unwrap();
    env.close_at(&pos, &winner.kp, 400.0).unwrap();
    winner
}

#[test]
fn queue_when_vault_short_then_harvest_and_claim() {
    let mut env = Env::new();
    env.seed(usdc(100.0)); // a small vault: cap 60
    let winner = short_vault_win(&mut env); // ~160 profit against 100 in the vault
    let loser = env.trader(usdc(1_000.0));
    let (w, l) = (winner.kp.pubkey(), loser.kp.pubkey());

    let ws = env.user_state(&w);
    let v = env.vault_state();
    assert_eq!(v.cash, 0, "the vault pays what it has");
    assert!(ws.queued > 0 && v.queue_total == ws.queued);
    assert_eq!(v.fees_accrued, 0, "no fee on a partial payout");
    let t = env.ticket(&w, 0).expect("ticket 0");
    assert_eq!((t.start, t.amount, t.claimed), (0, ws.queued, 0));
    assert_eq!(ws.next_ticket_id, 1);
    env.assert_invariant();

    // With the debt equal to the cash the cap is zero: no new positions
    expect_err(env.open(&l, &loser.kp, Side::Long, usdc(10.0), 2).map(|_| ()), "OpenInterestCap");
    // Nothing is paid before cash comes in
    expect_err(env.harvest(), "NothingToWithdraw");
    expect_err(env.claim_ticket(&w, 0), "NothingToWithdraw");

    // Cash comes in: one harvest pays the debt, the claim moves it to the balance
    env.seed(usdc(1_000.0));
    env.harvest().unwrap();
    let v = env.vault_state();
    assert_eq!((v.queue_total, v.queue_unclaimed, v.paid_cumulative), (0, ws.queued, ws.queued));
    env.assert_invariant();

    let keeper_before = env.svm.get_balance(&env.keeper.pubkey()).unwrap();
    env.claim_ticket(&w, 0).unwrap();
    let ws2 = env.user_state(&w);
    assert_eq!(ws2.queued, 0);
    assert_eq!(ws2.balance, ws.balance + ws.queued);
    assert!(env.ticket(&w, 0).is_none(), "a fully paid ticket closes");
    assert!(env.svm.get_balance(&env.keeper.pubkey()).unwrap() > keeper_before, "the rent goes back to the keeper");
    assert_eq!(env.vault_state().queue_unclaimed, 0);
    env.assert_invariant();

    // The vault is positive again: trading resumes
    env.open(&l, &loser.kp, Side::Long, usdc(10.0), 2).unwrap();
}

#[test]
fn tickets_are_paid_in_order() {
    let mut env = Env::new();
    env.seed(usdc(100.0)); // cap 60
    let a = env.trader(usdc(1_000.0));
    let b = env.trader(usdc(1_000.0));
    let (ka, kb) = (a.kp.pubkey(), b.kp.pubkey());

    let pa = env.open(&ka, &a.kp, Side::Long, usdc(15.0), 2).unwrap();
    let pb = env.open(&kb, &b.kp, Side::Long, usdc(15.0), 2).unwrap();
    env.close_at(&pa, &a.kp, 1_000.0).unwrap(); // takes the vault's 100 and queues the rest
    env.close_at(&pb, &b.kp, 1_000.0).unwrap(); // there is debt already: the whole profit is queued
    let (ta, tb) = (env.ticket(&ka, 0).unwrap(), env.ticket(&kb, 0).unwrap());
    assert!(ta.amount > 0 && tb.amount > ta.amount);
    assert_eq!(tb.start, ta.start + ta.amount, "b's ticket starts where a's ends");
    assert_eq!(env.vault_state().queue_total, ta.amount + tb.amount);

    // Enough for a's ticket and one unit of b's
    env.seed(ta.amount + 1);
    env.harvest().unwrap();
    // Claim b first: order of claims does not change who was paid
    env.claim_ticket(&kb, 0).unwrap();
    env.claim_ticket(&ka, 0).unwrap();
    assert_eq!(env.user_state(&ka).queued, 0);
    assert!(env.ticket(&ka, 0).is_none());
    assert_eq!(env.user_state(&kb).queued, tb.amount - 1);
    assert_eq!(env.ticket(&kb, 0).unwrap().claimed, 1);
    assert_eq!(env.vault_state().cash, 0);
    env.assert_invariant();

    env.seed(usdc(10_000.0));
    env.harvest_and_claim(&[kb]);
    assert_eq!(env.vault_state().queue_total, 0);
    assert!(env.ticket(&kb, 0).is_none());
    env.assert_invariant();
}

#[test]
fn one_user_can_hold_several_tickets_in_line_with_others() {
    let mut env = Env::new();
    env.seed(usdc(100.0)); // cap 60
    let a = env.trader(usdc(1_000.0));
    let b = env.trader(usdc(1_000.0));
    let (ka, kb) = (a.kp.pubkey(), b.kp.pubkey());
    let a1 = env.open(&ka, &a.kp, Side::Long, usdc(10.0), 2).unwrap();
    let b1 = env.open(&kb, &b.kp, Side::Long, usdc(10.0), 2).unwrap();
    let a2 = env.open(&ka, &a.kp, Side::Long, usdc(10.0), 2).unwrap();
    for (p, kp) in [(a1, &a.kp), (b1, &b.kp), (a2, &a.kp)] {
        let kp = kp.insecure_clone();
        env.close_at(&p, &kp, 1_000.0).unwrap();
    }
    // a's two tickets sit on either side of b's
    let (a0, a_1, b0) = (env.ticket(&ka, 0).unwrap(), env.ticket(&ka, 1).unwrap(), env.ticket(&kb, 0).unwrap());
    assert!(a0.start < b0.start && b0.start < a_1.start);
    assert_eq!(env.user_state(&ka).queued, a0.amount + a_1.amount);

    // Pay through b's ticket only: a's second ticket gets nothing yet
    env.seed(a0.amount + b0.amount);
    env.harvest_and_claim(&[ka, kb]);
    assert!(env.ticket(&ka, 0).is_none() && env.ticket(&kb, 0).is_none());
    assert_eq!(env.ticket(&ka, 1).unwrap().claimed, 0);
    expect_err(env.claim_ticket(&ka, 1), "NothingToWithdraw");
    env.assert_invariant();
}

#[test]
fn a_ticket_pays_only_its_owner() {
    let mut env = Env::new();
    env.seed(usdc(100.0));
    let winner = short_vault_win(&mut env);
    let thief = env.trader(0);
    let (w, th) = (winner.kp.pubkey(), thief.kp.pubkey());
    env.seed(usdc(1_000.0));
    env.harvest().unwrap();

    let rent_payer = env.ticket(&w, 0).unwrap().rent_payer;
    expect_err(env.claim_ticket_into(&user_pda(&th), &w, 0, rent_payer), "ConstraintSeeds");
    // Nor can the rent be redirected
    let stranger = Pubkey::new_unique();
    expect_err(env.claim_ticket_into(&user_pda(&w), &w, 0, stranger), "ConstraintAddress");

    env.claim_ticket(&w, 0).unwrap();
    assert_eq!(env.user_state(&th).balance, 0);
    env.assert_invariant();
}

#[test]
fn lamports_sent_to_the_next_ticket_address_do_not_block_a_close() {
    let mut env = Env::new();
    env.seed(usdc(100.0));
    let winner = env.trader(usdc(1_000.0));
    let w = winner.kp.pubkey();
    let pos = env.open(&w, &winner.kp, Side::Long, usdc(30.0), 2).unwrap();

    // Anyone can fund the PDA of the next ticket before the close creates it
    let griefer = env.caller();
    let ix = system_instruction::transfer(&griefer.pubkey(), &ticket_pda(&w, 0), 1);
    env.send(&[ix], &[&griefer]).unwrap();

    env.close_at(&pos, &winner.kp, 400.0).unwrap();
    let t = env.ticket(&w, 0).expect("the ticket is created anyway");
    assert_eq!(t.amount, env.user_state(&w).queued);
    env.seed(usdc(1_000.0));
    env.harvest_and_claim(&[w]);
    assert!(env.ticket(&w, 0).is_none());
    env.assert_invariant();
}

#[test]
fn paid_but_unclaimed_tickets_do_not_hold_up_the_distribution() {
    let mut env = Env::new();
    env.seed(usdc(100.0));
    let winner = short_vault_win(&mut env);
    let w = winner.kp.pubkey();
    env.next_epoch();
    expect_err(env.distribute(), "QueueNotEmpty");

    env.seed(usdc(1_000.0));
    env.harvest().unwrap(); // paid, nobody has claimed yet
    env.distribute().unwrap();
    env.assert_invariant();
    env.claim_ticket(&w, 0).unwrap();
    env.assert_invariant();
}

#[test]
fn one_harvest_pays_any_number_of_tickets() {
    let mut env = Env::new();
    env.seed(usdc(100.0)); // cap 60: thirty positions of 2 notional
    let traders: Vec<Trader> = (0..6).map(|_| env.trader(usdc(100.0))).collect();
    let mut positions = Vec::new();
    for i in 0..30 {
        let t = &traders[i % traders.len()];
        let kp = t.kp.insecure_clone();
        positions.push((env.open(&kp.pubkey(), &kp, Side::Long, usdc(1.0), 2).unwrap(), kp));
    }
    for (p, kp) in &positions {
        env.close_at(p, kp, 1_000.0).unwrap();
    }
    let owners: Vec<Pubkey> = traders.iter().map(|t| t.kp.pubkey()).collect();
    let tickets: u64 = owners.iter().map(|o| env.user_state(o).next_ticket_id).sum();
    assert!(tickets >= 20, "most of the wins are queued ({tickets})");
    let debt = env.vault_state().queue_total;
    assert_eq!(owners.iter().map(|o| env.user_state(o).queued).sum::<u64>(), debt);

    env.seed(usdc(10_000.0));
    env.harvest().unwrap(); // a single instruction, whatever the number of tickets
    assert_eq!(env.vault_state().queue_total, 0);
    assert_eq!(env.vault_state().queue_unclaimed, debt);
    env.harvest_and_claim(&owners);
    assert_eq!(env.vault_state().queue_unclaimed, 0);
    assert!(owners.iter().all(|o| env.user_state(o).queued == 0));
    env.assert_invariant();
}
