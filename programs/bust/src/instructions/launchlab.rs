use anchor_lang::{
    prelude::*,
    solana_program::{
        instruction::{AccountMeta, Instruction},
        program::invoke_signed,
    },
};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{Mint, Token, TokenAccount},
    token_interface::{self, TokenInterface, TransferChecked},
};

use crate::{constants::*, error::PerpError, events::LaunchFeesSwept, state::*};

// ---------- init_launch_config / update_launch_config (admin)

#[derive(Accounts)]
pub struct InitLaunchConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin)]
    pub config: Account<'info, Config>,
    #[account(init, payer = admin, space = 8 + LaunchConfig::INIT_SPACE, seeds = [LAUNCH_CONFIG_SEED], bump)]
    pub launch_config: Account<'info, LaunchConfig>,
    /// SPL Token or Token-2022: a fee can arrive in a token of either program
    pub quote_mint: InterfaceAccount<'info, token_interface::Mint>,
    #[account(token::mint = quote_mint)]
    pub swap_destination: InterfaceAccount<'info, token_interface::TokenAccount>,
    pub system_program: Program<'info, System>,
}

pub fn handle_init_launch_config(
    ctx: Context<InitLaunchConfig>,
    launchpad_program: Pubkey,
    platform_config: Pubkey,
) -> Result<()> {
    require!(LAUNCHPAD_PROGRAMS.contains(&launchpad_program), PerpError::InvalidConfig);
    let (_, fee_wallet_bump) = Pubkey::find_program_address(&[FEE_WALLET_SEED], &crate::ID);
    ctx.accounts.launch_config.set_inner(LaunchConfig {
        launchpad_program,
        platform_config,
        quote_mint: ctx.accounts.quote_mint.key(),
        swap_destination: ctx.accounts.swap_destination.key(),
        bump: ctx.bumps.launch_config,
        fee_wallet_bump,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateLaunchConfig<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [LAUNCH_CONFIG_SEED], bump = launch_config.bump)]
    pub launch_config: Account<'info, LaunchConfig>,
    #[account(token::mint = launch_config.quote_mint)]
    pub swap_destination: InterfaceAccount<'info, token_interface::TokenAccount>,
}

pub fn handle_update_launch_config(ctx: Context<UpdateLaunchConfig>, platform_config: Pubkey) -> Result<()> {
    let c = &mut ctx.accounts.launch_config;
    c.platform_config = platform_config;
    c.swap_destination = ctx.accounts.swap_destination.key();
    Ok(())
}

// ---------- claim_launch_fees (anyone; claims from LaunchLab with the fee wallet PDA's signature)

#[derive(Accounts)]
pub struct ClaimLaunchFees<'info> {
    pub caller: Signer<'info>,
    #[account(seeds = [LAUNCH_CONFIG_SEED], bump = launch_config.bump)]
    pub launch_config: Account<'info, LaunchConfig>,
    /// CHECK: This program's PDA; passed to LaunchLab as a writable signer (the recipient already exists, so it pays nothing)
    #[account(mut, seeds = [FEE_WALLET_SEED], bump = launch_config.fee_wallet_bump)]
    pub fee_wallet: UncheckedAccount<'info>,
    /// CHECK: The LaunchLab program stored in launch_config, from the allow list
    #[account(executable, address = launch_config.launchpad_program)]
    pub launchpad_program: UncheckedAccount<'info>,
    /// CHECK: Our PlatformConfig; LaunchLab checks the fee wallet against it
    #[account(address = launch_config.platform_config)]
    pub platform_config: UncheckedAccount<'info>,
    /// Where the fee arrives: the fee wallet's ATA for the quote token
    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = fee_wallet,
    )]
    pub recipient: Account<'info, TokenAccount>,
    #[account(address = launch_config.quote_mint)]
    pub quote_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    // remaining_accounts (in LaunchLab's order):
    //   from a pool:  [authority, pool_state (w), quote_vault (w)]
    //   from vault:   [fee_vault_authority, platform_fee_vault (w)]
}

