use anchor_lang::{
    prelude::*,
    system_program::{self as sys, Allocate, Assign, CreateAccount, Transfer},
};
use pyth_solana_receiver_sdk::price_update::{PriceUpdateV2, VerificationLevel};

use crate::{
    constants::*,
    error::PerpError,
    events::{OrderCancelled, OrderRequested, PositionClosed, PositionOpened, TicketOpened},
    math,
    state::*,
};

pub struct PriceRead {
    /// 1e-8 scale
    pub price: u64,
    pub publish_time: i64,
    /// Publish time of the update before this one on the same feed (signed with it)
    pub prev_publish_time: i64,
    /// Confidence interval, 1e-8 scale
    pub conf: u64,
}

/// A fill price is fixed by the order, not by when it is read: any age is fine
const FILL_PRICE_ANY_AGE: u64 = i64::MAX as u64;

/// Checks a Pyth price (age, signature count under partial verification, confidence) and rescales it to 1e-8
#[allow(clippy::manual_range_contains)]
pub fn read_price(update: &PriceUpdateV2, feed_id: &[u8; 32], params: &Params, max_age_sec: u64) -> Result<PriceRead> {
    let level = VerificationLevel::Partial { num_signatures: params.min_price_signatures };
    let price = update.get_price_no_older_than_with_custom_verification_level(&Clock::get()?, max_age_sec, feed_id, level)?;
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
    Ok(PriceRead {
        price: value as u64,
        publish_time: price.publish_time,
        prev_publish_time: update.price_message.prev_publish_time,
        conf: conf as u64,
    })
}

/// An order fills at exactly one price: the first update on the feed published at or after its fill time
/// T = requested_at + fill_delay_sec, i.e. publish_time ≥ T and prev_publish_time < T. Pyth signs prev_publish_time
/// with each update, so the message is unique and cannot be forged. A user running their own keeper therefore cannot
/// pick the best of several updates, and cannot skip a bad one either: any keeper can fetch the same message from
/// Hermes later and fill with it. The delay keeps T past what the user could have seen when the request landed,
/// even if the chain clock runs a little behind.
///
/// The price's uncertainty must also be small next to what the margin covers: confidence / price × leverage at most
/// max_conf_margin_ppm. At 1000× the margin covers a 10 bps move, and the general 0.2% bound would allow twice that.
fn check_fill_price(read: &PriceRead, requested_at: i64, leverage: u16, params: &Params) -> Result<()> {
    let fill_at = requested_at + params.fill_delay_sec;
    require!(read.publish_time >= fill_at, PerpError::PriceBeforeRequest);
    require!(read.prev_publish_time < fill_at, PerpError::NotTheFillPrice);
    require!(
        read.conf as u128 * PPM * leverage as u128 <= read.price as u128 * params.max_conf_margin_ppm as u128,
        PerpError::PriceConfidence
    );
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
    let config = &ctx.accounts.config;
    let params = &config.params;
    let now = Clock::get()?.unix_timestamp;
    require!(!config.paused, PerpError::Paused);
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
    let config = &ctx.accounts.config;
    let params = &config.params;
    let order = &ctx.accounts.order;
    require!(!config.paused, PerpError::Paused);

    let read = read_price(&ctx.accounts.price_update, &ctx.accounts.market.feed_id, params, FILL_PRICE_ANY_AGE)?;
    check_fill_price(&read, order.requested_at, order.leverage, params)?;
    let (side, margin, leverage) = (order.side, order.margin, order.leverage);
    let notional = margin.checked_mul(leverage as u64).ok_or(PerpError::MathOverflow)?;

    // The caps follow the vault: net vault × ratio, never below the floor, per market and side and over all markets
    // on a side. A single position cannot exceed either
    let net = ctx.accounts.vault.net_equity();
    let cap = math::oi_cap(net, params.oi_cap_ratio_ppm, params.oi_floor);
    let total_cap = math::oi_cap(net, params.oi_total_cap_ratio_ppm, params.oi_total_floor);
    let market = &mut ctx.accounts.market;
    let oi = match side {
        Side::Long => &mut market.oi_long,
        Side::Short => &mut market.oi_short,
    };
    let new_oi = oi.checked_add(notional).ok_or(PerpError::MathOverflow)?;
    require!(new_oi <= cap, PerpError::OpenInterestCap);
    *oi = new_oi;
    let units = match side {
        Side::Long => &mut market.long_units,
        Side::Short => &mut market.short_units,
    };
    *units = units.checked_add(math::pnl_units(notional, read.price)).ok_or(PerpError::MathOverflow)?;
    let vault = &mut ctx.accounts.vault;
    let total = match side {
        Side::Long => &mut vault.oi_long_total,
        Side::Short => &mut vault.oi_short_total,
    };
    let new_total = total.checked_add(notional).ok_or(PerpError::MathOverflow)?;
    require!(new_total <= total_cap, PerpError::OpenInterestCap);
    *total = new_total;

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
    queued: u64,
    /// Where the queued part starts on the queue line
    ticket_start: u64,
    fee: u64,
}

