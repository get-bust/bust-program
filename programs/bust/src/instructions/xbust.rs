use anchor_lang::prelude::*;
use anchor_spl::token::{burn, mint_to, transfer_checked, Burn, Mint, MintTo, Token, TokenAccount, TransferChecked};

use crate::{
    constants::*,
    error::PerpError,
    events::{EpochFundedEvent, XbustClaimed, XbustConverted, XbustLocked},
    state::*,
};

// ---------- init_treasury / update_treasury (admin)

#[derive(Accounts)]
pub struct InitTreasury<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin)]
    pub config: Account<'info, Config>,
    pub bust_mint: Account<'info, Mint>,
    #[account(init, payer = admin, space = 8 + Treasury::INIT_SPACE, seeds = [TREASURY_SEED], bump)]
    pub treasury: Account<'info, Treasury>,
    #[account(
        init,
        payer = admin,
        seeds = [TREASURY_TOKENS_SEED],
        bump,
        token::mint = bust_mint,
        token::authority = treasury,
    )]
    pub treasury_tokens: Account<'info, TokenAccount>,
    /// Classic SPL; the treasury PDA is the only mint authority, and there is no freeze authority
    #[account(
        init,
        payer = admin,
        seeds = [XBUST_MINT_SEED],
        bump,
        mint::decimals = bust_mint.decimals,
        mint::authority = treasury,
    )]
    pub xbust_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handle_init_treasury(ctx: Context<InitTreasury>, vest_sec: i64, buyback_authority: Pubkey) -> Result<()> {
    require!(vest_sec > 0, PerpError::InvalidConfig);
    ctx.accounts.treasury.set_inner(Treasury {
        xbust_mint: ctx.accounts.xbust_mint.key(),
        bust_mint: ctx.accounts.bust_mint.key(),
        buyback_authority,
        total_locked: 0,
        vest_sec,
        bump: ctx.bumps.treasury,
        tokens_bump: ctx.bumps.treasury_tokens,
        mint_bump: ctx.bumps.xbust_mint,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateTreasury<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [TREASURY_SEED], bump = treasury.bump)]
    pub treasury: Account<'info, Treasury>,
}

pub fn handle_update_treasury(ctx: Context<UpdateTreasury>, buyback_authority: Pubkey) -> Result<()> {
    ctx.accounts.treasury.buyback_authority = buyback_authority;
    Ok(())
}

// ---------- fund_epoch (buyback authority only, once per epoch)

#[derive(Accounts)]
#[instruction(epoch: u64)]
pub struct FundEpoch<'info> {
    pub authority: Signer<'info>,
    #[account(seeds = [TREASURY_SEED], bump = treasury.bump, has_one = bust_mint)]
    pub treasury: Account<'info, Treasury>,
    #[account(mut, seeds = [TREASURY_TOKENS_SEED], bump = treasury.tokens_bump)]
    pub treasury_tokens: Account<'info, TokenAccount>,
    #[account(mut, seeds = [EPOCH_SEED, &epoch.to_le_bytes()], bump = epoch_record.bump)]
    pub epoch_record: Account<'info, EpochRecord>,
    #[account(mut, token::mint = bust_mint, token::authority = authority)]
    pub authority_bust: Account<'info, TokenAccount>,
    pub bust_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

/// The $BUST bought with the epoch's xBUST share enters the treasury; the same amount of xBUST can be minted for that epoch.
/// If anyone could fund an epoch, they could fund it with one base unit and lock the losers out of their share.
pub fn handle_fund_epoch(ctx: Context<FundEpoch>, _epoch: u64, amount: u64) -> Result<()> {
    require_keys_eq!(ctx.accounts.authority.key(), ctx.accounts.treasury.buyback_authority, PerpError::NotBuybackAuthority);
    require!(amount > 0, PerpError::ZeroAmount);
    let record = &mut ctx.accounts.epoch_record;
    require!(!record.funded, PerpError::EpochFunded);
    require!(record.total_loss > 0, PerpError::NothingToWithdraw);

    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.authority_bust.to_account_info(),
                mint: ctx.accounts.bust_mint.to_account_info(),
                to: ctx.accounts.treasury_tokens.to_account_info(),
                authority: ctx.accounts.authority.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.bust_mint.decimals,
    )?;
    record.pool = amount;
    record.funded = true;
    emit!(EpochFundedEvent { epoch: record.epoch, bust: amount, total_loss: record.total_loss });
    Ok(())
}

// ---------- claim_xbust (anyone can trigger it; the xBUST only goes to the loser's account)

#[derive(Accounts)]
#[instruction(epoch: u64)]
pub struct ClaimXbust<'info> {
    pub caller: Signer<'info>,
    #[account(seeds = [TREASURY_SEED], bump = treasury.bump, has_one = xbust_mint)]
    pub treasury: Account<'info, Treasury>,
    #[account(mut, seeds = [EPOCH_SEED, &epoch.to_le_bytes()], bump = epoch_record.bump)]
    pub epoch_record: Account<'info, EpochRecord>,
    #[account(
        mut,
        close = rent_payer,
        seeds = [EPOCH_LOSS_SEED, epoch_loss.owner.as_ref(), &epoch.to_le_bytes()],
        bump = epoch_loss.bump,
    )]
    pub epoch_loss: Account<'info, EpochLoss>,
    /// CHECK: Paid the record's rent (address stored on the record) and gets it back
    #[account(mut, address = epoch_loss.rent_payer)]
    pub rent_payer: UncheckedAccount<'info>,
    #[account(mut)]
    pub xbust_mint: Account<'info, Mint>,
    #[account(mut, token::mint = xbust_mint, token::authority = epoch_loss.owner)]
    pub owner_xbust: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

