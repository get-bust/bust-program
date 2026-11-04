use anchor_lang::prelude::*;
use pyth_solana_receiver_sdk::price_update::{PriceUpdateV2, VerificationLevel};

use crate::{
    constants::*,
    error::PerpError,
    events::{OrderCancelled, OrderRequested, PositionClosed, PositionOpened},
    math,
    state::*,
};

pub struct PriceRead {
    /// 1e-8 scale
    pub price: u64,
    pub publish_time: i64,
}

/// Checks a Pyth price (age, signature count under partial verification, confidence) and rescales it to 1e-8
#[allow(clippy::manual_range_contains)]
pub fn read_price(update: &PriceUpdateV2, feed_id: &[u8; 32], params: &Params) -> Result<PriceRead> {
    let level = VerificationLevel::Partial { num_signatures: params.min_price_signatures };
    let price = update.get_price_no_older_than_with_custom_verification_level(
        &Clock::get()?,
        params.max_price_age_sec,
        feed_id,
        level,
    )?;
    require!(price.price > 0, PerpError::InvalidPrice);
    let to_scale = |v: u128| -> Result<u128> {
        let shift = price.exponent - PRICE_EXPONENT;
        match shift {
            0 => Ok(v),
            s if s > 0 && s <= 18 => v.checked_mul(10u128.pow(s as u32)).ok_or(error!(PerpError::InvalidPrice)),
            s if s < 0 && s >= -18 => Ok(v / 10u128.pow((-s) as u32)),
            _ => err!(PerpError::InvalidPrice),
        }
    };
    let value = to_scale(price.price as u128)?;
    let conf = to_scale(price.conf as u128)?;
    require!(value > 0 && value <= u64::MAX as u128, PerpError::InvalidPrice);
    require!(conf * PPM <= value * params.max_conf_ppm as u128, PerpError::PriceConfidence);
    Ok(PriceRead { price: value as u64, publish_time: price.publish_time })
}

/// The fill price must be published AFTER the request and within max_fill_delay_sec of it.
/// The user cannot pick a stale price, and the keeper cannot pick one outside the window.
fn check_fill_window(read: &PriceRead, requested_at: i64, params: &Params) -> Result<()> {
    require!(read.publish_time > requested_at, PerpError::PriceBeforeRequest);
    require!(read.publish_time <= requested_at + params.max_fill_delay_sec, PerpError::PriceTooLate);
    Ok(())
}

fn order_space() -> usize {
    8 + OrderRequest::INIT_SPACE
}

// ---------- request_open

#[derive(Accounts)]
pub struct RequestOpen<'info> {
    /// The owner or an active session key
    pub authority: Signer<'info>,
    /// Pays the order account's rent (relayer or user)
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(seeds = [MARKET_SEED, &market.index.to_le_bytes()], bump = market.bump)]
    pub market: Account<'info, Market>,
    #[account(mut, seeds = [USER_SEED, user.owner.as_ref()], bump = user.bump)]
    pub user: Account<'info, UserAccount>,
    #[account(
        init,
        payer = payer,
        space = order_space(),
        seeds = [ORDER_SEED, user.owner.as_ref(), &user.next_order_id.to_le_bytes()],
        bump,
    )]
    pub order: Account<'info, OrderRequest>,
    pub system_program: Program<'info, System>,
}

/// Margin is locked right away (balance → reserved); the price is set at fill time
pub fn handle_request_open(ctx: Context<RequestOpen>, side: Side, margin: u64, leverage: u16) -> Result<()> {
    let params = &ctx.accounts.config.params;
    let now = Clock::get()?.unix_timestamp;
    require!(ctx.accounts.user.can_trade(&ctx.accounts.authority.key(), now), PerpError::Unauthorized);
    require!(leverage >= 1 && leverage <= params.max_leverage, PerpError::InvalidLeverage);
    require!(margin >= params.min_margin, PerpError::MarginTooSmall);

    let user = &mut ctx.accounts.user;
    user.balance = user.balance.checked_sub(margin).ok_or(PerpError::InsufficientBalance)?;
    user.reserved = user.reserved.checked_add(margin).ok_or(PerpError::MathOverflow)?;
    let id = user.next_order_id;
    user.next_order_id += 1;

    ctx.accounts.order.set_inner(OrderRequest {
        owner: user.owner,
        id,
        kind: OrderKind::Open,
        market: ctx.accounts.market.key(),
        position: Pubkey::default(),
        side,
        margin,
        leverage,
        requested_at: now,
        rent_payer: ctx.accounts.payer.key(),
        bump: ctx.bumps.order,
    });
    emit!(OrderRequested { owner: user.owner, order: ctx.accounts.order.key(), kind: OrderKind::Open, requested_at: now });
    Ok(())
}

