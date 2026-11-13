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
pub fn read_price(update: &PriceUpdateV2, feed_id: &[u8; 32], params: &Params, max_age_sec: u64) -> Result<PriceRead> {
    let read = read_unchecked_conf(update, feed_id, params, max_age_sec)?;
    require!(conf_within(&read, params.max_conf_ppm as u128), PerpError::PriceConfidence);
    Ok(read)
}

/// confidence / price within `ppm`
fn conf_within(read: &PriceRead, ppm: u128) -> bool {
    read.conf as u128 * PPM <= read.price as u128 * ppm
}

/// read_price without the confidence bound, for fills and cancels, which apply their own
#[allow(clippy::manual_range_contains)]
fn read_unchecked_conf(update: &PriceUpdateV2, feed_id: &[u8; 32], params: &Params, max_age_sec: u64) -> Result<PriceRead> {
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
    Ok(PriceRead {
        price: value as u64,
        publish_time: price.publish_time,
        prev_publish_time: update.price_message.prev_publish_time,
        conf: conf as u64,
    })
}

/// An order fills at exactly one price: the first update on the feed published at or after its fill time
/// T = order.fill_at, i.e. publish_time ≥ T and prev_publish_time < T. Pyth signs prev_publish_time with each update,
/// so the message is unique and cannot be forged. A user running their own keeper therefore cannot pick the best of
/// several updates, and cannot skip a bad one either: any keeper can fetch the same message from Hermes later and
/// fill with it. T is set at request from the later of the chain clock and the newest price the market has seen,
/// plus a delay, so it lies past anything the user could have seen even if the chain clock lags.
fn require_fill_message(read: &PriceRead, fill_at: i64) -> Result<()> {
    require!(read.publish_time >= fill_at, PerpError::PriceBeforeRequest);
    require!(read.prev_publish_time < fill_at, PerpError::NotTheFillPrice);
    Ok(())
}

/// Whether the order's fill message can fill it. Decided by the message alone, so whether the order fills or can be
/// cancelled is never anyone's choice:
/// - it must come at most max_fill_gap_sec after T; a later one means Pyth was down, and filling at it would hand
///   the trader a price from long after they asked;
/// - its confidence must be within the general bound;
/// - for an open, also within max_conf_margin_ppm of the margin as confidence / price × leverage (at 1000× the
///   margin covers 10 bps; the general 0.2% would allow twice that). The point is not to open a position whose entry
///   is noise next to its margin. A close does not take it: its price is fixed all the same and the uncertainty cuts
///   both ways, and holding a close back in a volatile market would keep traders in when they most want out
fn fillable(read: &PriceRead, fill_at: i64, open_leverage: Option<u16>, params: &Params) -> bool {
    read.publish_time - fill_at <= params.max_fill_gap_sec
        && conf_within(read, params.max_conf_ppm as u128)
        && open_leverage.is_none_or(|leverage| {
            read.conf as u128 * PPM * leverage as u128 <= read.price as u128 * params.max_conf_margin_ppm as u128
        })
}

/// Whether an open of this size fits both open interest caps right now. The caps follow the vault: net vault × ratio,
/// never below the floor, per market and side and over all markets on a side. A single position cannot exceed either
fn fits_caps(vault: &Vault, market: &Market, side: Side, notional: u64, params: &Params) -> bool {
    let net = vault.net_equity();
    let (floor, total_floor) = params.floors(vault.queue_total);
    let (oi, pending, total, pending_total) = match side {
        Side::Long => (market.oi_long, market.pending_long, vault.oi_long_total, vault.pending_long_total),
        Side::Short => (market.oi_short, market.pending_short, vault.oi_short_total, vault.pending_short_total),
    };
    let within = |open: u64, waiting: u64, cap: u64| {
        open.checked_add(waiting).and_then(|n| n.checked_add(notional)).is_some_and(|n| n <= cap)
    };
    within(oi, pending, math::oi_cap(net, params.oi_cap_ratio_ppm, floor))
        && within(total, pending_total, math::oi_cap(net, params.oi_total_cap_ratio_ppm, total_floor))
}

/// The market's and the vault's pending notional on a side
fn pending_mut<'a>(vault: &'a mut Vault, market: &'a mut Market, side: Side) -> (&'a mut u64, &'a mut u64) {
    match side {
        Side::Long => (&mut market.pending_long, &mut vault.pending_long_total),
        Side::Short => (&mut market.pending_short, &mut vault.pending_short_total),
    }
}

