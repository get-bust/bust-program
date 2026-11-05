mod common;

use common::*;

#[test]
fn winning_close_pays_haircut_profit() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();

    let pos = env.open(&owner, &t.kp, Side::Long, usdc(100.0), 10).unwrap();
    assert_eq!(env.user_state(&owner).balance, usdc(900.0));
    let m: bust::Market = env.account(&env.market);
    assert_eq!(m.oi_long, usdc(1_000.0));
    env.assert_invariant();

    env.close_at(&pos, &t.kp, 101.0).unwrap();
    let win = math::win_breakdown(100_0000_0000, 101_0000_0000, usdc(1_000.0), ETH_MULT, &Params::DEFAULT);
    assert_eq!(env.user_state(&owner).balance, usdc(1_000.0) + win.net);
    let v = env.vault_state();
    assert_eq!(v.cash, usdc(25_000.0) - win.gain);
    assert_eq!(v.fees_accrued, win.fee);
    assert!(env.svm.get_account(&pos).is_none_or(|a| a.lamports == 0), "position closed");
    let m: bust::Market = env.account(&env.market);
    assert_eq!(m.oi_long, 0);
    env.assert_invariant();
}

#[test]
fn losing_close_moves_loss_to_vault() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();

    let pos = env.open(&owner, &t.kp, Side::Long, usdc(100.0), 10).unwrap();
    env.close_at(&pos, &t.kp, 99.0).unwrap();
    // A 10 USDC loss with a 2% fee
    assert_eq!(env.user_state(&owner).balance, usdc(990.0));
    let v = env.vault_state();
    assert_eq!(v.cash, usdc(25_000.0) + usdc(10.0) - usdc(0.2));
    assert_eq!(v.fees_accrued, usdc(0.2));
    env.assert_invariant();
}

#[test]
fn close_past_the_bust_price_loses_the_whole_margin() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();

    // 100× long @100 → bust at 99.05
    let pos = env.open(&owner, &t.kp, Side::Long, usdc(10.0), 100).unwrap();
    env.close_at(&pos, &t.kp, 99.0).unwrap();
    assert_eq!(env.user_state(&owner).balance, usdc(990.0));
    let v = env.vault_state();
    assert_eq!(v.cash, usdc(25_000.0) + usdc(10.0) - usdc(0.2));
    assert_eq!(v.open_margin, 0);
    env.assert_invariant();
}

#[test]
fn leverage_and_margin_limits() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();
    expect_err(env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 1001).map(|_| ()), "InvalidLeverage");
    expect_err(env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 0).map(|_| ()), "InvalidLeverage");
    expect_err(env.request_open(&owner, &t.kp, Side::Long, usdc(0.5), 10).map(|_| ()), "MarginTooSmall");
    expect_err(env.request_open(&owner, &t.kp, Side::Long, usdc(1_001.0), 10).map(|_| ()), "InsufficientBalance");
}

#[test]
fn session_key_trades_but_cannot_withdraw() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();
    let session = env.caller();

    // Nobody else trades on the account before a session is set
    expect_err(env.open(&owner, &session, Side::Long, usdc(10.0), 10).map(|_| ()), "Unauthorized");

    env.set_session(&t.kp, session.pubkey(), NOW + 30 * 86_400).unwrap();
    let pos = env.open(&owner, &session, Side::Long, usdc(10.0), 10).unwrap();
    env.close_at(&pos, &session, 100.0).unwrap();

    // Withdrawing still needs the owner's signature
    let mut ix = env.deposit_ix(&t, usdc(1.0), false);
    ix.accounts[0] = AccountMeta::new_readonly(session.pubkey(), true);
    assert!(env.send(&[ix], &[&session]).is_err());

    // The session ends at its expiry
    env.set_time(NOW + 30 * 86_400 + 1);
    env.set_price(100.0);
    expect_err(env.open(&owner, &session, Side::Long, usdc(10.0), 10).map(|_| ()), "Unauthorized");
}

#[test]
fn stale_or_uncertain_prices_are_rejected() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();

    let order = env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    env.set_time(env.now + FILL_DELAY);
    // Published before the fill time
    env.set_price_full(100.0, 0.01, env.now - 10);
    assert!(env.execute_open(&order).is_err());
    // Confidence of 1% > 0.2%
    env.set_price_full(100.0, 1.0, env.now);
    expect_err(env.execute_open(&order).map(|_| ()), "PriceConfidence");
    env.set_price(100.0);
    env.execute_open(&order).unwrap();
}

#[test]
fn an_order_fills_at_exactly_one_price() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();
    let order = env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    let fill_at = env.now + FILL_DELAY;
    let (price, mantissa) = (env.price, 100_0000_0000i64);
    let pyth = pyth_solana_receiver_sdk::ID;

    // Anything published before the fill time, including what the user saw when requesting
    env.set_time(fill_at);
    env.write_price_after(price, mantissa, fill_at - 1, fill_at - 2, pyth);
    expect_err(env.execute_open(&order).map(|_| ()), "PriceBeforeRequest");
    // A later update in the fill second: the first one is the only fill price
    env.write_price_after(price, mantissa, fill_at, fill_at, pyth);
    expect_err(env.execute_open(&order).map(|_| ()), "NotTheFillPrice");
    // Any update after that
    env.write_price_after(price, mantissa, fill_at + 3, fill_at + 2, pyth);
    expect_err(env.execute_open(&order).map(|_| ()), "NotTheFillPrice");

    // The first update at or after the fill time, even if no update landed exactly in that second
    env.write_price_after(price, 101_0000_0000, fill_at + 1, fill_at - 1, pyth);
    let pos = env.execute_open(&order).unwrap();
    let p: bust::Position = env.account(&pos);
    assert_eq!(p.entry_price, 101_0000_0000);
}

