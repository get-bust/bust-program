use anchor_lang::prelude::*;

#[error_code]
pub enum PerpError {
    #[msg("Insufficient balance")]
    InsufficientBalance,
    #[msg("Session expiry out of range")]
    InvalidSession,
    #[msg("Invalid config parameter")]
    InvalidConfig,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Amount must be positive")]
    ZeroAmount,
}