/// Holds room under the caps for an open order from its request until it fills or cancels
fn reserve(vault: &mut Vault, market: &mut Market, side: Side, notional: u64) -> Result<()> {
    let (pending, total) = pending_mut(vault, market, side);
    *pending = pending.checked_add(notional).ok_or(PerpError::MathOverflow)?;
    *total = total.checked_add(notional).ok_or(PerpError::MathOverflow)?;
    Ok(())
}

fn release(vault: &mut Vault, market: &mut Market, side: Side, notional: u64) -> Result<()> {
    let (pending, total) = pending_mut(vault, market, side);
    *pending = pending.checked_sub(notional).ok_or(PerpError::MathOverflow)?;
    *total = total.checked_sub(notional).ok_or(PerpError::MathOverflow)?;
    Ok(())
}

fn check_fill_price(read: &PriceRead, fill_at: i64, open_leverage: Option<u16>, params: &Params) -> Result<()> {
    require_fill_message(read, fill_at)?;
    require!(read.publish_time - fill_at <= params.max_fill_gap_sec, PerpError::PythOutage);
    require!(fillable(read, fill_at, open_leverage, params), PerpError::PriceConfidence);
    Ok(())
}

/// Moves the market's newest seen publish time forward (never back)
fn observe(market: &mut Market, publish_time: i64) {
    market.last_publish_time = market.last_publish_time.max(publish_time);
}

/// An order requested now fills at the first price published at or after this
fn fill_time(now: i64, market: &Market, params: &Params) -> i64 {
    now.max(market.last_publish_time) + params.fill_delay_sec
}

/// Past its expiry an order can no longer fill, and can be cancelled without a price
fn expired(order: &OrderRequest, now: i64) -> bool {
    now > order.fill_at.saturating_add(ORDER_EXPIRY_SEC)
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
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, seeds = [MARKET_SEED, &market.index.to_le_bytes()], bump = market.bump)]
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

/// Margin is locked right away (balance → reserved) and the caps are checked and the notional reserved right away; the
/// price is set at fill time. An order that passes here is never refused by the caps later, so no order can sit waiting
/// for someone to free the cap while the price runs away from its fill price
pub fn handle_request_open(ctx: Context<RequestOpen>, side: Side, margin: u64, leverage: u16) -> Result<()> {
    let config = &ctx.accounts.config;
    let params = &config.params;
    let now = Clock::get()?.unix_timestamp;
    require!(!config.paused, PerpError::Paused);
    require!(ctx.accounts.user.can_trade(&ctx.accounts.authority.key(), now), PerpError::Unauthorized);
    require!(leverage >= 1 && leverage <= params.max_leverage, PerpError::InvalidLeverage);
    require!(margin >= params.min_margin, PerpError::MarginTooSmall);
    let notional = margin.checked_mul(leverage as u64).ok_or(PerpError::MathOverflow)?;
    require!(fits_caps(&ctx.accounts.vault, &ctx.accounts.market, side, notional, params), PerpError::OpenInterestCap);
    reserve(&mut ctx.accounts.vault, &mut ctx.accounts.market, side, notional)?;

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
        fill_at: fill_time(now, &ctx.accounts.market, params),
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
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(address = position.market)]
    pub market: Account<'info, Market>,
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
        fill_at: fill_time(now, &ctx.accounts.market, &ctx.accounts.config.params),
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
    pub vault: Box<Account<'info, Vault>>,
    #[account(mut, address = order.market)]
    pub market: Box<Account<'info, Market>>,
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
    require!(!expired(order, Clock::get()?.unix_timestamp), PerpError::OrderExpired);

    let read = read_unchecked_conf(&ctx.accounts.price_update, &ctx.accounts.market.feed_id, params, FILL_PRICE_ANY_AGE)?;
    check_fill_price(&read, order.fill_at, Some(order.leverage), params)?;
    let (side, margin, leverage) = (order.side, order.margin, order.leverage);
    let notional = margin.checked_mul(leverage as u64).ok_or(PerpError::MathOverflow)?;

    // The caps were checked at the request. Even if the vault has shrunk since, the order keeps its room
    release(&mut ctx.accounts.vault, &mut ctx.accounts.market, side, notional)?;
    let market = &mut ctx.accounts.market;
    observe(market, read.publish_time);
    let oi = match side {
        Side::Long => &mut market.oi_long,
        Side::Short => &mut market.oi_short,
    };
    *oi = oi.checked_add(notional).ok_or(PerpError::MathOverflow)?;
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
    *total = total.checked_add(notional).ok_or(PerpError::MathOverflow)?;

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
        fill_publish_time: read.publish_time,
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
    require!(!expired(&ctx.accounts.order, Clock::get()?.unix_timestamp), PerpError::OrderExpired);
    let read = read_unchecked_conf(&ctx.accounts.price_update, &ctx.accounts.market.feed_id, &params, FILL_PRICE_ANY_AGE)?;
    check_fill_price(&read, ctx.accounts.order.fill_at, None, &params)?;
    observe(&mut ctx.accounts.market, read.publish_time);
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
    #[account(mut, address = order.market)]
    pub market: Account<'info, Market>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    /// The order's fill message, as proof that it cannot fill the order. Not needed for an expired order, or for a
    /// close order whose position is gone
    pub price_update: Option<Account<'info, PriceUpdateV2>>,
    /// CHECK: The position a close order would close, only to see whether it still exists, or whatever order.position
    /// holds for an open order (the default key). A close order whose position is already gone has nothing to fill
    #[account(address = order.position)]
    pub position: UncheckedAccount<'info>,
}