#[allow(clippy::too_many_arguments)]
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
    let units = match position.side {
        Side::Long => &mut market.long_units,
        Side::Short => &mut market.short_units,
    };
    *units = units.saturating_sub(math::pnl_units(position.notional, position.entry_price));
    let total = match position.side {
        Side::Long => &mut vault.oi_long_total,
        Side::Short => &mut vault.oi_short_total,
    };
    *total = total.saturating_sub(position.notional);
    vault.open_margin = vault.open_margin.checked_sub(position.margin).ok_or(PerpError::MathOverflow)?;

    let queue_empty = vault.queue_total == 0;
    let raw = math::raw_pnl(position.side, position.entry_price, exit, position.notional);
    let margin = position.margin;
    let mut s = Settlement { liquidated: false, raw_pnl: raw, paid: 0, queued: 0, ticket_start: 0, fee: 0 };

    if liquidate || math::is_bust(position.side, exit, position.bust_price) {
        // Hard bust: the whole margin goes to the vault
        s.liquidated = true;
        s.fee = if queue_empty { math::fee_of(margin, params.loss_fee_ppm) } else { 0 };
        vault.cash = vault.cash.checked_add(margin - s.fee).ok_or(PerpError::MathOverflow)?;
    } else if raw < 0 {
        let loss = ((-raw) as u64).min(margin);
        s.fee = if queue_empty { math::fee_of(loss, params.loss_fee_ppm) } else { 0 };
        vault.cash = vault.cash.checked_add(loss - s.fee).ok_or(PerpError::MathOverflow)?;
        s.paid = margin - loss;
    } else {
        // Win: the margin always comes back, the profit comes from vault cash; what it cannot cover is queued (no fee)
        let win = math::win_breakdown(position.entry_price, exit, position.notional, market.position_multiplier_milli, params);
        let profit = if queue_empty && vault.cash >= win.gain {
            vault.cash -= win.gain;
            s.fee = win.fee;
            win.net
        } else if queue_empty {
            let partial = vault.cash.min(win.net);
            vault.cash -= partial;
            partial
        } else {
            0
        };
        s.queued = win.net - profit;
        s.paid = margin + profit;
        if s.queued > 0 {
            s.ticket_start = vault.queued_cumulative;
            vault.queued_cumulative = vault.queued_cumulative.checked_add(s.queued).ok_or(PerpError::MathOverflow)?;
            vault.queue_total = vault.queue_total.checked_add(s.queued).ok_or(PerpError::MathOverflow)?;
            user.queued = user.queued.checked_add(s.queued).ok_or(PerpError::MathOverflow)?;
        }
    }

    vault.fees_accrued = vault.fees_accrued.checked_add(s.fee).ok_or(PerpError::MathOverflow)?;
    user.balance = user.balance.checked_add(s.paid).ok_or(PerpError::MathOverflow)?;
    vault.user_balances = vault.user_balances.checked_add(s.paid).ok_or(PerpError::MathOverflow)?;
    Ok(s)
}