// ---------- request_close

#[derive(Accounts)]
pub struct RequestClose<'info> {
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [USER_SEED, position.owner.as_ref()], bump = user.bump)]
    pub user: Account<'info, UserAccount>,
    #[account(seeds = [POSITION_SEED, position.owner.as_ref(), &position.id.to_le_bytes()], bump = position.bump)]
    pub position: Account<'info, Position>,
    #[account(
        init,
        payer = payer,
        space = order_space(),
        seeds = [ORDER_SEED, user.owner.as_ref(), &user.next_order_id.to_le_bytes()],
        bump,
    )]
    pub order: Account<'info, OrderRequest>,
    pub system_program: Program<'info, System>,
}

pub fn handle_request_close(ctx: Context<RequestClose>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(ctx.accounts.user.can_trade(&ctx.accounts.authority.key(), now), PerpError::Unauthorized);
    let user = &mut ctx.accounts.user;
    let position = &ctx.accounts.position;
    let id = user.next_order_id;
    user.next_order_id += 1;
    ctx.accounts.order.set_inner(OrderRequest {
        owner: user.owner,
        id,
        kind: OrderKind::Close,
        market: position.market,
        position: position.key(),
        side: position.side,
        margin: 0,
        leverage: position.leverage,
        requested_at: now,
        rent_payer: ctx.accounts.payer.key(),
        bump: ctx.bumps.order,
    });
    emit!(OrderRequested { owner: user.owner, order: ctx.accounts.order.key(), kind: OrderKind::Close, requested_at: now });
    Ok(())
}

// ---------- execute_open (keeper)

#[derive(Accounts)]
pub struct ExecuteOpen<'info> {
    /// Anyone can fill; pays the position account's rent
    #[account(mut)]
    pub keeper: Signer<'info>,
    /// CHECK: Paid the order's rent and gets it back (address stored on the order)
    #[account(mut, address = order.rent_payer)]
    pub rent_payer: UncheckedAccount<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, address = order.market)]
    pub market: Account<'info, Market>,
    #[account(mut, seeds = [USER_SEED, order.owner.as_ref()], bump = user.bump)]
    pub user: Account<'info, UserAccount>,
    #[account(
        mut,
        close = rent_payer,
        seeds = [ORDER_SEED, order.owner.as_ref(), &order.id.to_le_bytes()],
        bump = order.bump,
        constraint = order.kind == OrderKind::Open @ PerpError::OrderMismatch,
    )]
    pub order: Account<'info, OrderRequest>,
    #[account(
        init,
        payer = keeper,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, order.owner.as_ref(), &user.next_position_id.to_le_bytes()],
        bump,
    )]
    pub position: Account<'info, Position>,
    pub price_update: Account<'info, PriceUpdateV2>,
    pub system_program: Program<'info, System>,
}

pub fn handle_execute_open(ctx: Context<ExecuteOpen>) -> Result<()> {
    let params = &ctx.accounts.config.params;
    let order = &ctx.accounts.order;

    let read = read_price(&ctx.accounts.price_update, &ctx.accounts.market.feed_id, params)?;
    check_fill_window(&read, order.requested_at, params)?;
    let (side, margin, leverage) = (order.side, order.margin, order.leverage);
    let notional = margin.checked_mul(leverage as u64).ok_or(PerpError::MathOverflow)?;

    let market = &mut ctx.accounts.market;
    let oi = match side {
        Side::Long => &mut market.oi_long,
        Side::Short => &mut market.oi_short,
    };
    *oi = oi.checked_add(notional).ok_or(PerpError::MathOverflow)?;

    let user = &mut ctx.accounts.user;
    user.reserved = user.reserved.checked_sub(margin).ok_or(PerpError::MathOverflow)?;
    let vault = &mut ctx.accounts.vault;
    vault.user_balances = vault.user_balances.checked_sub(margin).ok_or(PerpError::MathOverflow)?;
    vault.open_margin = vault.open_margin.checked_add(margin).ok_or(PerpError::MathOverflow)?;

    let id = user.next_position_id;
    user.next_position_id += 1;
    let bust_price = math::bust_price(side, read.price, leverage, params.liq_buffer_ppm);
    let now = Clock::get()?.unix_timestamp;
    ctx.accounts.position.set_inner(Position {
        owner: user.owner,
        id,
        market: market.key(),
        side,
        margin,
        leverage,
        notional,
        entry_price: read.price,
        bust_price,
        opened_at: now,
        rent_payer: ctx.accounts.keeper.key(),
        bump: ctx.bumps.position,
    });

    emit!(PositionOpened {
        owner: user.owner,
        position: ctx.accounts.position.key(),
        market: market.key(),
        side,
        margin,
        leverage,
        notional,
        entry_price: read.price,
        bust_price,
    });
    Ok(())
}

