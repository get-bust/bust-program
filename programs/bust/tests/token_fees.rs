mod common;

use common::*;

fn fee_wallet() -> Pubkey {
    pda(&[bust::FEE_WALLET_SEED])
}
fn token_fee_config() -> Pubkey {
    pda(&[bust::TOKEN_FEE_CONFIG_SEED])
}

/// Opens the fee config with no operations share. The operations account is still registered, and is its own account:
/// a sweep takes it and the destination as writable, and the same account cannot be passed twice
fn init_fees(env: &mut Env, quote: Pubkey, swap_destination: Pubkey) -> Result<(), String> {
    let ops = env.token_account_of(&quote, &Pubkey::new_unique(), 0);
    init_fees_with(env, quote, swap_destination, ops, 0)
}

fn init_fees_with(
    env: &mut Env,
    quote: Pubkey,
    swap_destination: Pubkey,
    ops_destination: Pubkey,
    ops_bps: u16,
) -> Result<(), String> {
    let admin = env.admin.insecure_clone();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::InitTokenFeeConfig { ops_bps }.data(),
        bust::accounts::InitTokenFeeConfig {
            admin: admin.pubkey(),
            config: config(),
            token_fee_config: token_fee_config(),
            quote_mint: quote,
            swap_destination,
            ops_destination,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    env.send(&[ix], &[&admin])
}

fn update_fees(
    env: &mut Env,
    quote: Pubkey,
    swap_destination: Pubkey,
    ops_destination: Pubkey,
    ops_bps: u16,
) -> Result<(), String> {
    let admin = env.admin.insecure_clone();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::UpdateTokenFeeConfig { ops_bps }.data(),
        bust::accounts::UpdateTokenFeeConfig {
            admin: admin.pubkey(),
            config: config(),
            token_fee_config: token_fee_config(),
            quote_mint: quote,
            swap_destination,
            ops_destination,
        }
        .to_account_metas(None),
    );
    env.send(&[ix], &[&admin])
}

fn sweep(env: &mut Env, source: Pubkey, quote: Pubkey, destination: Option<Pubkey>) -> Result<(), String> {
    sweep_with(env, source, quote, destination, spl_token::ID)
}

fn sweep_with(
    env: &mut Env,
    source: Pubkey,
    quote: Pubkey,
    destination: Option<Pubkey>,
    token_program: Pubkey,
) -> Result<(), String> {
    let ops_destination = env.account::<bust::TokenFeeConfig>(&token_fee_config()).ops_destination;
    sweep_to(env, source, quote, destination, ops_destination, token_program)
}

fn sweep_to(
    env: &mut Env,
    source: Pubkey,
    quote: Pubkey,
    destination: Option<Pubkey>,
    ops_destination: Pubkey,
    token_program: Pubkey,
) -> Result<(), String> {
    let c = env.caller();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::SweepTokenFees {}.data(),
        bust::accounts::SweepTokenFees {
            config: config(),
            vault: vault(),
            vault_tokens: vault_tokens(),
            distributor: distributor(),
            token_fee_config: token_fee_config(),
            fee_wallet: fee_wallet(),
            source,
            quote_mint: quote,
            destination,
            ops_destination,
            usdc_mint: env.mint,
            token_program,
        }
        .to_account_metas(None),
    );
    env.send(&[ix], &[&c])
}

#[test]
fn only_the_admin_opens_the_fee_config_and_only_once() {
    let mut env = Env::new();
    let (mint, dest) = (env.mint, env.buyback_tokens);
    let attacker = env.caller();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::InitTokenFeeConfig { ops_bps: 0 }.data(),
        bust::accounts::InitTokenFeeConfig {
            admin: attacker.pubkey(),
            config: config(),
            token_fee_config: token_fee_config(),
            quote_mint: mint,
            swap_destination: dest,
            ops_destination: dest,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&attacker]), "ConstraintHasOne");
    init_fees(&mut env, mint, dest).unwrap();
    assert!(init_fees(&mut env, mint, dest).is_err());
}

#[test]
fn usdc_token_fees_enter_the_vault_as_capital() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let (mint, dest) = (env.mint, env.buyback_tokens);
    init_fees(&mut env, mint, dest).unwrap();
    // A fee paid in USDC into the fee wallet
    let source = env.token_account_of(&mint, &fee_wallet(), usdc(300.0));
    let v = env.vault_state();
    let mark = env.dist_state().cash_mark;

    let attacker = env.caller();
    let theirs = env.token_account(&attacker.pubkey(), 0);
    expect_err(sweep(&mut env, source, mint, Some(theirs)), "InvalidDestination");
    expect_err(sweep(&mut env, source, mint, Some(dest)), "InvalidDestination");
    sweep(&mut env, source, mint, None).unwrap();

    assert_eq!(env.vault_state().cash, v.cash + usdc(300.0));
    assert_eq!(env.dist_state().cash_mark, mark + usdc(300.0), "capital, not profit");
    assert_eq!(env.token_balance(&source), 0);
    env.assert_invariant();
    env.next_epoch();
    env.distribute().unwrap();
    assert_eq!(env.dist_state().buckets(), 0, "token fees stay in the vault and are not distributed");
    expect_err(sweep(&mut env, source, mint, None), "NothingToWithdraw");
}