/// from_vault: from LaunchLab's platform fee vault (claim_platform_fee_from_vault), otherwise from a single pool
pub fn handle_claim_launch_fees<'info>(ctx: Context<'info, ClaimLaunchFees<'info>>, from_vault: bool) -> Result<()> {
    let a = &ctx.accounts;
    let rest = ctx.remaining_accounts;
    let meta = |info: &AccountInfo, writable: bool| {
        if writable {
            AccountMeta::new(info.key(), false)
        } else {
            AccountMeta::new_readonly(info.key(), false)
        }
    };
    let wallet = AccountMeta::new(a.fee_wallet.key(), true);
    let tail = [
        AccountMeta::new(a.recipient.key(), false),
        AccountMeta::new_readonly(a.quote_mint.key(), false),
        AccountMeta::new_readonly(a.token_program.key(), false),
        AccountMeta::new_readonly(a.system_program.key(), false),
        AccountMeta::new_readonly(a.associated_token_program.key(), false),
    ];
    let (data, mut metas) = if from_vault {
        require!(rest.len() == 2, PerpError::InvalidConfig);
        (
            IX_CLAIM_PLATFORM_FEE_FROM_VAULT,
            vec![wallet, meta(&rest[0], false), AccountMeta::new_readonly(a.platform_config.key(), false), meta(&rest[1], true)],
        )
    } else {
        require!(rest.len() == 3, PerpError::InvalidConfig);
        (
            IX_CLAIM_PLATFORM_FEE,
            vec![
                wallet,
                meta(&rest[0], false),
                meta(&rest[1], true),
                AccountMeta::new_readonly(a.platform_config.key(), false),
                meta(&rest[2], true),
            ],
        )
    };
    metas.extend(tail);
    let ix = Instruction { program_id: a.launchpad_program.key(), accounts: metas, data: data.to_vec() };

    let mut infos = vec![
        a.fee_wallet.to_account_info(),
        a.platform_config.to_account_info(),
        a.recipient.to_account_info(),
        a.quote_mint.to_account_info(),
        a.token_program.to_account_info(),
        a.system_program.to_account_info(),
        a.associated_token_program.to_account_info(),
        a.launchpad_program.to_account_info(),
    ];
    infos.extend(rest.iter().cloned());
    let bump = [a.launch_config.fee_wallet_bump];
    invoke_signed(&ix, &infos, &[&[FEE_WALLET_SEED, &bump]])?;
    Ok(())
}

// ---------- sweep_launch_fees (anyone)

#[derive(Accounts)]
pub struct SweepLaunchFees<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(mut, seeds = [VAULT_TOKENS_SEED], bump = vault.tokens_bump)]
    pub vault_tokens: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Box<Account<'info, Distributor>>,
    #[account(seeds = [LAUNCH_CONFIG_SEED], bump = launch_config.bump)]
    pub launch_config: Box<Account<'info, LaunchConfig>>,
    /// CHECK: The fee wallet PDA (signs only)
    #[account(seeds = [FEE_WALLET_SEED], bump = launch_config.fee_wallet_bump)]
    pub fee_wallet: UncheckedAccount<'info>,
    #[account(mut, token::mint = quote_mint, token::authority = fee_wallet, token::token_program = token_program)]
    pub source: Box<InterfaceAccount<'info, token_interface::TokenAccount>>,
    #[account(address = launch_config.quote_mint)]
    pub quote_mint: Box<InterfaceAccount<'info, token_interface::Mint>>,
    /// The registered swap account when the quote is not USDC; omitted for USDC (the money goes to the vault token account)
    #[account(mut)]
    pub destination: Option<Box<InterfaceAccount<'info, token_interface::TokenAccount>>>,
    pub usdc_mint: Box<Account<'info, Mint>>,
    /// The quote mint's own token program: SPL Token or Token-2022
    pub token_program: Interface<'info, TokenInterface>,
}

/// A claimed fee either enters the vault (USDC; counted as capital, not profit) or goes to the registered swap account.
/// The money cannot go anywhere else from the fee wallet.
pub fn handle_sweep_launch_fees(ctx: Context<SweepLaunchFees>) -> Result<()> {
    let amount = ctx.accounts.source.amount;
    require!(amount > 0, PerpError::NothingToWithdraw);
    let to_vault = ctx.accounts.quote_mint.key() == ctx.accounts.usdc_mint.key();
    let to = match (&ctx.accounts.destination, to_vault) {
        (None, true) => ctx.accounts.vault_tokens.to_account_info(),
        (Some(d), false) if d.key() == ctx.accounts.launch_config.swap_destination => d.to_account_info(),
        _ => return err!(PerpError::InvalidDestination),
    };

    let bump = [ctx.accounts.launch_config.fee_wallet_bump];
    let seeds: &[&[&[u8]]] = &[&[FEE_WALLET_SEED, &bump]];
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
        amount,
        ctx.accounts.quote_mint.decimals,
    )?;
    if to_vault {
        let vault = &mut ctx.accounts.vault;
        vault.cash = vault.cash.checked_add(amount).ok_or(PerpError::MathOverflow)?;
        let d = &mut ctx.accounts.distributor;
        d.cash_mark = d.cash_mark.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    }
    emit!(LaunchFeesSwept { amount, to_vault });
    Ok(())
}
