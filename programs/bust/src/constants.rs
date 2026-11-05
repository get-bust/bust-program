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
pub const LAUNCH_CONFIG_SEED: &[u8] = b"launch_config";
/// The wallet that claims the LaunchLab platform fee: a PDA of this program, which signs for it
#[constant]
pub const FEE_WALLET_SEED: &[u8] = b"fee_wallet";
/// A user's deposit address: a PDA per owner; any USDC token account it owns is swept into that owner's balance
#[constant]
pub const DEPOSIT_SEED: &[u8] = b"deposit";

/// Allowed Raydium LaunchLab programs (mainnet, devnet). The admin cannot point at any other program:
/// if it could, that program could drain fee accounts with the fee wallet PDA's signature
pub const LAUNCHPAD_PROGRAMS: [Pubkey; 2] = [
    anchor_lang::pubkey!("LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj"),
    anchor_lang::pubkey!("DRay6fNdQ5J82H7xV6uq2aV3mNrUZ1J4PgSKsWgptcm6"),
];
/// LaunchLab claim_platform_fee and claim_platform_fee_from_vault discriminators (raydium-idl)
pub const IX_CLAIM_PLATFORM_FEE: [u8; 8] = [156, 39, 208, 135, 76, 237, 61, 72];
pub const IX_CLAIM_PLATFORM_FEE_FROM_VAULT: [u8; 8] = [117, 241, 198, 168, 248, 218, 80, 29];

/// One day (s); open interest utilisation is tracked per day
pub const DAY_SEC: i64 = 86_400;

/// Prices are kept at 1e-8 (the exponent of Pyth's crypto feeds)
pub const PRICE_EXPONENT: i32 = -8;
/// Rates are parts per million
pub const PPM: u128 = 1_000_000;
/// Fixed-point scale for intermediate values
pub const SCALE: u128 = 1_000_000_000_000;