/// Share = loss / the epoch's total loss × pool (rounded down). A user who ended the epoch up just has their record closed
pub fn handle_claim_xbust(ctx: Context<ClaimXbust>, _epoch: u64) -> Result<()> {
    let record = &mut ctx.accounts.epoch_record;
    require!(record.funded, PerpError::EpochNotFunded);
    let loss = ctx.accounts.epoch_loss.loss();
    let amount = if loss == 0 { 0 } else { (loss as u128 * record.pool as u128 / record.total_loss as u128) as u64 };
    record.claimed = record.claimed.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    require!(record.claimed <= record.pool, PerpError::MathOverflow);
    if amount > 0 {
        let bump = [ctx.accounts.treasury.bump];
        let seeds: &[&[&[u8]]] = &[&[TREASURY_SEED, &bump]];
        mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                MintTo {
                    mint: ctx.accounts.xbust_mint.to_account_info(),
                    to: ctx.accounts.owner_xbust.to_account_info(),
                    authority: ctx.accounts.treasury.to_account_info(),
                },
                seeds,
            ),
            amount,
        )?;
    }
    emit!(XbustClaimed { owner: ctx.accounts.epoch_loss.owner, epoch: record.epoch, loss, amount });
    Ok(())
}

// ---------- lock_xbust / convert_vested (owner signature only)

#[derive(Accounts)]
pub struct LockXbust<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut, seeds = [TREASURY_SEED], bump = treasury.bump, has_one = xbust_mint)]
    pub treasury: Account<'info, Treasury>,
    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + Vest::INIT_SPACE,
        seeds = [VEST_SEED, owner.key().as_ref()],
        bump,
    )]
    pub vest: Account<'info, Vest>,
    #[account(mut)]
    pub xbust_mint: Account<'info, Mint>,
    #[account(mut, token::mint = xbust_mint, token::authority = owner)]
    pub owner_xbust: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// The xBUST is burned and added to the lock. What is left merges with the new amount and the clock restarts; the unlocked part is kept (carried)
pub fn handle_lock_xbust(ctx: Context<LockXbust>, amount: u64) -> Result<()> {
    require!(amount > 0, PerpError::ZeroAmount);
    burn(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            Burn {
                mint: ctx.accounts.xbust_mint.to_account_info(),
                from: ctx.accounts.owner_xbust.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        amount,
    )?;
    let now = Clock::get()?.unix_timestamp;
    let vest_sec = ctx.accounts.treasury.vest_sec;
    let vest = &mut ctx.accounts.vest;
    if vest.owner == Pubkey::default() {
        vest.owner = ctx.accounts.owner.key();
        vest.bump = ctx.bumps.vest;
    }
    let vested = vest.vested(now, vest_sec);
    vest.carried = vest.carried.checked_add(vested - vest.converted).ok_or(PerpError::MathOverflow)?;
    vest.amount = (vest.amount - vested).checked_add(amount).ok_or(PerpError::MathOverflow)?;
    vest.start = now;
    vest.converted = 0;
    let treasury = &mut ctx.accounts.treasury;
    treasury.total_locked = treasury.total_locked.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    emit!(XbustLocked { owner: vest.owner, amount, locked: vest.amount + vest.carried });
    Ok(())
}

#[derive(Accounts)]
pub struct ConvertVested<'info> {
    pub owner: Signer<'info>,
    #[account(mut, seeds = [TREASURY_SEED], bump = treasury.bump, has_one = xbust_mint, has_one = bust_mint)]
    pub treasury: Account<'info, Treasury>,
    #[account(mut, seeds = [TREASURY_TOKENS_SEED], bump = treasury.tokens_bump)]
    pub treasury_tokens: Account<'info, TokenAccount>,
    #[account(mut, seeds = [VEST_SEED, owner.key().as_ref()], bump = vest.bump, has_one = owner)]
    pub vest: Account<'info, Vest>,
    pub xbust_mint: Account<'info, Mint>,
    pub bust_mint: Account<'info, Mint>,
    #[account(mut, token::mint = bust_mint, token::authority = owner)]
    pub owner_bust: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

/// Unlocked xBUST becomes $BUST at that day's rate: treasury $BUST / (xBUST supply + locked).
/// xBUST burned at the wall lowers the supply, so the rate rises for those who wait.
pub fn handle_convert_vested(ctx: Context<ConvertVested>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let treasury = &mut ctx.accounts.treasury;
    let vest = &mut ctx.accounts.vest;
    let xbust = vest.convertible(now, treasury.vest_sec);
    require!(xbust > 0, PerpError::NothingToWithdraw);

    let outstanding = ctx.accounts.xbust_mint.supply as u128 + treasury.total_locked as u128;
    let bust = (xbust as u128 * ctx.accounts.treasury_tokens.amount as u128 / outstanding) as u64;
    vest.converted = vest.vested(now, treasury.vest_sec);
    vest.carried = 0;
    treasury.total_locked -= xbust;

    if bust > 0 {
        let bump = [treasury.bump];
        let seeds: &[&[&[u8]]] = &[&[TREASURY_SEED, &bump]];
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.treasury_tokens.to_account_info(),
                    mint: ctx.accounts.bust_mint.to_account_info(),
                    to: ctx.accounts.owner_bust.to_account_info(),
                    authority: treasury.to_account_info(),
                },
                seeds,
            ),
            bust,
            ctx.accounts.bust_mint.decimals,
        )?;
    }
    emit!(XbustConverted { owner: vest.owner, xbust, bust });
    Ok(())
}
