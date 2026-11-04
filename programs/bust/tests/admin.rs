mod common;

use common::*;

#[test]
fn initialize_opens_an_empty_vault() {
    let env = Env::new();
    let c = env.config_state();
    assert_eq!(c.admin, env.admin.pubkey());
    assert_eq!(c.usdc_mint, env.mint);
    assert_eq!(c.params, Params::DEFAULT);

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
        Params { order_timeout_sec: 5, max_fill_delay_sec: 5, ..Params::DEFAULT },
    ] {
        expect_err(env.update_config(&admin, bad), "InvalidConfig");
    }
    assert_eq!(env.config_state().params, Params::DEFAULT);
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

    env.add_market(&admin, 1, [9u8; 32], 814_598).unwrap();
    let m: bust::Market = env.account(&market_pda(1));
    assert_eq!((m.index, m.feed_id), (1, [9u8; 32]));
}
