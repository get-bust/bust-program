use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::{constants::*, error::PerpError, state::*};

// ---------- initialize

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    pub usdc_mint: Account<'info, Mint>,
    #[account(init, payer = admin, space = 8 + Config::INIT_SPACE, seeds = [CONFIG_SEED], bump)]
    pub config: Account<'info, Config>,
    #[account(init, payer = admin, space = 8 + Vault::INIT_SPACE, seeds = [VAULT_SEED], bump)]
    pub vault: Account<'info, Vault>,
    #[account(
        init,
        payer = admin,
        seeds = [VAULT_TOKENS_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = vault,
    )]
    pub vault_tokens: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handle_initialize(ctx: Context<Initialize>, params: Params) -> Result<()> {
    require!(params.validate(), PerpError::InvalidConfig);
    ctx.accounts.config.set_inner(Config {
        admin: ctx.accounts.admin.key(),
        usdc_mint: ctx.accounts.usdc_mint.key(),
        params,
        bump: ctx.bumps.config,
    });
    ctx.accounts.vault.set_inner(Vault {
        cash: 0,
        user_balances: 0,
        open_margin: 0,
        fees_accrued: 0,
        bump: ctx.bumps.vault,
        tokens_bump: ctx.bumps.vault_tokens,
    });
    Ok(())
}

// ---------- update_config

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = admin)]
    pub config: Account<'info, Config>,
}

pub fn handle_update_config(ctx: Context<AdminOnly>, params: Params) -> Result<()> {
    require!(params.validate(), PerpError::InvalidConfig);
    ctx.accounts.config.params = params;
    Ok(())
}
