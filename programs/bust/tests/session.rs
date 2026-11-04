mod common;

use common::*;

#[test]
fn session_expiry_is_bounded() {
    let mut env = Env::new();
    let t = env.trader(usdc(100.0));
    let session = Pubkey::new_unique();
    let max = Params::DEFAULT.max_session_sec;

    expect_err(env.set_session(&t.kp, session, env.now), "InvalidSession");
    expect_err(env.set_session(&t.kp, session, env.now + max + 1), "InvalidSession");
    expect_err(env.set_session(&t.kp, Pubkey::default(), env.now + 60), "InvalidSession");

    env.set_session(&t.kp, session, env.now + max).unwrap();
    let u = env.user_state(&t.kp.pubkey());
    assert_eq!((u.session_key, u.session_expires_at), (session, env.now + max));
    assert!(u.can_trade(&session, env.now));
    assert!(!u.can_trade(&session, env.now + max));
    assert!(u.can_trade(&t.kp.pubkey(), env.now + max));
}

#[test]
fn session_key_cannot_withdraw() {
    let mut env = Env::new();
    let t = env.trader(usdc(100.0));
    let session = env.caller();
    env.set_session(&t.kp, session.pubkey(), env.now + 3600).unwrap();

    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::Withdraw { amount: usdc(100.0) }.data(),
        bust::accounts::MoveFunds {
            owner: session.pubkey(),
            config: config(),
            vault: vault(),
            vault_tokens: vault_tokens(),
            user: user_pda(&t.kp.pubkey()),
            owner_tokens: t.tokens,
            usdc_mint: env.mint,
            token_program: spl_token::ID,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&session]), "ConstraintSeeds");
    assert_eq!(env.user_state(&t.kp.pubkey()).balance, usdc(100.0));
}

#[test]
fn only_the_owner_sets_or_revokes_a_session() {
    let mut env = Env::new();
    let t = env.trader(usdc(100.0));
    let session = env.caller();
    env.set_session(&t.kp, session.pubkey(), env.now + 3600).unwrap();

    // The session key cannot extend itself or hand the account to another key
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::SetSession { session_key: session.pubkey(), expires_at: env.now + 7200 }.data(),
        bust::accounts::ManageSession { owner: session.pubkey(), config: config(), user: user_pda(&t.kp.pubkey()) }
            .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&session]), "ConstraintSeeds");

    let ix = env.session_ix(&t.kp, bust::instruction::RevokeSession {}.data());
    env.send(&[ix], &[&t.kp]).unwrap();
    let u = env.user_state(&t.kp.pubkey());
    assert_eq!((u.session_key, u.session_expires_at), (Pubkey::default(), 0));
    assert!(!u.can_trade(&session.pubkey(), env.now));
}
