use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};

use crate::{constants::*, error::PerpError, events::DepositSwept, state::*};

// ---------- create_user

#[derive(Accounts)]
pub struct CreateUser<'info> {
    pub owner: Signer<'info>,
    /// Pays the rent; can be a relayer
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + UserAccount::INIT_SPACE,
        seeds = [USER_SEED, owner.key().as_ref()],
        bump,
    )]
    pub user: Account<'info, UserAccount>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_user(ctx: Context<CreateUser>) -> Result<()> {
    ctx.accounts.user.set_inner(UserAccount {
        owner: ctx.accounts.owner.key(),
        balance: 0,
        reserved: 0,
        queued: 0,
        session_key: Pubkey::default(),
        session_expires_at: 0,
        next_position_id: 0,
        next_order_id: 0,
        next_ticket_id: 0,
        bump: ctx.bumps.user,
    });
    Ok(())
}

// ---------- deposit / withdraw

#[derive(Accounts)]
pub struct MoveFunds<'info> {
    pub owner: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, seeds = [VAULT_TOKENS_SEED], bump = vault.tokens_bump)]
    pub vault_tokens: Account<'info, TokenAccount>,
    #[account(mut, seeds = [USER_SEED, owner.key().as_ref()], bump = user.bump, has_one = owner)]
    pub user: Account<'info, UserAccount>,
    #[account(mut, token::mint = usdc_mint, token::authority = owner)]
    pub owner_tokens: Account<'info, TokenAccount>,
    pub usdc_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

pub fn handle_deposit(ctx: Context<MoveFunds>, amount: u64) -> Result<()> {
    require!(amount > 0, PerpError::ZeroAmount);
    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.owner_tokens.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.vault_tokens.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.usdc_mint.decimals,
    )?;
    let user = &mut ctx.accounts.user;
    let vault = &mut ctx.accounts.vault;
    user.balance = user.balance.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    vault.user_balances = vault.user_balances.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    Ok(())
}

/// Owner signature only; a session key cannot move funds out
pub fn handle_withdraw(ctx: Context<MoveFunds>, amount: u64) -> Result<()> {
    require!(amount > 0, PerpError::ZeroAmount);
    let user = &mut ctx.accounts.user;
    let vault = &mut ctx.accounts.vault;
    user.balance = user.balance.checked_sub(amount).ok_or(PerpError::InsufficientBalance)?;
    vault.user_balances = vault.user_balances.checked_sub(amount).ok_or(PerpError::MathOverflow)?;

    let bump = [vault.bump];
    let seeds: &[&[&[u8]]] = &[&[VAULT_SEED, &bump]];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.vault_tokens.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.owner_tokens.to_account_info(),
                authority: ctx.accounts.vault.to_account_info(),
            },
            seeds,
        ),
        amount,
        ctx.accounts.usdc_mint.decimals,
    )
}

// ---------- sweep_deposit (anyone; the USDC can only reach the deposit address's owner)

#[derive(Accounts)]
pub struct SweepDeposit<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, seeds = [VAULT_TOKENS_SEED], bump = vault.tokens_bump)]
    pub vault_tokens: Account<'info, TokenAccount>,
    #[account(mut, seeds = [USER_SEED, user.owner.as_ref()], bump = user.bump)]
    pub user: Account<'info, UserAccount>,
    /// CHECK: The owner's deposit PDA; holds nothing itself, only signs for its token accounts
    #[account(seeds = [DEPOSIT_SEED, user.owner.as_ref()], bump)]
    pub deposit_authority: UncheckedAccount<'info>,
    /// Usually the deposit PDA's associated token account; any USDC account it owns works
    #[account(mut, token::mint = usdc_mint, token::authority = deposit_authority)]
    pub deposit_tokens: Account<'info, TokenAccount>,
    pub usdc_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

/// Moves everything at the deposit address into the vault and credits the owner, so USDC sent from an exchange or
/// another wallet lands in the trading balance without the owner signing. A keeper sweeps; anyone may.
pub fn handle_sweep_deposit(ctx: Context<SweepDeposit>) -> Result<()> {
    let amount = ctx.accounts.deposit_tokens.amount;
    require!(amount > 0, PerpError::ZeroAmount);
    require!(amount >= ctx.accounts.config.params.min_sweep, PerpError::DepositTooSmall);
    let owner = ctx.accounts.user.owner;
    let bump = [ctx.bumps.deposit_authority];
    let seeds: &[&[&[u8]]] = &[&[DEPOSIT_SEED, owner.as_ref(), &bump]];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.deposit_tokens.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.vault_tokens.to_account_info(),
                authority: ctx.accounts.deposit_authority.to_account_info(),
            },
            seeds,
        ),
        amount,
        ctx.accounts.usdc_mint.decimals,
    )?;
    let user = &mut ctx.accounts.user;
    let vault = &mut ctx.accounts.vault;
    user.balance = user.balance.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    vault.user_balances = vault.user_balances.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    emit!(DepositSwept { owner, amount });
    Ok(())
}

// ---------- seed_vault (swapped token fees and donations)

#[derive(Accounts)]
pub struct SeedVault<'info> {
    pub funder: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    /// The high-water mark rises too, so capital put in is never distributed as profit
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Account<'info, Distributor>,
    #[account(mut, seeds = [VAULT_TOKENS_SEED], bump = vault.tokens_bump)]
    pub vault_tokens: Account<'info, TokenAccount>,
    #[account(mut, token::mint = usdc_mint, token::authority = funder)]
    pub funder_tokens: Account<'info, TokenAccount>,
    pub usdc_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

pub fn handle_seed_vault(ctx: Context<SeedVault>, amount: u64) -> Result<()> {
    require!(amount > 0, PerpError::ZeroAmount);
    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.funder_tokens.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.vault_tokens.to_account_info(),
                authority: ctx.accounts.funder.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.usdc_mint.decimals,
    )?;
    let vault = &mut ctx.accounts.vault;
    vault.cash = vault.cash.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    let d = &mut ctx.accounts.distributor;
    d.cash_mark = d.cash_mark.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    Ok(())
}

// ---------- session key

#[derive(Accounts)]
pub struct ManageSession<'info> {
    pub owner: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [USER_SEED, owner.key().as_ref()], bump = user.bump, has_one = owner)]
    pub user: Account<'info, UserAccount>,
}

/// A session key can only open and close positions; it lives at most config.max_session_sec
pub fn handle_set_session(ctx: Context<ManageSession>, session_key: Pubkey, expires_at: i64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(session_key != Pubkey::default(), PerpError::InvalidSession);
    require!(expires_at > now && expires_at <= now + ctx.accounts.config.params.max_session_sec, PerpError::InvalidSession);
    let user = &mut ctx.accounts.user;
    user.session_key = session_key;
    user.session_expires_at = expires_at;
    Ok(())
}

pub fn handle_revoke_session(ctx: Context<ManageSession>) -> Result<()> {
    let user = &mut ctx.accounts.user;
    user.session_key = Pubkey::default();
    user.session_expires_at = 0;
    Ok(())
}