/// Opens the owner's next ticket for the part of a win the vault could not pay. The keeper closing the position
/// pays the rent and gets it back when the ticket is fully claimed.
fn open_ticket<'info>(
    ticket: &UncheckedAccount<'info>,
    payer: &Signer<'info>,
    system: &Program<'info, System>,
    user: &mut UserAccount,
    bump: u8,
    start: u64,
    amount: u64,
) -> Result<()> {
    let (owner, id) = (user.owner, user.next_ticket_id);
    let space = 8 + Ticket::INIT_SPACE;
    let rent = Rent::get()?.minimum_balance(space);
    let (id_bytes, bump_bytes) = (id.to_le_bytes(), [bump]);
    let seeds: &[&[&[u8]]] = &[&[TICKET_SEED, owner.as_ref(), &id_bytes, &bump_bytes]];
    let info = ticket.to_account_info();
    // Anyone can send lamports to the PDA beforehand. Creating it must work then too,
    // or one lamport would be enough to block a winning close
    if info.lamports() == 0 {
        let accounts = CreateAccount { from: payer.to_account_info(), to: info.clone() };
        sys::create_account(CpiContext::new_with_signer(system.key(), accounts, seeds), rent, space as u64, &crate::ID)?;
    } else {
        let top_up = rent.saturating_sub(info.lamports());
        if top_up > 0 {
            let accounts = Transfer { from: payer.to_account_info(), to: info.clone() };
            sys::transfer(CpiContext::new(system.key(), accounts), top_up)?;
        }
        let accounts = Allocate { account_to_allocate: info.clone() };
        sys::allocate(CpiContext::new_with_signer(system.key(), accounts, seeds), space as u64)?;
        let accounts = Assign { account_to_assign: info.clone() };
        sys::assign(CpiContext::new_with_signer(system.key(), accounts, seeds), &crate::ID)?;
    }
    let record = Ticket { owner, id, start, amount, claimed: 0, rent_payer: payer.key(), bump };
    record.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
    user.next_ticket_id = id.checked_add(1).ok_or(PerpError::MathOverflow)?;
    emit!(TicketOpened { owner, ticket: info.key(), id, start, amount });
    Ok(())
}

/// Writes the user's net result for the epoch and moves the epoch's total loss by the difference
/// (a win reduces a loss in the same epoch). A freshly opened record gets its identity here.
fn record_result(
    distributor: &mut Distributor,
    record: &mut EpochLoss,
    owner: Pubkey,
    rent_payer: Pubkey,
    bump: u8,
    margin: u64,
    s: &Settlement,
) -> Result<()> {
    if record.owner == Pubkey::default() {
        record.owner = owner;
        record.epoch = distributor.epoch;
        record.rent_payer = rent_payer;
        record.bump = bump;
    }
    let before = record.loss();
    let result = s.paid as i128 + s.queued as i128 - margin as i128;
    let net = (record.net as i128 + result).clamp(i64::MIN as i128, i64::MAX as i128) as i64;
    record.net = net;
    let after = record.loss();
    distributor.epoch_loss = if after >= before {
        distributor.epoch_loss.checked_add(after - before)
    } else {
        distributor.epoch_loss.checked_sub(before - after)
    }
    .ok_or(PerpError::MathOverflow)?;
    Ok(())
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
        queued: s.queued,
        fee: s.fee,
    });
}

// ---------- execute_close (keeper)

#[derive(Accounts)]
pub struct ExecuteClose<'info> {
    /// Pays the rent of the epoch loss record (gets it back on claim)
    #[account(mut)]
    pub keeper: Signer<'info>,
    /// CHECK: Paid the order's rent (address stored on the order)
    #[account(mut, address = order.rent_payer)]
    pub order_rent_payer: UncheckedAccount<'info>,
    /// CHECK: Paid the position's rent (address stored on the position)
    #[account(mut, address = position.rent_payer)]
    pub position_rent_payer: UncheckedAccount<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(mut, address = position.market)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, seeds = [USER_SEED, position.owner.as_ref()], bump = user.bump)]
    pub user: Box<Account<'info, UserAccount>>,
    #[account(
        mut,
        close = order_rent_payer,
        seeds = [ORDER_SEED, order.owner.as_ref(), &order.id.to_le_bytes()],
        bump = order.bump,
        constraint = order.kind == OrderKind::Close && order.position == position.key() @ PerpError::OrderMismatch,
    )]
    pub order: Box<Account<'info, OrderRequest>>,
    #[account(
        mut,
        close = position_rent_payer,
        seeds = [POSITION_SEED, position.owner.as_ref(), &position.id.to_le_bytes()],
        bump = position.bump,
    )]
    pub position: Box<Account<'info, Position>>,
    /// CHECK: The PDA of the owner's next ticket; created only if the close queues part of a win
    #[account(mut, seeds = [TICKET_SEED, position.owner.as_ref(), &user.next_ticket_id.to_le_bytes()], bump)]
    pub ticket: UncheckedAccount<'info>,
    pub price_update: Box<Account<'info, PriceUpdateV2>>,
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Box<Account<'info, Distributor>>,
    #[account(
        init_if_needed,
        payer = keeper,
        space = 8 + EpochLoss::INIT_SPACE,
        seeds = [EPOCH_LOSS_SEED, position.owner.as_ref(), &distributor.epoch.to_le_bytes()],
        bump,
    )]
    pub epoch_loss: Box<Account<'info, EpochLoss>>,
    pub system_program: Program<'info, System>,
}

