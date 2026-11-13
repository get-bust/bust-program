//! Attacks on the instructions that the module tests do not already cover: wrong prices, wrong accounts,
//! replays, second initialisations and round trips that try to pull money out of the vault.

mod common;

use common::*;

const OTHER_FEED: [u8; 32] = [9u8; 32];

fn funded(env: &mut Env) -> Trader {
    env.seed(usdc(25_000.0));
    env.trader(usdc(1_000.0))
}

// ---------- prices

#[test]
fn a_fill_cannot_use_another_feeds_price() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let order = env.request_open(&t.kp.pubkey(), &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    env.advance_to_fill();
    // A real, fresh Pyth update, but for another asset
    let other = Pubkey::new_unique();
    let now = env.now;
    env.write_price(other, OTHER_FEED, 1_0000_0000, -8, 10_000, now, pyth_solana_receiver_sdk::ID);
    let market = env.market;
    expect_err(env.execute_open_with(&order, other, market).map(|_| ()), "MismatchedFeedId");
    env.execute_open(&order).unwrap();
}

#[test]
fn a_fill_cannot_use_a_price_account_pyth_does_not_own() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let order = env.request_open(&t.kp.pubkey(), &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    env.advance_to_fill();
    // The same bytes Pyth would write, in an account the attacker owns
    let fake = Pubkey::new_unique();
    let now = env.now;
    env.write_price(fake, FEED, 100_000_000_000, -8, 10_000, now, Pubkey::new_unique());
    let market = env.market;
    expect_err(env.execute_open_with(&order, fake, market).map(|_| ()), "AccountOwnedByWrongProgram");
}

#[test]
fn prices_in_other_exponents_are_rescaled() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let owner = t.kp.pubkey();
    let order = env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    env.advance_to_fill();
    // $100 at exponent -5
    let (price, now) = (env.price, env.now);
    env.write_price(price, FEED, 100_00000, -5, 100, now, pyth_solana_receiver_sdk::ID);
    let pos = env.execute_open(&order).unwrap();
    let p: bust::Position = env.account(&pos);
    assert_eq!(p.entry_price, 100_0000_0000);

    // An exponent the program cannot represent is refused, not misread
    let order = env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    env.advance_to_fill();
    let now = env.now;
    env.write_price(price, FEED, 1, 30, 0, now, pyth_solana_receiver_sdk::ID);
    expect_err(env.execute_open(&order).map(|_| ()), "InvalidPrice");
}

#[test]
fn a_fill_must_use_the_orders_market() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let admin = env.admin.insecure_clone();
    env.add_market(&admin, 1, OTHER_FEED, ETH_MULT).unwrap();
    let order = env.request_open(&t.kp.pubkey(), &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    env.advance_to_fill();
    let price = env.price;
    expect_err(env.execute_open_with(&order, price, market_pda(1)).map(|_| ()), "ConstraintAddress");
}

#[test]
fn a_liquidation_needs_a_fresh_price() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let owner = t.kp.pubkey();
    let pos = env.open(&owner, &t.kp, Side::Long, usdc(10.0), 100).unwrap();
    // A bust price from a minute ago cannot be used now
    let at = env.now;
    env.set_time(at + 60);
    env.set_price_full(98.0, 0.01, at);
    assert!(env.liquidate(&pos).is_err());
    assert!(env.svm.get_account(&pos).is_some_and(|a| !a.data.is_empty()), "the position is still open");
}

// ---------- accounts that do not belong together

#[test]
fn nobody_closes_someone_elses_position() {
    let mut env = Env::new();
    let victim = funded(&mut env);
    let attacker = env.trader(usdc(100.0));
    let pos = env.open(&victim.kp.pubkey(), &victim.kp, Side::Long, usdc(10.0), 10).unwrap();
    // The attacker's own key, and a session key the attacker set on their own account, are both refused
    expect_err(env.request_close(&pos, &attacker.kp).map(|_| ()), "Unauthorized");
    let session = env.caller();
    env.set_session(&attacker.kp, session.pubkey(), env.now + 3600).unwrap();
    expect_err(env.request_close(&pos, &session).map(|_| ()), "Unauthorized");
}

