use anchor_lang::prelude::*;

use crate::state::Side;

#[event]
pub struct PositionOpened {
    pub owner: Pubkey,
    pub position: Pubkey,
    pub market: Pubkey,
    pub side: Side,
    pub margin: u64,
    pub leverage: u16,
    pub notional: u64,
    pub entry_price: u64,
    pub bust_price: u64,
}

#[event]
pub struct PositionClosed {
    pub owner: Pubkey,
    pub position: Pubkey,
    pub market: Pubkey,
    pub liquidated: bool,
    pub exit_price: u64,
    pub raw_pnl: i64,
    /// Credited to the balance right away, margin included
    pub paid: u64,
    pub queued: u64,
    pub fee: u64,
}

#[event]
pub struct TicketOpened {
    pub owner: Pubkey,
    pub ticket: Pubkey,
    pub id: u64,
    /// Position on the queue line: paid once paid_cumulative passes start + amount
    pub start: u64,
    pub amount: u64,
}

#[event]
pub struct QueuePaid {
    pub amount: u64,
    pub paid_cumulative: u64,
    /// Still owed after this payment
    pub queue_total: u64,
}

#[event]
pub struct TicketClaimed {
    pub owner: Pubkey,
    pub ticket: Pubkey,
    pub amount: u64,
    /// The ticket is fully paid and its account closed
    pub closed: bool,
}

#[event]
pub struct OrderRequested {
    pub owner: Pubkey,
    pub order: Pubkey,
    pub kind: crate::state::OrderKind,
    pub requested_at: i64,
}

#[event]
pub struct OrderCancelled {
    pub owner: Pubkey,
    pub order: Pubkey,
}

#[event]
pub struct Distributed {
    pub epoch: u64,
    /// Vault profit above the high-water mark, less what open positions are up
    pub profit: u64,
    /// Unrealised profit of open positions (before haircuts) held back from the profit
    pub unrealized: u64,
    /// Share kept in the vault (ppm)
    pub keep_ppm: u32,
    /// Fees plus the distributed part of profit
    pub pool: u64,
    pub burn: u64,
    pub staker: u64,
    pub xbust: u64,
    pub dev: u64,
}

#[event]
pub struct BucketWithdrawn {
    pub bucket: crate::state::Bucket,
    pub amount: u64,
    pub destination: Pubkey,
}

#[event]
pub struct Staked {
    pub owner: Pubkey,
    pub amount: u64,
    pub total_staked: u64,
}

#[event]
pub struct UnstakeRequested {
    pub owner: Pubkey,
    pub amount: u64,
    pub available_at: i64,
}

#[event]
pub struct RewardsClaimed {
    pub owner: Pubkey,
    pub amount: u64,
}

#[event]
pub struct EpochFundedEvent {
    pub epoch: u64,
    pub bust: u64,
    /// xBUST rights opened (at the current rate)
    pub xbust: u64,
    pub total_loss: u64,
}

#[event]
pub struct XbustClaimed {
    pub owner: Pubkey,
    pub epoch: u64,
    pub loss: u64,
    pub amount: u64,
}

#[event]
pub struct XbustLocked {
    pub owner: Pubkey,
    pub amount: u64,
    /// Total still in the lock (not yet unlocked + unlocked but not converted)
    pub locked: u64,
}

#[event]
pub struct TokenFeesSwept {
    /// Everything that left the fee wallet
    pub amount: u64,
    /// The part of it that went to operations
    pub ops: u64,
    /// true: the USDC entered the vault; false: it went to the swap wallet (to be swapped to USDC and seeded)
    pub to_vault: bool,
}

#[event]
pub struct XbustConverted {
    pub owner: Pubkey,
    pub xbust: u64,
    pub bust: u64,
}

#[event]
pub struct DepositSwept {
    pub owner: Pubkey,
    pub amount: u64,
}
