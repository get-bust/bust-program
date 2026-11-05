use anchor_lang::prelude::*;

#[error_code]
pub enum PerpError {
    #[msg("Program is paused")]
    Paused,
    #[msg("Signer is neither the owner nor an active session key")]
    Unauthorized,
    #[msg("Leverage out of range")]
    InvalidLeverage,
    #[msg("Margin below minimum")]
    MarginTooSmall,
    #[msg("Insufficient balance")]
    InsufficientBalance,
    #[msg("Position size exceeds the open interest cap")]
    OpenInterestCap,
    #[msg("Invalid side")]
    InvalidSide,
    #[msg("Price is not positive or uses an unsupported exponent")]
    InvalidPrice,
    #[msg("Price confidence interval too wide")]
    PriceConfidence,
    #[msg("Position is not liquidatable at this price")]
    NotLiquidatable,
    #[msg("Session expiry out of range")]
    InvalidSession,
    #[msg("Invalid config parameter")]
    InvalidConfig,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Amount must be positive")]
    ZeroAmount,
    #[msg("Price was published before the order request")]
    PriceBeforeRequest,
    #[msg("Price was published too long after the order request")]
    PriceTooLate,
    #[msg("Order cannot be cancelled yet")]
    OrderNotExpired,
    #[msg("Order does not match the accounts")]
    OrderMismatch,
    #[msg("Epoch has not ended yet")]
    EpochNotOver,
    #[msg("Payout queue must be empty")]
    QueueNotEmpty,
    #[msg("Destination does not match the registered wallet")]
    InvalidDestination,
    #[msg("Nothing to withdraw")]
    NothingToWithdraw,
    #[msg("Unstaked tokens are still in the waiting period")]
    UnstakeNotReady,
    #[msg("Epoch is already funded")]
    EpochFunded,
    #[msg("Epoch is not funded yet")]
    EpochNotFunded,
    #[msg("Signer is not the registered buyback authority")]
    NotBuybackAuthority,
    #[msg("The epoch's funding window has closed")]
    FundingWindowClosed,
    #[msg("The epoch can still be funded")]
    FundingWindowOpen,
}
