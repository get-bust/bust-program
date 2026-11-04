use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};

use crate::{constants::*, error::PerpError, state::*};

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
    ctx.accounts.user.set_inner(UserAccount { owner: ctx.accounts.owner.key(), balance: 0, bump: ctx.bumps.user });
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

// ---------- seed_vault (capital and donations)

#[derive(Accounts)]
pub struct SeedVault<'info> {
    pub funder: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
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
    Ok(())
}
