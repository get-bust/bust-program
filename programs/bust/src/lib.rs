pub mod constants;
pub mod error;
pub mod instructions;
pub mod math;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("8P7m2ChjnNtEbvVyHGKkBtfYP2AVG9ktsFEyvQrmcAiw");

/// Leveraged perpetuals traded against a USDC vault.
#[program]
pub mod bust {
    use super::*;

    // ---------- admin
    pub fn initialize(ctx: Context<Initialize>, params: Params) -> Result<()> {
        instructions::admin::handle_initialize(ctx, params)
    }

    pub fn update_config(ctx: Context<AdminOnly>, params: Params) -> Result<()> {
        instructions::admin::handle_update_config(ctx, params)
    }

    // ---------- accounts and funds
    pub fn create_user(ctx: Context<CreateUser>) -> Result<()> {
        instructions::user::handle_create_user(ctx)
    }

    pub fn deposit(ctx: Context<MoveFunds>, amount: u64) -> Result<()> {
        instructions::user::handle_deposit(ctx, amount)
    }

    pub fn withdraw(ctx: Context<MoveFunds>, amount: u64) -> Result<()> {
        instructions::user::handle_withdraw(ctx, amount)
    }

    pub fn seed_vault(ctx: Context<SeedVault>, amount: u64) -> Result<()> {
        instructions::user::handle_seed_vault(ctx, amount)
    }

    pub fn set_session(ctx: Context<ManageSession>, session_key: Pubkey, expires_at: i64) -> Result<()> {
        instructions::user::handle_set_session(ctx, session_key, expires_at)
    }

    pub fn revoke_session(ctx: Context<ManageSession>) -> Result<()> {
        instructions::user::handle_revoke_session(ctx)
    }
}