// ---------- settle

struct Settlement {
    liquidated: bool,
    raw_pnl: i128,
    paid: u64,
    fee: u64,
}

fn settle(
    params: &Params,
    vault: &mut Vault,
    market: &mut Market,
    user: &mut UserAccount,
    position: &Position,
    exit: u64,
    liquidate: bool,
) -> Result<Settlement> {
    let oi = match position.side {
        Side::Long => &mut market.oi_long,
        Side::Short => &mut market.oi_short,
    };
    *oi = oi.saturating_sub(position.notional);
    vault.open_margin = vault.open_margin.checked_sub(position.margin).ok_or(PerpError::MathOverflow)?;

    let raw = math::raw_pnl(position.side, position.entry_price, exit, position.notional);
    let margin = position.margin;
    let mut s = Settlement { liquidated: false, raw_pnl: raw, paid: 0, fee: 0 };

    if liquidate || math::is_bust(position.side, exit, position.bust_price) {
        // Hard bust: the whole margin goes to the vault
        s.liquidated = true;
        s.fee = math::fee_of(margin, params.loss_fee_ppm);
        vault.cash = vault.cash.checked_add(margin - s.fee).ok_or(PerpError::MathOverflow)?;
    } else if raw < 0 {
        let loss = ((-raw) as u64).min(margin);
        s.fee = math::fee_of(loss, params.loss_fee_ppm);
        vault.cash = vault.cash.checked_add(loss - s.fee).ok_or(PerpError::MathOverflow)?;
        s.paid = margin - loss;
    } else {
        // Win: the margin always comes back, the profit comes from vault cash. A short vault pays what it has, without a fee
        let win = math::win_breakdown(position.entry_price, exit, position.notional, market.position_multiplier_milli, params);
        let profit = if vault.cash >= win.gain {
            vault.cash -= win.gain;
            s.fee = win.fee;
            win.net
        } else {
            let partial = vault.cash.min(win.net);
            vault.cash -= partial;
            partial
        };
        s.paid = margin + profit;
    }

    vault.fees_accrued = vault.fees_accrued.checked_add(s.fee).ok_or(PerpError::MathOverflow)?;
    user.balance = user.balance.checked_add(s.paid).ok_or(PerpError::MathOverflow)?;
    vault.user_balances = vault.user_balances.checked_add(s.paid).ok_or(PerpError::MathOverflow)?;
    Ok(s)
}

fn emit_closed(position: &Account<Position>, exit: u64, s: &Settlement) {
    emit!(PositionClosed {
        owner: position.owner,
        position: position.key(),
        market: position.market,
        liquidated: s.liquidated,
        exit_price: exit,
        raw_pnl: s.raw_pnl as i64,
        paid: s.paid,
        fee: s.fee,
    });
}

// ---------- execute_close (keeper)

