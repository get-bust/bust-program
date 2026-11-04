pub mod constants;
pub mod error;
pub mod instructions;
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
}
