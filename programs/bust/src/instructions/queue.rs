use anchor_lang::prelude::*;

use crate::{constants::*, error::PerpError, events::QueuePaid, state::*};

#[derive(Accounts)]
pub struct Harvest<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [VAULT_SEED], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(mut, address = config.queue)]
    pub queue: AccountLoader<'info, PayoutQueue>,
    // remaining_accounts: the UserAccounts of the entries at the head of the queue, in order (writable)
}

/// Pays the queue from the head with vault cash (anyone can call; the keeper does every block)
pub fn handle_harvest<'info>(ctx: Context<'info, Harvest<'info>>) -> Result<()> {
    let vault = &mut ctx.accounts.vault;
    let mut queue = ctx.accounts.queue.load_mut()?;

    for info in ctx.remaining_accounts.iter() {
        if vault.cash == 0 {
            break;
        }
        let Some(entry) = queue.front_mut() else { break };

        let mut user: Account<UserAccount> = Account::try_from(info)?;
        let (expected, _) = Pubkey::find_program_address(&[USER_SEED, entry.owner.as_ref()], &crate::ID);
        require!(user.owner == entry.owner && info.key() == expected, PerpError::QueueAccountMismatch);

        let pay = entry.amount.min(vault.cash);
        entry.amount -= pay;
        let owner = entry.owner;
        let done = entry.amount == 0;

        vault.cash -= pay;
        vault.queue_total = vault.queue_total.checked_sub(pay).ok_or(PerpError::MathOverflow)?;
        vault.user_balances = vault.user_balances.checked_add(pay).ok_or(PerpError::MathOverflow)?;
        user.queued = user.queued.checked_sub(pay).ok_or(PerpError::MathOverflow)?;
        user.balance = user.balance.checked_add(pay).ok_or(PerpError::MathOverflow)?;
        user.exit(&crate::ID)?;
        emit!(QueuePaid { owner, amount: pay });

        if !done {
            break; // out of cash
        }
        queue.pop();
    }
    Ok(())
}
