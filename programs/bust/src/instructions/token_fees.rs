use anchor_lang::prelude::*;
use anchor_spl::{
    token::{Mint, TokenAccount},
    token_interface::{self, TokenInterface, TransferChecked},
};

use crate::{constants::*, error::PerpError, events::TokenFeesSwept, state::*};

// ---------- init_token_fee_config / update_token_fee_config (admin)

#[derive(Accounts)]
pub struct InitTokenFeeConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin)]
    pub config: Account<'info, Config>,
    #[account(init, payer = admin, space = 8 + TokenFeeConfig::INIT_SPACE, seeds = [TOKEN_FEE_CONFIG_SEED], bump)]
    pub token_fee_config: Account<'info, TokenFeeConfig>,
    /// SPL Token or Token-2022: a fee can arrive in a token of either program
    pub quote_mint: InterfaceAccount<'info, token_interface::Mint>,
    #[account(token::mint = quote_mint)]
    pub swap_destination: InterfaceAccount<'info, token_interface::TokenAccount>,
    #[account(token::mint = quote_mint)]
    pub ops_destination: InterfaceAccount<'info, token_interface::TokenAccount>,
    pub system_program: Program<'info, System>,
}

pub fn handle_init_token_fee_config(ctx: Context<InitTokenFeeConfig>, ops_bps: u16) -> Result<()> {
    require!(ops_bps <= MAX_TOKEN_FEE_OPS_BPS, PerpError::InvalidConfig);
    let (_, fee_wallet_bump) = Pubkey::find_program_address(&[FEE_WALLET_SEED], &crate::ID);
    ctx.accounts.token_fee_config.set_inner(TokenFeeConfig {
        quote_mint: ctx.accounts.quote_mint.key(),
        swap_destination: ctx.accounts.swap_destination.key(),
        ops_destination: ctx.accounts.ops_destination.key(),
        ops_bps,
        bump: ctx.bumps.token_fee_config,
        fee_wallet_bump,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateTokenFeeConfig<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [TOKEN_FEE_CONFIG_SEED], bump = token_fee_config.bump)]
    pub token_fee_config: Account<'info, TokenFeeConfig>,
    /// The token fees arrive in. It can change with where $BUST trades; the swap account must hold the same token
    pub quote_mint: InterfaceAccount<'info, token_interface::Mint>,
    #[account(token::mint = quote_mint)]
    pub swap_destination: InterfaceAccount<'info, token_interface::TokenAccount>,
    #[account(token::mint = quote_mint)]
    pub ops_destination: InterfaceAccount<'info, token_interface::TokenAccount>,
}

pub fn handle_update_token_fee_config(ctx: Context<UpdateTokenFeeConfig>, ops_bps: u16) -> Result<()> {
    require!(ops_bps <= MAX_TOKEN_FEE_OPS_BPS, PerpError::InvalidConfig);
    let c = &mut ctx.accounts.token_fee_config;
    c.quote_mint = ctx.accounts.quote_mint.key();
    c.swap_destination = ctx.accounts.swap_destination.key();
    c.ops_destination = ctx.accounts.ops_destination.key();
    c.ops_bps = ops_bps;
    Ok(())
}

// ---------- sweep_token_fees (anyone)

#[derive(Accounts)]
pub struct SweepTokenFees<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(mut, seeds = [VAULT_TOKENS_SEED], bump = vault.tokens_bump)]
    pub vault_tokens: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Box<Account<'info, Distributor>>,
    #[account(seeds = [TOKEN_FEE_CONFIG_SEED], bump = token_fee_config.bump)]
    pub token_fee_config: Box<Account<'info, TokenFeeConfig>>,
    /// CHECK: The fee wallet PDA (signs only)
    #[account(seeds = [FEE_WALLET_SEED], bump = token_fee_config.fee_wallet_bump)]
    pub fee_wallet: UncheckedAccount<'info>,
    #[account(mut, token::mint = quote_mint, token::authority = fee_wallet, token::token_program = token_program)]
    pub source: Box<InterfaceAccount<'info, token_interface::TokenAccount>>,
    #[account(address = token_fee_config.quote_mint)]
    pub quote_mint: Box<InterfaceAccount<'info, token_interface::Mint>>,
    /// The registered swap account when the quote is not USDC; omitted for USDC (the money goes to the vault token account)
    #[account(mut)]
    pub destination: Option<Box<InterfaceAccount<'info, token_interface::TokenAccount>>>,
    /// The registered operations account; it takes its share of the sweep first
    #[account(mut, address = token_fee_config.ops_destination)]
    pub ops_destination: Box<InterfaceAccount<'info, token_interface::TokenAccount>>,
    pub usdc_mint: Box<Account<'info, Mint>>,
    /// The quote mint's own token program: SPL Token or Token-2022
    pub token_program: Interface<'info, TokenInterface>,
}

/// A fee that reached the fee wallet is split: the operations share (at most MAX_TOKEN_FEE_OPS_BPS, rounded down) goes
/// to the registered operations account, and the rest either enters the vault (USDC; counted as capital, not profit)
/// or goes to the registered swap account. The money cannot go anywhere else from the fee wallet, and the fee wallet
/// signs for nothing but these transfers.
pub fn handle_sweep_token_fees<'info>(ctx: Context<'info, SweepTokenFees<'info>>) -> Result<()> {
    let amount = ctx.accounts.source.amount;
    require!(amount > 0, PerpError::NothingToWithdraw);
    let to_vault = ctx.accounts.quote_mint.key() == ctx.accounts.usdc_mint.key();
    let to = match (&ctx.accounts.destination, to_vault) {
        (None, true) => ctx.accounts.vault_tokens.to_account_info(),
        (Some(d), false) if d.key() == ctx.accounts.token_fee_config.swap_destination => d.to_account_info(),
        _ => return err!(PerpError::InvalidDestination),
    };

    let ops = (amount as u128 * ctx.accounts.token_fee_config.ops_bps as u128 / 10_000) as u64;
    let rest = amount - ops;

    let bump = [ctx.accounts.token_fee_config.fee_wallet_bump];
    let seeds: &[&[&[u8]]] = &[&[FEE_WALLET_SEED, &bump]];
    let decimals = ctx.accounts.quote_mint.decimals;
    let pay = |to: AccountInfo<'info>, value: u64| {
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.source.to_account_info(),
                    mint: ctx.accounts.quote_mint.to_account_info(),
                    to,
                    authority: ctx.accounts.fee_wallet.to_account_info(),
                },
                seeds,
            ),
            value,
            decimals,
        )
    };
    if ops > 0 {
        pay(ctx.accounts.ops_destination.to_account_info(), ops)?;
    }
    if rest > 0 {
        pay(to, rest)?;
    }
    if to_vault {
        let vault = &mut ctx.accounts.vault;
        vault.cash = vault.cash.checked_add(rest).ok_or(PerpError::MathOverflow)?;
        let d = &mut ctx.accounts.distributor;
        d.cash_mark = d.cash_mark.checked_add(rest).ok_or(PerpError::MathOverflow)?;
    }
    emit!(TokenFeesSwept { amount, ops, to_vault });
    Ok(())
}
