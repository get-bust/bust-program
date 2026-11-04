use anchor_lang::prelude::*;

#[error_code]
pub enum PerpError {
    #[msg("Invalid config parameter")]
    InvalidConfig,
}
