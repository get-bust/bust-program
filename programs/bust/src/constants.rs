use anchor_lang::prelude::*;

#[constant]
pub const CONFIG_SEED: &[u8] = b"config";
#[constant]
pub const VAULT_SEED: &[u8] = b"vault";
#[constant]
pub const VAULT_TOKENS_SEED: &[u8] = b"vault_tokens";
#[constant]
pub const MARKET_SEED: &[u8] = b"market";
#[constant]
pub const USER_SEED: &[u8] = b"user";
#[constant]
pub const POSITION_SEED: &[u8] = b"position";
#[constant]
pub const ORDER_SEED: &[u8] = b"order";

/// Prices are kept at 1e-8 (the exponent of Pyth's crypto feeds)
pub const PRICE_EXPONENT: i32 = -8;
/// Rates are parts per million
pub const PPM: u128 = 1_000_000;
/// Fixed-point scale for intermediate values
pub const SCALE: u128 = 1_000_000_000_000;