#[test]
fn a_liquidation_settles_only_against_the_positions_owner() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let other = env.trader(usdc(100.0));
    let owner = t.kp.pubkey();
    let pos = env.open(&owner, &t.kp, Side::Long, usdc(10.0), 100).unwrap();
    env.set_price(98.0);
    let p: bust::Position = env.account(&pos);
    let keeper = env.caller();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::Liquidate {}.data(),
        bust::accounts::Liquidate {
            keeper: keeper.pubkey(),
            rent_payer: p.rent_payer,
            config: config(),
            vault: vault(),
            market: p.market,
            user: user_pda(&other.kp.pubkey()),
            position: pos,
            price_update: env.price,
            distributor: distributor(),
            epoch_loss: epoch_loss_pda(&other.kp.pubkey(), 0),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&keeper]), "ConstraintSeeds");
    env.liquidate(&pos).unwrap();
}

#[test]
fn rent_goes_back_only_to_whoever_paid_it() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let owner = t.kp.pubkey();
    let pos = env.open(&owner, &t.kp, Side::Long, usdc(10.0), 100).unwrap();
    env.set_price(98.0);
    let p: bust::Position = env.account(&pos);
    let keeper = env.caller();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::Liquidate {}.data(),
        bust::accounts::Liquidate {
            keeper: keeper.pubkey(),
            rent_payer: keeper.pubkey(), // not the one who paid for the position
            config: config(),
            vault: vault(),
            market: p.market,
            user: user_pda(&owner),
            position: pos,
            price_update: env.price,
            distributor: distributor(),
            epoch_loss: epoch_loss_pda(&owner, 0),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&keeper]), "ConstraintAddress");
}

// ---------- replays

#[test]
fn an_order_fills_once_and_a_filled_order_cannot_be_cancelled() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let owner = t.kp.pubkey();
    let order = env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    env.advance_to_fill();
    env.execute_open(&order).unwrap();
    assert!(env.svm.get_account(&order).is_none_or(|a| a.data.is_empty()), "the order account is closed");
    env.set_time(env.now + 60);
    env.set_price(100.0);

    // Replaying the fill would open a second position from the same margin
    let keeper = env.keeper.insecure_clone();
    let fill = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::ExecuteOpen {}.data(),
        bust::accounts::ExecuteOpen {
            keeper: keeper.pubkey(),
            rent_payer: owner,
            config: config(),
            vault: vault(),
            market: env.market,
            user: user_pda(&owner),
            order,
            position: position_pda(&owner, 1),
            price_update: env.price,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    assert!(env.send(&[fill], &[&keeper]).is_err());
    // Cancelling it would refund margin that is already in a position
    let cancel = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::CancelOrder {}.data(),
        bust::accounts::CancelOrder {
            caller: keeper.pubkey(),
            config: config(),
            rent_payer: owner,
            user: user_pda(&owner),
            order,
            vault: vault(),
            market: env.market,
            price_update: Some(env.price),
            position: Pubkey::default(),
        }
        .to_account_metas(None),
    );
    assert!(env.send(&[cancel], &[&keeper]).is_err());
    let u = env.user_state(&owner);
    assert_eq!((u.balance, u.reserved, u.next_position_id), (usdc(990.0), 0, 1));
    env.assert_invariant();
}

#[test]
fn nothing_initialises_twice() {
    let mut env = Env::new();
    assert!(env.initialize(Params::DEFAULT).is_err());
    let admin = env.admin.insecure_clone();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::InitDistributor { params: DistParams::DEFAULT }.data(),
        bust::accounts::InitDistributor {
            admin: admin.pubkey(),
            config: config(),
            vault: vault(),
            distributor: distributor(),
            buyback_tokens: env.buyback_tokens,
            dev_tokens: env.dev_tokens,
            usdc_mint: env.mint,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    assert!(env.send(&[ix], &[&admin]).is_err());
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::InitStakePool { unstake_sec: 1 }.data(),
        bust::accounts::InitStakePool {
            admin: admin.pubkey(),
            config: config(),
            bust_mint: env.bust_mint,
            stake_pool: stake_pool(),
            stake_tokens: stake_tokens(),
            token_program: spl_token::ID,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    assert!(env.send(&[ix], &[&admin]).is_err());
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::InitTreasury { vest_sec: 1, buyback_authority: admin.pubkey() }.data(),
        bust::accounts::InitTreasury {
            admin: admin.pubkey(),
            config: config(),
            bust_mint: env.bust_mint,
            treasury: treasury(),
            treasury_tokens: treasury_tokens(),
            xbust_mint: xbust_mint(),
            token_program: spl_token::ID,
            bust_token_program: spl_token::ID,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    assert!(env.send(&[ix], &[&admin]).is_err());
    // The buyback authority did not change
    assert_eq!(env.account::<bust::Treasury>(&treasury()).buyback_authority, env.buyback.pubkey());
}

// ---------- admin-only settings

#[test]
fn only_admin_changes_the_buyback_authority() {
    let mut env = Env::new();
    let attacker = env.caller();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::UpdateTreasury { buyback_authority: attacker.pubkey() }.data(),
        bust::accounts::UpdateTreasury { admin: attacker.pubkey(), config: config(), treasury: treasury() }
            .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&attacker]), "ConstraintHasOne");
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::MigrateTreasury {}.data(),
        bust::accounts::MigrateTreasury {
            admin: attacker.pubkey(),
            config: config(),
            treasury: treasury(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&attacker]), "ConstraintHasOne");
}

