use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};

use crate::{
    constants::*,
    error::PerpError,
    events::{RewardsClaimed, Staked, UnstakeRequested},
    state::*,
};

// ---------- init_stake_pool (admin)

#[derive(Accounts)]
pub struct InitStakePool<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin)]
    pub config: Account<'info, Config>,
    pub bust_mint: Account<'info, Mint>,
    #[account(init, payer = admin, space = 8 + StakePool::INIT_SPACE, seeds = [STAKE_POOL_SEED], bump)]
    pub stake_pool: Account<'info, StakePool>,
    #[account(
        init,
        payer = admin,
        seeds = [STAKE_TOKENS_SEED],
        bump,
        token::mint = bust_mint,
        token::authority = stake_pool,
    )]
    pub stake_tokens: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handle_init_stake_pool(ctx: Context<InitStakePool>, unstake_sec: i64) -> Result<()> {
    require!(unstake_sec > 0, PerpError::InvalidConfig);
    let now = Clock::get()?.unix_timestamp;
    ctx.accounts.stake_pool.set_inner(StakePool {
        bust_mint: ctx.accounts.bust_mint.key(),
        total_staked: 0,
        reward_rate: 0,
        period_finish: now,
        last_update: now,
        reward_per_token: 0,
        unstake_sec,
        bump: ctx.bumps.stake_pool,
        tokens_bump: ctx.bumps.stake_tokens,
    });
    Ok(())
}

// ---------- create_stake_account

#[derive(Accounts)]
pub struct CreateStakeAccount<'info> {
    pub owner: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + StakeAccount::INIT_SPACE,
        seeds = [STAKE_SEED, owner.key().as_ref()],
        bump,
    )]
    pub stake_account: Account<'info, StakeAccount>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_stake_account(ctx: Context<CreateStakeAccount>) -> Result<()> {
    ctx.accounts.stake_account.set_inner(StakeAccount {
        owner: ctx.accounts.owner.key(),
        staked: 0,
        reward_per_token_paid: 0,
        rewards: 0,
        pending_unstake: 0,
        unstake_at: 0,
        bump: ctx.bumps.stake_account,
    });
    Ok(())
}

// ---------- stake / request_unstake / withdraw_unstaked (owner signature only)

#[derive(Accounts)]
pub struct MoveStake<'info> {
    pub owner: Signer<'info>,
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = stake_pool.bump, has_one = bust_mint)]
    pub stake_pool: Account<'info, StakePool>,
    #[account(mut, seeds = [STAKE_TOKENS_SEED], bump = stake_pool.tokens_bump)]
    pub stake_tokens: Account<'info, TokenAccount>,
    #[account(mut, seeds = [STAKE_SEED, owner.key().as_ref()], bump = stake_account.bump, has_one = owner)]
    pub stake_account: Account<'info, StakeAccount>,
    #[account(mut, token::mint = bust_mint, token::authority = owner)]
    pub owner_tokens: Account<'info, TokenAccount>,
    pub bust_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

fn sync(pool: &mut StakePool, account: &mut StakeAccount) -> Result<()> {
    pool.update(Clock::get()?.unix_timestamp).ok_or(PerpError::MathOverflow)?;
    account.settle(pool).ok_or(PerpError::MathOverflow)?;
    Ok(())
}

pub fn handle_stake(ctx: Context<MoveStake>, amount: u64) -> Result<()> {
    require!(amount > 0, PerpError::ZeroAmount);
    let pool = &mut ctx.accounts.stake_pool;
    let account = &mut ctx.accounts.stake_account;
    sync(pool, account)?;
    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.owner_tokens.to_account_info(),
                mint: ctx.accounts.bust_mint.to_account_info(),
                to: ctx.accounts.stake_tokens.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.bust_mint.decimals,
    )?;
    account.staked = account.staked.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    pool.total_staked = pool.total_staked.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    emit!(Staked { owner: account.owner, amount, total_staked: pool.total_staked });
    Ok(())
}

/// Rewards stop right away; the $BUST can be withdrawn after unstake_sec. A new request restarts the wait
pub fn handle_request_unstake(ctx: Context<MoveStake>, amount: u64) -> Result<()> {
    require!(amount > 0, PerpError::ZeroAmount);
    let pool = &mut ctx.accounts.stake_pool;
    let account = &mut ctx.accounts.stake_account;
    sync(pool, account)?;
    account.staked = account.staked.checked_sub(amount).ok_or(PerpError::InsufficientBalance)?;
    pool.total_staked = pool.total_staked.checked_sub(amount).ok_or(PerpError::MathOverflow)?;
    account.pending_unstake = account.pending_unstake.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    account.unstake_at = Clock::get()?.unix_timestamp.checked_add(pool.unstake_sec).ok_or(PerpError::MathOverflow)?;
    emit!(UnstakeRequested { owner: account.owner, amount, available_at: account.unstake_at });
    Ok(())
}

pub fn handle_withdraw_unstaked(ctx: Context<MoveStake>) -> Result<()> {
    let account = &mut ctx.accounts.stake_account;
    let amount = account.pending_unstake;
    require!(amount > 0, PerpError::NothingToWithdraw);
    require!(Clock::get()?.unix_timestamp >= account.unstake_at, PerpError::UnstakeNotReady);
    account.pending_unstake = 0;

    let bump = [ctx.accounts.stake_pool.bump];
    let seeds: &[&[&[u8]]] = &[&[STAKE_POOL_SEED, &bump]];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.stake_tokens.to_account_info(),
                mint: ctx.accounts.bust_mint.to_account_info(),
                to: ctx.accounts.owner_tokens.to_account_info(),
                authority: ctx.accounts.stake_pool.to_account_info(),
            },
            seeds,
        ),
        amount,
        ctx.accounts.bust_mint.decimals,
    )
}

// ---------- claim_rewards (owner signature; USDC from the vault token account)

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
    pub owner: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Account<'info, Config>,
    #[account(seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, seeds = [VAULT_TOKENS_SEED], bump = vault.tokens_bump)]
    pub vault_tokens: Account<'info, TokenAccount>,
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Box<Account<'info, Distributor>>,
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = stake_pool.bump)]
    pub stake_pool: Box<Account<'info, StakePool>>,
    #[account(mut, seeds = [STAKE_SEED, owner.key().as_ref()], bump = stake_account.bump, has_one = owner)]
    pub stake_account: Account<'info, StakeAccount>,
    #[account(mut, token::mint = usdc_mint, token::authority = owner)]
    pub owner_usdc: Account<'info, TokenAccount>,
    pub usdc_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

pub fn handle_claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
    let pool = &mut ctx.accounts.stake_pool;
    let account = &mut ctx.accounts.stake_account;
    sync(pool, account)?;
    let amount = std::mem::take(&mut account.rewards);
    require!(amount > 0, PerpError::NothingToWithdraw);
    // the streamed reward cannot exceed the USDC set aside for stakers (rounded down)
    let d = &mut ctx.accounts.distributor;
    d.staker_usdc = d.staker_usdc.checked_sub(amount).ok_or(PerpError::MathOverflow)?;

    let bump = [ctx.accounts.vault.bump];
    let seeds: &[&[&[u8]]] = &[&[VAULT_SEED, &bump]];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.vault_tokens.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.owner_usdc.to_account_info(),
                authority: ctx.accounts.vault.to_account_info(),
            },
            seeds,
        ),
        amount,
        ctx.accounts.usdc_mint.decimals,
    )?;
    emit!(RewardsClaimed { owner: account.owner, amount });
    Ok(())
}
