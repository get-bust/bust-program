use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

use crate::{
    constants::*,
    error::PerpError,
    events::{BucketWithdrawn, Distributed},
    instructions::trade::read_price,
    math,
    state::*,
};

// ---------- init_distributor / update_distributor (admin)

#[derive(Accounts)]
pub struct InitDistributor<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin, has_one = usdc_mint)]
    pub config: Account<'info, Config>,
    #[account(seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(init, payer = admin, space = 8 + Distributor::INIT_SPACE, seeds = [DISTRIBUTOR_SEED], bump)]
    pub distributor: Account<'info, Distributor>,
    #[account(token::mint = usdc_mint)]
    pub buyback_tokens: Account<'info, TokenAccount>,
    #[account(token::mint = usdc_mint)]
    pub dev_tokens: Account<'info, TokenAccount>,
    pub usdc_mint: Account<'info, Mint>,
    pub system_program: Program<'info, System>,
}

/// The first epoch starts now; vault cash up to this point is not profit (the mark starts at current cash)
pub fn handle_init_distributor(ctx: Context<InitDistributor>, params: DistParams) -> Result<()> {
    require!(params.validate(), PerpError::InvalidConfig);
    ctx.accounts.distributor.set_inner(Distributor {
        params,
        epoch: 0,
        epoch_started_at: Clock::get()?.unix_timestamp,
        cash_mark: ctx.accounts.vault.cash,
        util_area: [0; 7],
        util_day: [i64::MIN; 7],
        util_last: 0,
        util_last_at: Clock::get()?.unix_timestamp,
        burn_usdc: 0,
        staker_usdc: 0,
        xbust_usdc: 0,
        dev_usdc: 0,
        buyback_tokens: ctx.accounts.buyback_tokens.key(),
        dev_tokens: ctx.accounts.dev_tokens.key(),
        epoch_loss: 0,
        bump: ctx.bumps.distributor,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateDistributor<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin, has_one = usdc_mint)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Account<'info, Distributor>,
    #[account(token::mint = usdc_mint)]
    pub buyback_tokens: Account<'info, TokenAccount>,
    #[account(token::mint = usdc_mint)]
    pub dev_tokens: Account<'info, TokenAccount>,
    pub usdc_mint: Account<'info, Mint>,
}

pub fn handle_update_distributor(ctx: Context<UpdateDistributor>, params: DistParams) -> Result<()> {
    require!(params.validate(), PerpError::InvalidConfig);
    let d = &mut ctx.accounts.distributor;
    d.params = params;
    d.buyback_tokens = ctx.accounts.buyback_tokens.key();
    d.dev_tokens = ctx.accounts.dev_tokens.key();
    Ok(())
}

// ---------- record_utilization (anyone; the keeper calls it regularly)

#[derive(Accounts)]
pub struct RecordUtilization<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Account<'info, Distributor>,
    // remaining_accounts: every Market, in index order
}

/// Samples open interest utilisation over every market. It only reads chain state and must be given all markets, so
/// the caller gets no choice; and the sample is weighted by how long it stands, so calling often during a brief spike
/// adds nothing
pub fn handle_record_utilization<'info>(ctx: Context<'info, RecordUtilization<'info>>) -> Result<()> {
    let (vault, params) = (&ctx.accounts.vault, &ctx.accounts.config.params);
    let net = vault.net_equity();
    let cap = math::oi_cap(net, params.oi_cap_ratio_ppm, params.oi_floor);
    let total_cap = math::oi_cap(net, params.oi_total_cap_ratio_ppm, params.oi_total_floor);
    // The busiest market, or all markets together on one side, whichever is closer to its cap
    let mut util = math::utilization(vault.oi_long_total, vault.oi_short_total, total_cap);
    for market in all_markets(ctx.remaining_accounts, &ctx.accounts.config)? {
        util = util.max(math::utilization(market.oi_long, market.oi_short, cap));
    }
    ctx.accounts.distributor.record_util(Clock::get()?.unix_timestamp, util);
    Ok(())
}

// ---------- distribute (anyone; once per epoch)

#[derive(Accounts)]
pub struct Distribute<'info> {
    /// Pays the rent of the closing epoch's record
    #[account(mut)]
    pub caller: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Account<'info, Distributor>,
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = stake_pool.bump)]
    pub stake_pool: Account<'info, StakePool>,
    /// The closing epoch: its total net loss is fixed and xBUST is shared against it
    #[account(
        init,
        payer = caller,
        space = 8 + EpochRecord::INIT_SPACE,
        seeds = [EPOCH_SEED, &distributor.epoch.to_le_bytes()],
        bump,
    )]
    pub epoch_record: Account<'info, EpochRecord>,
    pub system_program: Program<'info, System>,
    // remaining_accounts: every Market in index order, then a fresh Pyth price for each, in the same order
}

/// What open positions are up in total at the latest prices, before haircuts: the sum of each market side's
/// unrealised PnL where it is positive. Sides that are down are not netted against it, which keeps it an upper bound
fn unrealized_profit<'info>(infos: &'info [AccountInfo<'info>], config: &Config) -> Result<u64> {
    let n = config.market_count as usize;
    require!(infos.len() == 2 * n, PerpError::MarketsMismatch);
    let markets = all_markets(&infos[..n], config)?;
    let mut total: u128 = 0;
    for (market, info) in markets.iter().zip(&infos[n..]) {
        let update: Account<PriceUpdateV2> = Account::try_from(info)?;
        let read = read_price(&update, &market.feed_id, &config.params, config.params.max_price_age_sec)?;
        for (side, oi, units) in
            [(Side::Long, market.oi_long, market.long_units), (Side::Short, market.oi_short, market.short_units)]
        {
            total += math::side_pnl(side, read.price, oi, units).max(0) as u128;
        }
    }
    Ok(total.min(u64::MAX as u128) as u64)
}