pub fn handle_execute_close(ctx: Context<ExecuteClose>) -> Result<()> {
    let params = ctx.accounts.config.params;
    let read = read_price(&ctx.accounts.price_update, &ctx.accounts.market.feed_id, &params, FILL_PRICE_ANY_AGE)?;
    check_fill_price(&read, ctx.accounts.order.requested_at, ctx.accounts.position.leverage, &params)?;
    let s = settle(
        &params,
        &mut ctx.accounts.vault,
        &mut ctx.accounts.market,
        &mut ctx.accounts.user,
        &ctx.accounts.position,
        read.price,
        false,
    )?;
    if s.queued > 0 {
        open_ticket(
            &ctx.accounts.ticket,
            &ctx.accounts.keeper,
            &ctx.accounts.system_program,
            &mut ctx.accounts.user,
            ctx.bumps.ticket,
            s.ticket_start,
            s.queued,
        )?;
    }
    record_result(
        &mut ctx.accounts.distributor,
        &mut ctx.accounts.epoch_loss,
        ctx.accounts.position.owner,
        ctx.accounts.keeper.key(),
        ctx.bumps.epoch_loss,
        ctx.accounts.position.margin,
        &s,
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

/// Cancels an unfilled order and refunds the locked margin. Only after the timeout, and only meant for a Pyth outage:
/// the fill price exists from requested_at + fill_delay_sec on and keepers fill with it, so an earlier cancel would let
/// a user look at that price and walk away from it (a free option).
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
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(mut, address = position.market)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, seeds = [USER_SEED, position.owner.as_ref()], bump = user.bump)]
    pub user: Box<Account<'info, UserAccount>>,
    #[account(
        mut,
        close = rent_payer,
        seeds = [POSITION_SEED, position.owner.as_ref(), &position.id.to_le_bytes()],
        bump = position.bump,
    )]
    pub position: Box<Account<'info, Position>>,
    pub price_update: Box<Account<'info, PriceUpdateV2>>,
    #[account(mut, seeds = [DISTRIBUTOR_SEED], bump = distributor.bump)]
    pub distributor: Box<Account<'info, Distributor>>,
    #[account(
        init_if_needed,
        payer = keeper,
        space = 8 + EpochLoss::INIT_SPACE,
        seeds = [EPOCH_LOSS_SEED, position.owner.as_ref(), &distributor.epoch.to_le_bytes()],
        bump,
    )]
    pub epoch_loss: Box<Account<'info, EpochLoss>>,
    pub system_program: Program<'info, System>,
}

pub fn handle_liquidate(ctx: Context<Liquidate>) -> Result<()> {
    let params = ctx.accounts.config.params;
    let read = read_price(&ctx.accounts.price_update, &ctx.accounts.market.feed_id, &params, params.max_price_age_sec)?;
    let position = &ctx.accounts.position;
    require!(math::is_bust(position.side, read.price, position.bust_price), PerpError::NotLiquidatable);
    let s =
        settle(&params, &mut ctx.accounts.vault, &mut ctx.accounts.market, &mut ctx.accounts.user, position, read.price, true)?;
    record_result(
        &mut ctx.accounts.distributor,
        &mut ctx.accounts.epoch_loss,
        ctx.accounts.position.owner,
        ctx.accounts.keeper.key(),
        ctx.bumps.epoch_loss,
        ctx.accounts.position.margin,
        &s,
    )?;
    emit_closed(&ctx.accounts.position, read.price, &s);
    Ok(())
}