// ---------- tokens that belong to someone else

#[test]
fn nobody_stakes_or_locks_with_someone_elses_tokens() {
    let mut env = Env::new();
    let victim = env.staker(1_000_000);
    let attacker = env.staker(0);
    // The attacker's stake account, the victim's $BUST
    let thief = Staker { kp: attacker.kp.insecure_clone(), tokens: victim.tokens, usdc: attacker.usdc };
    expect_err(env.stake(&thief, 1_000_000), "ConstraintTokenOwner");

    let victim_xbust = env.xbust_account(&victim.kp.pubkey());
    let akp = attacker.kp.insecure_clone();
    expect_err(env.lock_xbust(&akp, victim_xbust, 1), "ConstraintTokenOwner");
}

#[test]
fn an_epoch_with_no_losses_cannot_be_funded() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    env.next_epoch();
    env.distribute().unwrap();
    expect_err(env.fund_epoch(0, 1_000_000), "NothingToWithdraw");
}

// ---------- round trips

#[test]
fn opening_and_closing_at_the_same_price_never_gains() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let owner = t.kp.pubkey();
    let start = env.user_state(&owner).balance;
    for lev in [1u16, 10, 100, 1000] {
        for side in [Side::Long, Side::Short] {
            let pos = env.open(&owner, &t.kp, side, usdc(1.0), lev).unwrap();
            env.close_at(&pos, &t.kp, 100.0).unwrap();
        }
    }
    assert!(env.user_state(&owner).balance <= start);
    env.assert_invariant();
}

#[test]
fn tiny_moves_inside_the_dead_band_pay_nothing() {
    let mut env = Env::new();
    let t = funded(&mut env);
    let owner = t.kp.pubkey();
    let start = env.user_state(&owner).balance;
    // +0.1 bps on 1000× is +1% of margin before the dead band; after it, nothing
    for _ in 0..5 {
        let pos = env.open(&owner, &t.kp, Side::Long, usdc(10.0), 1000).unwrap();
        env.close_at(&pos, &t.kp, 100.001).unwrap();
    }
    assert_eq!(env.user_state(&owner).balance, start);
    env.assert_invariant();
}

#[test]
fn a_cap_is_never_a_way_to_hold_an_old_fill_price() {
    // With the cap checked at the fill, an order refused here could wait until the price moved, then fill at the old
    // price once its owner freed the cap
    let mut env = Env::new();
    env.seed(usdc(1_000.0)); // a cap of 600
    env.set_price(100.0);
    let t = env.trader(usdc(2_000.0));
    let o = t.kp.pubkey();
    let position = env.open(&o, &t.kp, Side::Long, usdc(60.0), 10).unwrap();
    let balance = env.user_state(&o).balance;
    let next = env.user_state(&o).next_order_id;
    expect_err(env.request_open(&o, &t.kp, Side::Long, usdc(30.0), 10).map(|_| ()), "OpenInterestCap");
    assert_eq!(env.user_state(&o).balance, balance);
    assert_eq!(env.user_state(&o).next_order_id, next, "no order is left waiting");
    env.close_at(&position, &t.kp, 100.0).unwrap();
    assert_eq!((env.market_state().pending_long, env.vault_state().pending_long_total), (0, 0));
    env.assert_invariant();
}

#[test]
fn an_order_within_the_caps_still_cannot_be_cancelled() {
    let mut env = Env::new();
    env.seed(usdc(1_000.0));
    env.set_price(100.0);
    let t = env.trader(usdc(2_000.0));
    let o = t.kp.pubkey();
    let order = env.request_open(&o, &t.kp, Side::Long, usdc(30.0), 10).unwrap();
    env.advance_to_fill();
    expect_err(env.cancel(&order), "OrderFillable");
    env.execute_open(&order).unwrap();
}
