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
/// A queued win: one ticket per win the vault could not pay, numbered per owner
#[constant]
pub const TICKET_SEED: &[u8] = b"ticket";
#[constant]
pub const DISTRIBUTOR_SEED: &[u8] = b"distributor";
#[constant]
pub const STAKE_POOL_SEED: &[u8] = b"stake_pool";
#[constant]
pub const STAKE_TOKENS_SEED: &[u8] = b"stake_tokens";
#[constant]
pub const STAKE_SEED: &[u8] = b"stake";
#[constant]
pub const TREASURY_SEED: &[u8] = b"treasury";
#[constant]
pub const TREASURY_TOKENS_SEED: &[u8] = b"treasury_tokens";
#[constant]
pub const XBUST_MINT_SEED: &[u8] = b"xbust_mint";
#[constant]
pub const EPOCH_SEED: &[u8] = b"epoch";
#[constant]
pub const EPOCH_LOSS_SEED: &[u8] = b"epoch_loss";
#[constant]
pub const VEST_SEED: &[u8] = b"vest";
#[constant]
pub const TOKEN_FEE_CONFIG_SEED: &[u8] = b"token_fee_config";
/// The wallet $BUST trading fees are paid to: a PDA of this program. Nobody holds a key for it; the program signs for
/// it, and only to move a fee into the vault or to the registered swap account
#[constant]
pub const FEE_WALLET_SEED: &[u8] = b"fee_wallet";
/// The most of a swept token fee that can go to operations (basis points). The rest always goes to the vault
pub const MAX_TOKEN_FEE_OPS_BPS: u16 = 2_000;
/// A user's deposit address: a PDA per owner; any USDC token account it owns is swept into that owner's balance
#[constant]
pub const DEPOSIT_SEED: &[u8] = b"deposit";

/// An epoch can be funded for this many epochs after it closes. After that its loss records can be closed and their
/// rent returned, and it can no longer be funded (its losers' shares would have nobody left to claim them)
pub const FUNDING_WINDOW_EPOCHS: u64 = 30;

/// One day (s); open interest utilisation is tracked per day
pub const DAY_SEC: i64 = 86_400;

/// Prices are kept at 1e-8 (the exponent of Pyth's crypto feeds)
pub const PRICE_EXPONENT: i32 = -8;
/// Rates are parts per million
pub const PPM: u128 = 1_000_000;
/// Fixed-point scale for intermediate values
pub const SCALE: u128 = 1_000_000_000_000;
