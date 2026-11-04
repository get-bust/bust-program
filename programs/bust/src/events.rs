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
    pub fee: u64,
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
