use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::PerpError,
    events::{QueuePaid, TicketClaimed},
    state::*,
};

// ---------- harvest (anyone; the keeper calls it whenever there is debt and cash)

#[derive(Accounts)]
pub struct Harvest<'info> {
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
}

/// Moves vault cash onto the queue: paid_cumulative advances by min(cash, debt). The money now belongs to the tickets it
/// reached, in order, and stays in the vault until each one is claimed. Costs the same however many tickets wait
pub fn handle_harvest(ctx: Context<Harvest>) -> Result<()> {
    let vault = &mut ctx.accounts.vault;
    let pay = vault.cash.min(vault.queue_total);
    require!(pay > 0, PerpError::NothingToWithdraw);
    vault.cash -= pay;
    vault.queue_total -= pay;
    vault.queue_unclaimed = vault.queue_unclaimed.checked_add(pay).ok_or(PerpError::MathOverflow)?;
    vault.paid_cumulative = vault.paid_cumulative.checked_add(pay).ok_or(PerpError::MathOverflow)?;
    emit!(QueuePaid { amount: pay, paid_cumulative: vault.paid_cumulative, queue_total: vault.queue_total });
    Ok(())
}

// ---------- claim_ticket (anyone; the money only goes to the ticket's owner)

#[derive(Accounts)]
pub struct ClaimTicket<'info> {
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, seeds = [USER_SEED, ticket.owner.as_ref()], bump = user.bump)]
    pub user: Account<'info, UserAccount>,
    #[account(mut, seeds = [TICKET_SEED, ticket.owner.as_ref(), &ticket.id.to_le_bytes()], bump = ticket.bump)]
    pub ticket: Account<'info, Ticket>,
    /// CHECK: Paid the ticket's rent and gets it back when it closes (address stored on the ticket)
    #[account(mut, address = ticket.rent_payer)]
    pub rent_payer: UncheckedAccount<'info>,
}

/// Credits the owner with whatever the queue has paid on the ticket since the last claim. A fully paid ticket closes
pub fn handle_claim_ticket(ctx: Context<ClaimTicket>) -> Result<()> {
    let vault = &mut ctx.accounts.vault;
    let ticket = &mut ctx.accounts.ticket;
    let amount = ticket.paid(vault.paid_cumulative) - ticket.claimed;
    require!(amount > 0, PerpError::NothingToWithdraw);
    ticket.claimed += amount;
    vault.queue_unclaimed = vault.queue_unclaimed.checked_sub(amount).ok_or(PerpError::MathOverflow)?;
    vault.user_balances = vault.user_balances.checked_add(amount).ok_or(PerpError::MathOverflow)?;
    let user = &mut ctx.accounts.user;
    user.queued = user.queued.checked_sub(amount).ok_or(PerpError::MathOverflow)?;
    user.balance = user.balance.checked_add(amount).ok_or(PerpError::MathOverflow)?;

    let closed = ticket.claimed == ticket.amount;
    emit!(TicketClaimed { owner: ticket.owner, ticket: ticket.key(), amount, closed });
    if closed {
        ctx.accounts.ticket.close(ctx.accounts.rent_payer.to_account_info())?;
    }
    Ok(())
}