#[test]
fn a_fill_price_can_be_used_long_after_it_was_published() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();
    let order = env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    let fill_at = env.now + FILL_DELAY;
    // A user who does not like the fill price cannot get out of it by not filling: the timeout is far away,
    // and any keeper can still fill with that same message from Hermes
    env.set_time(fill_at + 50);
    let price = env.price;
    env.write_price_after(price, 90_0000_0000, fill_at, fill_at - 1, pyth_solana_receiver_sdk::ID);
    expect_err(env.cancel(&order), "OrderNotExpired");
    let pos = env.execute_open(&order).unwrap();
    let p: bust::Position = env.account(&pos);
    assert_eq!(p.entry_price, 90_0000_0000);
}

#[test]
fn cancel_only_after_timeout_refunds_margin() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();

    let order = env.request_open(&owner, &t.kp, Side::Long, usdc(100.0), 10).unwrap();
    let u = env.user_state(&owner);
    assert_eq!((u.balance, u.reserved), (usdc(900.0), usdc(100.0)));
    env.assert_invariant();

    // No cancel before the timeout, so nobody can see the price and walk away
    expect_err(env.cancel(&order), "OrderNotExpired");
    env.set_time(env.now + Params::DEFAULT.order_timeout_sec + 1);
    env.cancel(&order).unwrap();
    let u = env.user_state(&owner);
    assert_eq!((u.balance, u.reserved), (usdc(1_000.0), 0));
    env.assert_invariant();
    assert!(env.svm.get_account(&order).is_none_or(|a| a.lamports == 0), "order closed");
}

#[test]
fn partial_verification_needs_enough_signatures() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();

    let order = env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    env.set_time(env.now + FILL_DELAY);
    env.level = VerificationLevel::Partial { num_signatures: 2 };
    env.set_price(100.0);
    assert!(env.execute_open(&order).is_err(), "2 signatures must be rejected");
    env.level = VerificationLevel::Partial { num_signatures: 3 };
    env.set_price(100.0);
    env.execute_open(&order).unwrap();
    env.assert_invariant();
}

#[test]
fn close_order_must_match_its_position() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();
    let a = env.open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    let b = env.open(&owner, &t.kp, Side::Short, usdc(10.0), 10).unwrap();

    let order = env.request_close(&a, &t.kp).unwrap();
    env.advance_to_fill();
    let o: bust::OrderRequest = env.account(&order);
    let p: bust::Position = env.account(&b);
    let keeper = env.keeper.insecure_clone();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::ExecuteClose {}.data(),
        bust::accounts::ExecuteClose {
            keeper: keeper.pubkey(),
            order_rent_payer: o.rent_payer,
            position_rent_payer: p.rent_payer,
            config: config(),
            vault: vault(),
            market: p.market,
            user: user_pda(&owner),
            order,
            position: b,
            ticket: ticket_pda(&owner, env.user_state(&owner).next_ticket_id),
            price_update: env.price,
            distributor: distributor(),
            epoch_loss: epoch_loss_pda(&owner, env.dist_state().epoch),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&keeper]), "OrderMismatch");
    env.execute_close(&order).unwrap();
}

#[test]
fn liquidation_only_past_bust_price() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();

    // 100× long @100 → bust at 99.05
    let pos = env.open(&owner, &t.kp, Side::Long, usdc(10.0), 100).unwrap();
    env.set_price(99.1);
    expect_err(env.liquidate(&pos), "NotLiquidatable");
    env.set_price(99.0);
    env.liquidate(&pos).unwrap();
    assert_eq!(env.user_state(&owner).balance, usdc(990.0));
    let v = env.vault_state();
    assert_eq!(v.cash, usdc(25_000.0) + usdc(10.0) - usdc(0.2));
    assert_eq!(v.open_margin, 0);
    assert!(env.svm.get_account(&pos).is_none_or(|a| a.lamports == 0), "position closed");
    env.assert_invariant();
}

#[test]
fn short_is_liquidated_on_the_way_up() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();

    // 100× short @100 → bust at 100.95
    let pos = env.open(&owner, &t.kp, Side::Short, usdc(10.0), 100).unwrap();
    env.set_price(99.0);
    expect_err(env.liquidate(&pos), "NotLiquidatable");
    env.set_price(101.0);
    env.liquidate(&pos).unwrap();
    let m: bust::Market = env.account(&env.market);
    assert_eq!(m.oi_short, 0);
    env.assert_invariant();
}

#[test]
fn open_interest_cap_follows_vault() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0)); // cap 15,000
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();
    expect_err(env.open(&owner, &t.kp, Side::Long, usdc(160.0), 100).map(|_| ()), "OpenInterestCap");
    env.open(&owner, &t.kp, Side::Long, usdc(100.0), 100).unwrap(); // 10,000
    expect_err(env.open(&owner, &t.kp, Side::Long, usdc(60.0), 100).map(|_| ()), "OpenInterestCap");
    env.open(&owner, &t.kp, Side::Short, usdc(140.0), 100).unwrap(); // the other side has its own cap

    // More capital, more room
    env.seed(usdc(10_000.0)); // cap 21,000
    env.open(&owner, &t.kp, Side::Long, usdc(60.0), 100).unwrap();
}

#[test]
fn an_empty_vault_takes_no_positions() {
    let mut env = Env::new();
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();
    expect_err(env.open(&owner, &t.kp, Side::Long, usdc(10.0), 1).map(|_| ()), "OpenInterestCap");
}
