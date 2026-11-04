mod common;

use common::*;

#[test]
fn deposit_withdraw_and_invariant() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let t = env.trader(usdc(1_000.0));
    assert_eq!(env.user_state(&t.kp.pubkey()).balance, usdc(1_000.0));
    env.assert_invariant();

    let ix = env.deposit_ix(&t, usdc(400.0), false);
    env.send(&[ix], &[&t.kp]).unwrap();
    assert_eq!(env.user_state(&t.kp.pubkey()).balance, usdc(600.0));
    assert_eq!(env.token_balance(&t.tokens), usdc(400.0));
    env.assert_invariant();

    let ix = env.deposit_ix(&t, usdc(10_000.0), false);
    expect_err(env.send(&[ix], &[&t.kp]), "InsufficientBalance");
}

#[test]
fn zero_amounts_are_rejected() {
    let mut env = Env::new();
    let t = env.trader(usdc(10.0));
    for deposit in [true, false] {
        let ix = env.deposit_ix(&t, 0, deposit);
        expect_err(env.send(&[ix], &[&t.kp]), "ZeroAmount");
    }
}

#[test]
fn seeded_capital_is_vault_cash_not_a_user_balance() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let v = env.vault_state();
    assert_eq!((v.cash, v.user_balances), (usdc(25_000.0), 0));
    env.assert_invariant();
}

#[test]
fn nobody_withdraws_from_someone_elses_account() {
    let mut env = Env::new();
    let victim = env.trader(usdc(1_000.0));
    let thief = env.trader(0);
    let thief_tokens = env.token_account(&thief.kp.pubkey(), 0);
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::Withdraw { amount: usdc(1_000.0) }.data(),
        bust::accounts::MoveFunds {
            owner: thief.kp.pubkey(),
            config: config(),
            vault: vault(),
            vault_tokens: vault_tokens(),
            user: user_pda(&victim.kp.pubkey()),
            owner_tokens: thief_tokens,
            usdc_mint: env.mint,
            token_program: spl_token::ID,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&thief.kp]), "ConstraintSeeds");
    assert_eq!(env.user_state(&victim.kp.pubkey()).balance, usdc(1_000.0));
}
