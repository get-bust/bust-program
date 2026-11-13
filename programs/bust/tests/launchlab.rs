mod common;

use common::*;

const DEVNET_LAUNCHPAD: Pubkey = anchor_lang::prelude::pubkey!("DRay6fNdQ5J82H7xV6uq2aV3mNrUZ1J4PgSKsWgptcm6");

fn fee_wallet() -> Pubkey {
    pda(&[bust::FEE_WALLET_SEED])
}
fn launch_config() -> Pubkey {
    pda(&[bust::LAUNCH_CONFIG_SEED])
}

fn init_launch(env: &mut Env, program: Pubkey, quote: Pubkey, swap_destination: Pubkey) -> Result<(), String> {
    let admin = env.admin.insecure_clone();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::InitLaunchConfig { launchpad_program: program, platform_config: Pubkey::new_unique() }.data(),
        bust::accounts::InitLaunchConfig {
            admin: admin.pubkey(),
            config: config(),
            launch_config: launch_config(),
            quote_mint: quote,
            swap_destination,
            system_program: system_program::ID,
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
    let c = env.caller();
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::SweepLaunchFees {}.data(),
        bust::accounts::SweepLaunchFees {
            config: config(),
            vault: vault(),
            vault_tokens: vault_tokens(),
            distributor: distributor(),
            launch_config: launch_config(),
            fee_wallet: fee_wallet(),
            source,
            quote_mint: quote,
            destination,
            usdc_mint: env.mint,
            token_program,
        }
        .to_account_metas(None),
    );
    env.send(&[ix], &[&c])
}

#[test]
fn launch_config_only_accepts_raydium_launchlab() {
    let mut env = Env::new();
    let (mint, dest) = (env.mint, env.buyback_tokens);
    expect_err(init_launch(&mut env, Pubkey::new_unique(), mint, dest), "InvalidConfig");
    init_launch(&mut env, DEVNET_LAUNCHPAD, mint, dest).unwrap();
}

#[test]
fn usdc_launch_fees_enter_the_vault_as_capital() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    let (mint, dest) = (env.mint, env.buyback_tokens);
    init_launch(&mut env, DEVNET_LAUNCHPAD, mint, dest).unwrap();
    // Stand in for LaunchLab paying a claimed fee into the fee wallet
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
    assert_eq!(env.dist_state().buckets(), 0, "LaunchLab fees stay in the vault and are not distributed");
    expect_err(sweep(&mut env, source, mint, None), "NothingToWithdraw");
}

#[test]
fn non_usdc_launch_fees_go_only_to_the_swap_wallet() {
    let mut env = Env::new();
    env.seed(usdc(25_000.0));
    // A test mint in place of the SOL quote
    let quote = env.create_mint();
    let swap_owner = Pubkey::new_unique();
    let swap = env.token_account_of(&quote, &swap_owner, 0);
    init_launch(&mut env, DEVNET_LAUNCHPAD, quote, swap).unwrap();
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
    init_launch(&mut env, DEVNET_LAUNCHPAD, quote, swap).unwrap();
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
fn only_admin_configures_the_platform() {
    let mut env = Env::new();
    let (mint, dest) = (env.mint, env.buyback_tokens);
    init_launch(&mut env, DEVNET_LAUNCHPAD, mint, dest).unwrap();
    let attacker = env.caller();
    let theirs = env.token_account(&attacker.pubkey(), 0);
    let ix = Instruction::new_with_bytes(
        bust::id(),
        &bust::instruction::UpdateLaunchConfig { platform_config: Pubkey::new_unique() }.data(),
        bust::accounts::UpdateLaunchConfig {
            admin: attacker.pubkey(),
            config: config(),
            launch_config: launch_config(),
            swap_destination: theirs,
        }
        .to_account_metas(None),
    );
    expect_err(env.send(&[ix], &[&attacker]), "ConstraintHasOne");
}