/// With the queue empty: all fees and the (1 − u) part of profit above the high-water mark are split into four shares.
/// Profit that open positions are sitting on is not the vault's yet, so it is held back until they close.
/// The money stays in the vault token account; only the ledger moves it from the vault to the shares.
pub fn handle_distribute<'info>(ctx: Context<'info, Distribute<'info>>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let unrealized = unrealized_profit(ctx.remaining_accounts, &ctx.accounts.config)?;
    let vault = &mut ctx.accounts.vault;
    let d = &mut ctx.accounts.distributor;
    require!(now >= d.epoch_started_at + d.params.epoch_sec, PerpError::EpochNotOver);
    require!(vault.queue_total == 0, PerpError::QueueNotEmpty);

    let profit = vault.cash.saturating_sub(d.cash_mark).saturating_sub(unrealized);
    d.accrue_util(now);
    let today = now.div_euclid(DAY_SEC);
    let keep = d.peak_util(today).clamp(d.params.keep_min_ppm, d.params.keep_max_ppm);
    let mut s = math::split(profit, vault.fees_accrued, keep, &d.params);

    // The staker share streams over one epoch, together with whatever streamed while nobody was staked (that USDC
    // is already in the staker bucket). With nobody staked now, both go to the burn instead of sitting idle
    let pool = &mut ctx.accounts.stake_pool;
    pool.update(now).ok_or(PerpError::MathOverflow)?;
    let orphaned = pool.take_orphaned();
    if pool.total_staked == 0 {
        s.burn += s.staker + orphaned;
        s.staker = 0;
        d.staker_usdc = d.staker_usdc.checked_sub(orphaned).ok_or(PerpError::MathOverflow)?;
    } else if s.staker + orphaned > 0 {
        pool.notify(s.staker + orphaned, now, d.params.epoch_sec).ok_or(PerpError::MathOverflow)?;
    }

    vault.cash -= s.from_cash;
    vault.fees_accrued = 0;
    d.cash_mark = d.cash_mark.max(vault.cash);
    d.burn_usdc = d.burn_usdc.checked_add(s.burn).ok_or(PerpError::MathOverflow)?;
    d.staker_usdc = d.staker_usdc.checked_add(s.staker).ok_or(PerpError::MathOverflow)?;
    d.xbust_usdc = d.xbust_usdc.checked_add(s.xbust).ok_or(PerpError::MathOverflow)?;
    d.dev_usdc = d.dev_usdc.checked_add(s.dev).ok_or(PerpError::MathOverflow)?;
    ctx.accounts.epoch_record.set_inner(EpochRecord {
        epoch: d.epoch,
        total_loss: d.epoch_loss,
        pool: 0,
        funded: false,
        claimed: 0,
        bump: ctx.bumps.epoch_record,
    });
    d.epoch_loss = 0;
    d.epoch += 1;
    d.epoch_started_at = now;

    emit!(Distributed {
        epoch: d.epoch,
        profit,
        unrealized,
        keep_ppm: keep,
        pool: s.pool,
        burn: s.burn,
        staker: s.staker,
        xbust: s.xbust,
        dev: s.dev,
    });
    Ok(())
}

// ---------- withdraw_bucket (anyone; the money only goes to the registered wallet)

#[derive(Accounts)]
pub struct WithdrawBucket<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Account<'info, Config>,
    #[account(seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, seeds = [VAULT_TOKENS_SEED], bump = vault.tokens_bump)]
    pub vault_tokens: Account<'info, TokenAccount>,
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Account<'info, Distributor>,
    #[account(mut, token::mint = usdc_mint)]
    pub destination: Account<'info, TokenAccount>,
    pub usdc_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

/// Burn and xBUST shares go to the buyback wallet, the developer share to the developer wallet. The staker share stays in the program.
pub fn handle_withdraw_bucket(ctx: Context<WithdrawBucket>, bucket: Bucket) -> Result<()> {
    let d = &mut ctx.accounts.distributor;
    let destination = ctx.accounts.destination.key();
    let amount = match bucket {
        Bucket::Burn | Bucket::Xbust => {
            require_keys_eq!(destination, d.buyback_tokens, PerpError::InvalidDestination);
            let slot = if bucket == Bucket::Burn { &mut d.burn_usdc } else { &mut d.xbust_usdc };
            std::mem::take(slot)
        }
        Bucket::Dev => {
            require_keys_eq!(destination, d.dev_tokens, PerpError::InvalidDestination);
            std::mem::take(&mut d.dev_usdc)
        }
        Bucket::Staker => return err!(PerpError::InvalidDestination),
    };
    require!(amount > 0, PerpError::NothingToWithdraw);

    let bump = [ctx.accounts.vault.bump];
    let seeds: &[&[&[u8]]] = &[&[VAULT_SEED, &bump]];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.vault_tokens.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.destination.to_account_info(),
                authority: ctx.accounts.vault.to_account_info(),
            },
            seeds,
        ),
        amount,
        ctx.accounts.usdc_mint.decimals,
    )?;
    emit!(BucketWithdrawn { bucket, amount, destination });
    Ok(())
}
