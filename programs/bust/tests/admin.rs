mod common;

use common::*;

#[test]
fn initialize_opens_an_empty_vault() {
    let env = Env::new();
    let c = env.config_state();
    assert_eq!(c.admin, env.admin.pubkey());
    assert_eq!(c.usdc_mint, env.mint);
    assert_eq!(c.params, TEST_PARAMS);

    let v = env.vault_state();
    assert_eq!((v.cash, v.user_balances, v.open_margin, v.fees_accrued), (0, 0, 0, 0));
    assert_eq!(env.token_balance(&vault_tokens()), 0);
}

#[test]
fn initialize_runs_once() {
    let mut env = Env::new();
    assert!(env.initialize(Params::DEFAULT).is_err());
}

#[test]
fn only_admin_updates_config() {
    let mut env = Env::new();
    let params = Params { max_leverage: 500, ..Params::DEFAULT };

    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    expect_err(env.update_config(&stranger, params), "ConstraintHasOne");

    let admin = env.admin.insecure_clone();
    env.update_config(&admin, params).unwrap();
    assert_eq!(env.config_state().params.max_leverage, 500);
}

#[test]
fn config_rejects_invalid_params() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    for bad in [
        Params { max_leverage: 0, ..Params::DEFAULT },
        Params { win_fee_ppm: 1_000_000, ..Params::DEFAULT },
        Params { holdback_start_ppm: 600, holdback_end_ppm: 600, ..Params::DEFAULT },
        Params { oi_cap_ratio_ppm: 1_000_001, ..Params::DEFAULT },
        Params { oi_total_cap_ratio_ppm: 1_000_001, ..Params::DEFAULT },
        Params { order_timeout_sec: 2, fill_delay_sec: 2, ..Params::DEFAULT },
        // a 5 bps buffer is the whole margin at 2000×
        Params { max_leverage: 2000, ..Params::DEFAULT },
    ] {
        expect_err(env.update_config(&admin, bad), "InvalidConfig");
    }
    assert_eq!(env.config_state().params, TEST_PARAMS);
}

#[test]
fn admin_adds_markets_once_per_index() {
    let mut env = Env::new();
    let m: bust::Market = env.account(&env.market);
    assert_eq!((m.index, m.feed_id, m.position_multiplier_milli), (0, FEED, ETH_MULT));
    assert_eq!((m.oi_long, m.oi_short), (0, 0));

    let admin = env.admin.insecure_clone();
    assert!(env.add_market(&admin, 0, [9u8; 32], ETH_MULT).is_err());
    expect_err(env.add_market(&admin, 1, [9u8; 32], 0), "InvalidConfig");

    let stranger = env.caller();
    expect_err(env.add_market(&stranger, 1, [9u8; 32], ETH_MULT), "ConstraintHasOne");

    // indexes come in order, with no gaps
    expect_err(env.add_market(&admin, 2, [9u8; 32], ETH_MULT), "InvalidConfig");
    env.add_market(&admin, 1, [9u8; 32], 814_598).unwrap();
    assert_eq!(env.config_state().market_count, 2);
    let m: bust::Market = env.account(&market_pda(1));
    assert_eq!((m.index, m.feed_id), (1, [9u8; 32]));
}

#[test]
fn pause_stops_new_positions_only() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    let owner = t.kp.pubkey();
    let pos = env.open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
    let pending = env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();

    let admin = env.admin.insecure_clone();
    let pause = env.admin_ix(bust::instruction::SetPaused { paused: true }.data());
    env.send(&[pause], &[&admin]).unwrap();
    assert!(env.config_state().paused);

    expect_err(env.request_open(&owner, &t.kp, Side::Long, usdc(10.0), 10).map(|_| ()), "Paused");
    // An order requested before the pause is not filled either
    env.advance_to_fill();
    expect_err(env.execute_open(&pending).map(|_| ()), "Paused");

    // Closing and withdrawing still work
    env.close_at(&pos, &t.kp, 100.0).unwrap();
    let ix = env.deposit_ix(&t, usdc(100.0), false);
    env.send(&[ix], &[&t.kp]).unwrap();

    // Only the admin pauses or unpauses
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::SetPaused { paused: false }.data(),
        bust::accounts::AdminOnly { admin: owner, config: config() }.to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&t.kp]), "ConstraintHasOne");

    let unpause = env.admin_ix(bust::instruction::SetPaused { paused: false }.data());
    env.send(&[unpause], &[&admin]).unwrap();
    env.open(&owner, &t.kp, Side::Long, usdc(10.0), 10).unwrap();
}