#[derive(Accounts)]
pub struct ExecuteClose<'info> {
    pub keeper: Signer<'info>,
    /// CHECK: Paid the order's rent (address stored on the order)
    #[account(mut, address = order.rent_payer)]
    pub order_rent_payer: UncheckedAccount<'info>,
    /// CHECK: Paid the position's rent (address stored on the position)
    #[account(mut, address = position.rent_payer)]
    pub position_rent_payer: UncheckedAccount<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, address = position.market)]
    pub market: Account<'info, Market>,
    #[account(mut, seeds = [USER_SEED, position.owner.as_ref()], bump = user.bump)]
    pub user: Account<'info, UserAccount>,
    #[account(
        mut,
        close = order_rent_payer,
        seeds = [ORDER_SEED, order.owner.as_ref(), &order.id.to_le_bytes()],
        bump = order.bump,
        constraint = order.kind == OrderKind::Close && order.position == position.key() @ PerpError::OrderMismatch,
    )]
    pub order: Account<'info, OrderRequest>,
    #[account(
        mut,
        close = position_rent_payer,
        seeds = [POSITION_SEED, position.owner.as_ref(), &position.id.to_le_bytes()],
        bump = position.bump,
    )]
    pub position: Account<'info, Position>,
    pub price_update: Account<'info, PriceUpdateV2>,
}

pub fn handle_execute_close(ctx: Context<ExecuteClose>) -> Result<()> {
    let params = ctx.accounts.config.params;
    let read = read_price(&ctx.accounts.price_update, &ctx.accounts.market.feed_id, &params)?;
    check_fill_window(&read, ctx.accounts.order.requested_at, &params)?;
    let s = settle(
        &params,
        &mut ctx.accounts.vault,
        &mut ctx.accounts.market,
        &mut ctx.accounts.user,
        &ctx.accounts.position,
        read.price,
        false,
    )?;
    emit_closed(&ctx.accounts.position, read.price, &s);
    Ok(())
}

// ---------- cancel_order (anyone, once the order has timed out)

#[derive(Accounts)]
pub struct CancelOrder<'info> {
    pub caller: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    /// CHECK: Paid the order's rent (address stored on the order)
    #[account(mut, address = order.rent_payer)]
    pub rent_payer: UncheckedAccount<'info>,
    #[account(mut, seeds = [USER_SEED, order.owner.as_ref()], bump = user.bump)]
    pub user: Account<'info, UserAccount>,
    #[account(
        mut,
        close = rent_payer,
        seeds = [ORDER_SEED, order.owner.as_ref(), &order.id.to_le_bytes()],
        bump = order.bump,
    )]
    pub order: Account<'info, OrderRequest>,
}

/// Cancels an unfilled order and refunds the locked margin. Not before the timeout:
/// otherwise a user could see the price and pull the order if they do not like it (a free option).
pub fn handle_cancel_order(ctx: Context<CancelOrder>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let order = &ctx.accounts.order;
    require!(now > order.requested_at + ctx.accounts.config.params.order_timeout_sec, PerpError::OrderNotExpired);
    if order.kind == OrderKind::Open {
        let user = &mut ctx.accounts.user;
        user.reserved = user.reserved.checked_sub(order.margin).ok_or(PerpError::MathOverflow)?;
        user.balance = user.balance.checked_add(order.margin).ok_or(PerpError::MathOverflow)?;
    }
    emit!(OrderCancelled { owner: order.owner, order: order.key() });
    Ok(())
}

// ---------- liquidate (anyone, at the latest price)

#[derive(Accounts)]
pub struct Liquidate<'info> {
    #[account(mut)]
    pub keeper: Signer<'info>,
    /// CHECK: Paid the position's rent and gets it back (address stored on the position)
    #[account(mut, address = position.rent_payer)]
    pub rent_payer: UncheckedAccount<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, address = position.market)]
    pub market: Account<'info, Market>,
    #[account(mut, seeds = [USER_SEED, position.owner.as_ref()], bump = user.bump)]
    pub user: Account<'info, UserAccount>,
    #[account(
        mut,
        close = rent_payer,
        seeds = [POSITION_SEED, position.owner.as_ref(), &position.id.to_le_bytes()],
        bump = position.bump,
    )]
    pub position: Account<'info, Position>,
    pub price_update: Account<'info, PriceUpdateV2>,
}

pub fn handle_liquidate(ctx: Context<Liquidate>) -> Result<()> {
    let params = ctx.accounts.config.params;
    let read = read_price(&ctx.accounts.price_update, &ctx.accounts.market.feed_id, &params)?;
    let position = &ctx.accounts.position;
    require!(math::is_bust(position.side, read.price, position.bust_price), PerpError::NotLiquidatable);
    let s =
        settle(&params, &mut ctx.accounts.vault, &mut ctx.accounts.market, &mut ctx.accounts.user, position, read.price, true)?;
    emit_closed(&ctx.accounts.position, read.price, &s);
    Ok(())
}