#[test]
fn non_usdc_token_fees_go_only_to_the_swap_wallet() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    // A test mint in place of a fee token that is not USDC
    let quote = env.create_mint();
    let swap_owner = Pubkey::new_unique();
    let swap = env.token_account_of(&quote, &swap_owner, 0);
    init_fees(&mut env, quote, swap).unwrap();
    let source = env.token_account_of(&quote, &fee_wallet(), 5_000_000);
    let cash = env.vault_state().cash;

    expect_err(sweep(&mut env, source, quote, None), "InvalidDestination");
    let attacker = env.caller();
    let theirs = env.token_account_of(&quote, &attacker.pubkey(), 0);
    expect_err(sweep(&mut env, source, quote, Some(theirs)), "InvalidDestination");
    sweep(&mut env, source, quote, Some(swap)).unwrap();
    assert_eq!(env.token_balance(&swap), 5_000_000);
    assert_eq!(env.vault_state().cash, cash, "the vault ledger does not move");
    env.assert_invariant();
}

/// A Token-2022 mint without extensions has the SPL Token layout; only the owning program differs
fn token_2022_mint(env: &mut Env) -> Pubkey {
    let key = Pubkey::new_unique();
    let mut data = vec![0u8; spl_token::state::Mint::LEN];
    spl_token::state::Mint { decimals: 6, is_initialized: true, supply: 1_000_000_000_000, ..Default::default() }
        .pack_into_slice(&mut data);
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    let account = Account { lamports, data, owner: anchor_spl::token_2022::ID, executable: false, rent_epoch: 0 };
    env.svm.set_account(key, account).unwrap();
    key
}

fn token_2022_account(env: &mut Env, mint: &Pubkey, owner: &Pubkey, amount: u64) -> Pubkey {
    let key = Pubkey::new_unique();
    let mut data = vec![0u8; spl_token::state::Account::LEN];
    spl_token::state::Account {
        mint: *mint,
        owner: *owner,
        amount,
        state: spl_token::state::AccountState::Initialized,
        ..Default::default()
    }
    .pack_into_slice(&mut data);
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    let account = Account { lamports, data, owner: anchor_spl::token_2022::ID, executable: false, rent_epoch: 0 };
    env.svm.set_account(key, account).unwrap();
    key
}

#[test]
fn token_2022_fees_go_only_to_the_swap_wallet() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    // A fee paid in a Token-2022 token into the fee wallet's token account
    let quote = token_2022_mint(&mut env);
    let swap = token_2022_account(&mut env, &quote, &Pubkey::new_unique(), 0);
    init_fees(&mut env, quote, swap).unwrap();
    let source = token_2022_account(&mut env, &quote, &fee_wallet(), 5_000_000);
    let cash = env.vault_state().cash;
    let token_2022 = anchor_spl::token_2022::ID;

    // The wrong token program, no destination, and somebody else's account are all refused
    assert!(sweep_with(&mut env, source, quote, Some(swap), spl_token::ID).is_err());
    expect_err(sweep_with(&mut env, source, quote, None, token_2022), "InvalidDestination");
    let attacker = env.caller();
    let theirs = token_2022_account(&mut env, &quote, &attacker.pubkey(), 0);
    expect_err(sweep_with(&mut env, source, quote, Some(theirs), token_2022), "InvalidDestination");

    sweep_with(&mut env, source, quote, Some(swap), token_2022).unwrap();
    assert_eq!(env.token_balance(&swap), 5_000_000);
    assert_eq!(env.token_balance(&source), 0);
    assert_eq!(env.vault_state().cash, cash, "the vault ledger does not move");
    env.assert_invariant();
}

#[test]
fn only_the_admin_changes_the_fee_config() {
    let mut env = Env::new();
    let (mint, dest) = (env.mint, env.buyback_tokens);
    init_fees(&mut env, mint, dest).unwrap();
    let attacker = env.caller();
    let theirs = env.token_account(&attacker.pubkey(), 0);
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::UpdateTokenFeeConfig { ops_bps: 0 }.data(),
        bust::accounts::UpdateTokenFeeConfig {
            admin: attacker.pubkey(),
            config: config(),
            token_fee_config: token_fee_config(),
            quote_mint: mint,
            swap_destination: theirs,
            ops_destination: theirs,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&attacker]), "ConstraintHasOne");
}

