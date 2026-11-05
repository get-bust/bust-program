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

// ---------- deposit address

/// USDC sent to the deposit PDA's associated token account, as an exchange withdrawal would
fn send_to_deposit_address(env: &mut Env, owner: &Pubkey, amount: u64) -> Pubkey {
    use anchor_spl::associated_token::{get_associated_token_address, spl_associated_token_account};
    let depositor = deposit_pda(owner);
    let ata = get_associated_token_address(&depositor, &env.mint);
    let admin = env.admin.insecure_clone();
    let create = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
        &admin.pubkey(),
        &depositor,
        &env.mint,
        &spl_token::ID,
    );
    let mint = spl_token::instruction::mint_to(&spl_token::ID, &env.mint, &ata, &admin.pubkey(), &[], amount).unwrap();
    env.send(&[create, mint], &[&admin]).unwrap();
    ata
}

#[test]
fn deposit_address_credits_its_owner() {
    let mut env = Env::new();
    let t = env.trader(usdc(100.0));
    let owner = t.kp.pubkey();
    let ata = send_to_deposit_address(&mut env, &owner, usdc(250.0));

    // Anyone can sweep; the owner does not sign
    env.sweep_deposit(&owner, ata).unwrap();
    assert_eq!(env.user_state(&owner).balance, usdc(350.0));
    assert_eq!(env.token_balance(&ata), 0);
    env.assert_invariant();

    expect_err(env.sweep_deposit(&owner, ata), "ZeroAmount");

    // A second transfer to the same address is swept the same way
    send_to_deposit_address(&mut env, &owner, usdc(1.5));
    env.sweep_deposit(&owner, ata).unwrap();
    assert_eq!(env.user_state(&owner).balance, usdc(351.5));
    env.assert_invariant();
}

#[test]
fn a_deposit_cannot_be_swept_into_another_balance() {
    let mut env = Env::new();
    let victim = env.trader(0);
    let thief = env.trader(0);
    let (v, th) = (victim.kp.pubkey(), thief.kp.pubkey());
    let ata = send_to_deposit_address(&mut env, &v, usdc(500.0));

    // The thief's deposit PDA does not own the victim's token account
    expect_err(env.sweep_deposit_to(&th, &th, ata), "ConstraintTokenOwner");
    // and the victim's deposit PDA does not derive from the thief's user account
    expect_err(env.sweep_deposit_to(&th, &v, ata), "ConstraintSeeds");

    env.sweep_deposit(&v, ata).unwrap();
    assert_eq!(env.user_state(&v).balance, usdc(500.0));
    assert_eq!(env.user_state(&th).balance, 0);
    env.assert_invariant();
}

#[test]
fn only_usdc_is_swept() {
    let mut env = Env::new();
    let t = env.trader(0);
    let owner = t.kp.pubkey();
    let other = env.create_mint();
    let tokens = env.token_account_of(&other, &deposit_pda(&owner), 1_000_000);
    expect_err(env.sweep_deposit(&owner, tokens), "ConstraintTokenMint");
}

#[test]
fn deposits_are_swept_while_paused() {
    let mut env = Env::new();
    let t = env.trader(0);
    let owner = t.kp.pubkey();
    let ata = send_to_deposit_address(&mut env, &owner, usdc(10.0));
    let admin = env.admin.insecure_clone();
    let pause = env.admin_ix(bust::instruction::SetPaused { paused: true }.data());
    env.send(&[pause], &[&admin]).unwrap();
    env.sweep_deposit(&owner, ata).unwrap();
    assert_eq!(env.user_state(&owner).balance, usdc(10.0));
}

#[test]
fn dust_is_not_swept_until_it_adds_up() {
    let mut env = Env::new();
    let t = env.trader(0);
    let owner = t.kp.pubkey();
    let ata = send_to_deposit_address(&mut env, &owner, usdc(0.5));
    expect_err(env.sweep_deposit(&owner, ata), "DepositTooSmall");
    send_to_deposit_address(&mut env, &owner, usdc(0.5));
    env.sweep_deposit(&owner, ata).unwrap();
    assert_eq!(env.user_state(&owner).balance, usdc(1.0));
}
