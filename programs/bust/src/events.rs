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
pub struct QueuePaid {
    pub owner: Pubkey,
    pub amount: u64,
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
    /// Vault profit above the high-water mark
    pub profit: u64,
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