#[test]
fn the_admin_can_move_fees_to_another_token() {
    let mut env = Env::new();
    let (mint, dest) = (env.mint, env.buyback_tokens);
    init_fees(&mut env, mint, dest).unwrap();
    let quote = token_2022_mint(&mut env);
    let swap = token_2022_account(&mut env, &quote, &Pubkey::new_unique(), 0);
    let admin = env.admin.insecure_clone();
    let ops = env.token_account_of(&quote, &Pubkey::new_unique(), 0);
    let update = |quote_mint: Pubkey, swap_destination: Pubkey| {
        Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::UpdateTokenFeeConfig { ops_bps: 0 }.data(),
            bust::accounts::UpdateTokenFeeConfig {
                admin: admin.pubkey(),
                config: config(),
                token_fee_config: token_fee_config(),
                quote_mint,
                swap_destination,
                ops_destination: ops,
            }
            .to_account_metas(None),
        )
    };
    // The swap account must hold the new quote token
    expect_err(env.send(&[update(quote, dest)], &[&admin]), "ConstraintTokenMint");
    env.send(&[update(quote, swap)], &[&admin]).unwrap();
    let c: bust::TokenFeeConfig = env.account(&token_fee_config());
    assert_eq!((c.quote_mint, c.swap_destination), (quote, swap));

    // Fees in the new token now leave the fee wallet; the old token no longer matches the config
    let source = token_2022_account(&mut env, &quote, &fee_wallet(), 1_000_000);
    sweep_with(&mut env, source, quote, Some(swap), anchor_spl::token_2022::ID).unwrap();
    assert_eq!(env.token_balance(&swap), 1_000_000);
    let old = env.token_account_of(&mint, &fee_wallet(), usdc(1.0));
    assert!(sweep(&mut env, old, mint, None).is_err());
}

#[test]
fn operations_take_their_share_and_the_rest_is_capital() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let (mint, dest) = (env.mint, env.buyback_tokens);
    let ops = env.token_account(&Pubkey::new_unique(), 0);
    init_fees_with(&mut env, mint, dest, ops, 2_000).unwrap();
    let source = env.token_account_of(&mint, &fee_wallet(), usdc(300.0));
    let v = env.vault_state();
    let mark = env.dist_state().cash_mark;

    // Only the registered operations account can take the share
    let attacker = env.caller();
    let theirs = env.token_account(&attacker.pubkey(), 0);
    assert!(sweep_to(&mut env, source, mint, None, theirs, spl_token::ID).is_err());

    sweep(&mut env, source, mint, None).unwrap();
    assert_eq!(env.token_balance(&ops), usdc(60.0), "a fifth to operations");
    assert_eq!(env.vault_state().cash, v.cash + usdc(240.0), "the rest to the vault");
    assert_eq!(env.dist_state().cash_mark, mark + usdc(240.0), "as capital, not profit");
    assert_eq!(env.token_balance(&source), 0);
    env.assert_invariant();
}

#[test]
fn a_token_fee_is_split_before_it_reaches_the_swap_wallet() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let quote = token_2022_mint(&mut env);
    let swap = token_2022_account(&mut env, &quote, &Pubkey::new_unique(), 0);
    let ops = token_2022_account(&mut env, &quote, &Pubkey::new_unique(), 0);
    init_fees_with(&mut env, quote, swap, ops, 2_000).unwrap();
    // An amount that does not divide evenly: the operations share rounds down, nothing is left behind
    let source = token_2022_account(&mut env, &quote, &fee_wallet(), 5_000_003);
    let cash = env.vault_state().cash;

    sweep_with(&mut env, source, quote, Some(swap), anchor_spl::token_2022::ID).unwrap();
    assert_eq!(env.token_balance(&ops), 1_000_000);
    assert_eq!(env.token_balance(&swap), 4_000_003);
    assert_eq!(env.token_balance(&source), 0);
    assert_eq!(env.vault_state().cash, cash, "the vault ledger does not move");
    env.assert_invariant();
}

#[test]
fn the_operations_share_is_capped_and_the_admin_can_lower_it() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let (mint, dest) = (env.mint, env.buyback_tokens);
    let ops = env.token_account(&Pubkey::new_unique(), 0);
    // More than a fifth is refused, when opening and when changing
    expect_err(init_fees_with(&mut env, mint, dest, ops, bust::MAX_TOKEN_FEE_OPS_BPS + 1), "InvalidConfig");
    init_fees_with(&mut env, mint, dest, ops, bust::MAX_TOKEN_FEE_OPS_BPS).unwrap();
    expect_err(update_fees(&mut env, mint, dest, ops, bust::MAX_TOKEN_FEE_OPS_BPS + 1), "InvalidConfig");
    // The operations account must hold the fee token
    let other = env.create_mint();
    let wrong = env.token_account_of(&other, &Pubkey::new_unique(), 0);
    expect_err(update_fees(&mut env, mint, dest, wrong, 1_000), "ConstraintTokenMint");

    // Lowered to nothing: the whole fee is capital again
    update_fees(&mut env, mint, dest, ops, 0).unwrap();
    let source = env.token_account_of(&mint, &fee_wallet(), usdc(100.0));
    let cash = env.vault_state().cash;
    sweep(&mut env, source, mint, None).unwrap();
    assert_eq!(env.token_balance(&ops), 0);
    assert_eq!(env.vault_state().cash, cash + usdc(100.0));
}