/// Cancels an order that cannot fill: its fill message came more than max_fill_gap_sec after the fill time (a Pyth
/// outage) or its confidence is too wide; or a close order whose position no longer exists; or an order that nobody
/// filled within ORDER_EXPIRY_SEC, which can no longer fill at all. Every other order can only be filled, so
/// cancelling is never a way out of a fill price the user does not like. An open order's margin and its room under the
/// caps come back.
pub fn handle_cancel_order(ctx: Context<CancelOrder>) -> Result<()> {
    let params = ctx.accounts.config.params;
    let order = &ctx.accounts.order;
    let is_open = order.kind == OrderKind::Open;
    // A close order whose position was liquidated, or closed by another order, since it was requested has nothing
    // to fill; any other order needs its fill message to show that it cannot fill
    let gone = !is_open && ctx.accounts.position.data_is_empty();
    if !gone && !expired(order, Clock::get()?.unix_timestamp) {
        let open_leverage = is_open.then_some(order.leverage);
        let update = ctx.accounts.price_update.as_ref().ok_or(PerpError::OrderFillable)?;
        let read = read_unchecked_conf(update, &ctx.accounts.market.feed_id, &params, FILL_PRICE_ANY_AGE)?;
        require_fill_message(&read, order.fill_at)?;
        require!(!fillable(&read, order.fill_at, open_leverage, &params), PerpError::OrderFillable);
    }
    if is_open {
        let notional = order.margin.checked_mul(order.leverage as u64).ok_or(PerpError::MathOverflow)?;
        release(&mut ctx.accounts.vault, &mut ctx.accounts.market, order.side, notional)?;
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
    // Never a price from before the position existed, nor one older than the market has already used: a liquidator
    // could otherwise reach back for a wick that the fill came after
    require!(
        read.publish_time >= position.fill_publish_time && read.publish_time >= ctx.accounts.market.last_publish_time,
        PerpError::PriceTooOld
    );
    // Bust even at the trader's end of the confidence interval: an uncertain price does not take the margin
    require!(math::is_clearly_bust(position.side, read.price, read.conf, position.bust_price), PerpError::NotLiquidatable);
    observe(&mut ctx.accounts.market, read.publish_time);
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

// ---------- observe_price (anyone; keeps the market's clock on the Pyth timeline)

#[derive(Accounts)]
pub struct ObservePrice<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [MARKET_SEED, &market.index.to_le_bytes()], bump = market.bump)]
    pub market: Account<'info, Market>,
    pub price_update: Account<'info, PriceUpdateV2>,
}

/// Records a fresh verified price's publish time on the market. Keepers call it every few seconds, so that new orders
/// fill after the latest price anyone could have seen even when the chain clock lags, and liquidations cannot go back
pub fn handle_observe_price(ctx: Context<ObservePrice>) -> Result<()> {
    let params = ctx.accounts.config.params;
    let read = read_price(&ctx.accounts.price_update, &ctx.accounts.market.feed_id, &params, params.max_price_age_sec)?;
    observe(&mut ctx.accounts.market, read.publish_time);
    Ok(())
}
