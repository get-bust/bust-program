use anchor_lang::prelude::*;

#[error_code]
pub enum PerpError {
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
    #[msg("Payout queue is full")]
    QueueFull,
    #[msg("Remaining accounts do not match the payout queue")]
    QueueAccountMismatch,
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
}
