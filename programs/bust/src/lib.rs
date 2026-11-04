pub mod constants;
pub mod error;
pub mod events;
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

    pub fn set_paused(ctx: Context<AdminOnly>, paused: bool) -> Result<()> {
        instructions::admin::handle_set_paused(ctx, paused)
    }

    pub fn add_market(
        ctx: Context<AddMarket>,
        index: u16,
        symbol: [u8; 8],
        feed_id: [u8; 32],
        position_multiplier_milli: u64,
    ) -> Result<()> {
        instructions::admin::handle_add_market(ctx, index, symbol, feed_id, position_multiplier_milli)
    }

    // ---------- epoch distribution
    pub fn init_distributor(ctx: Context<InitDistributor>, params: DistParams) -> Result<()> {
        instructions::distribution::handle_init_distributor(ctx, params)
    }

    pub fn update_distributor(ctx: Context<UpdateDistributor>, params: DistParams) -> Result<()> {
        instructions::distribution::handle_update_distributor(ctx, params)
    }

    pub fn record_utilization<'info>(ctx: Context<'info, RecordUtilization<'info>>) -> Result<()> {
        instructions::distribution::handle_record_utilization(ctx)
    }

    pub fn distribute(ctx: Context<Distribute>) -> Result<()> {
        instructions::distribution::handle_distribute(ctx)
    }

    pub fn withdraw_bucket(ctx: Context<WithdrawBucket>, bucket: Bucket) -> Result<()> {
        instructions::distribution::handle_withdraw_bucket(ctx, bucket)
    }

    // ---------- $BUST staking (the staker share streams as USDC)
    pub fn init_stake_pool(ctx: Context<InitStakePool>, unstake_sec: i64) -> Result<()> {
        instructions::staking::handle_init_stake_pool(ctx, unstake_sec)
    }

    pub fn create_stake_account(ctx: Context<CreateStakeAccount>) -> Result<()> {
        instructions::staking::handle_create_stake_account(ctx)
    }

    pub fn stake(ctx: Context<MoveStake>, amount: u64) -> Result<()> {
        instructions::staking::handle_stake(ctx, amount)
    }

    pub fn request_unstake(ctx: Context<MoveStake>, amount: u64) -> Result<()> {
        instructions::staking::handle_request_unstake(ctx, amount)
    }

    pub fn withdraw_unstaked(ctx: Context<MoveStake>) -> Result<()> {
        instructions::staking::handle_withdraw_unstaked(ctx)
    }

    pub fn claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
        instructions::staking::handle_claim_rewards(ctx)
    }

    // ---------- xBUST (the losers' share; 1/30 of a lock becomes $BUST each day)
    pub fn init_treasury(ctx: Context<InitTreasury>, vest_sec: i64, buyback_authority: Pubkey) -> Result<()> {
        instructions::xbust::handle_init_treasury(ctx, vest_sec, buyback_authority)
    }

    pub fn update_treasury(ctx: Context<UpdateTreasury>, buyback_authority: Pubkey) -> Result<()> {
        instructions::xbust::handle_update_treasury(ctx, buyback_authority)
    }

    pub fn migrate_treasury(ctx: Context<MigrateTreasury>) -> Result<()> {
        instructions::xbust::handle_migrate_treasury(ctx)
    }

    pub fn fund_epoch(ctx: Context<FundEpoch>, epoch: u64, amount: u64) -> Result<()> {
        instructions::xbust::handle_fund_epoch(ctx, epoch, amount)
    }

    pub fn claim_xbust(ctx: Context<ClaimXbust>, epoch: u64) -> Result<()> {
        instructions::xbust::handle_claim_xbust(ctx, epoch)
    }

    pub fn lock_xbust(ctx: Context<LockXbust>, amount: u64) -> Result<()> {
        instructions::xbust::handle_lock_xbust(ctx, amount)
    }

    pub fn convert_vested(ctx: Context<ConvertVested>) -> Result<()> {
        instructions::xbust::handle_convert_vested(ctx)
    }

    // ---------- LaunchLab platform fee (the fee wallet is the program's PDA)
    pub fn init_launch_config(ctx: Context<InitLaunchConfig>, launchpad_program: Pubkey, platform_config: Pubkey) -> Result<()> {
        instructions::launchlab::handle_init_launch_config(ctx, launchpad_program, platform_config)
    }

    pub fn update_launch_config(ctx: Context<UpdateLaunchConfig>, platform_config: Pubkey) -> Result<()> {
        instructions::launchlab::handle_update_launch_config(ctx, platform_config)
    }

    pub fn claim_launch_fees<'info>(ctx: Context<'info, ClaimLaunchFees<'info>>, from_vault: bool) -> Result<()> {
        instructions::launchlab::handle_claim_launch_fees(ctx, from_vault)
    }

    pub fn sweep_launch_fees(ctx: Context<SweepLaunchFees>) -> Result<()> {
        instructions::launchlab::handle_sweep_launch_fees(ctx)
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

    // ---------- trading
    // Two steps: request (owner or session key) → fill with a price published after the request (keeper)
    pub fn request_open(ctx: Context<RequestOpen>, side: Side, margin: u64, leverage: u16) -> Result<()> {
        instructions::trade::handle_request_open(ctx, side, margin, leverage)
    }

    pub fn request_close(ctx: Context<RequestClose>) -> Result<()> {
        instructions::trade::handle_request_close(ctx)
    }

    pub fn execute_open(ctx: Context<ExecuteOpen>) -> Result<()> {
        instructions::trade::handle_execute_open(ctx)
    }

    pub fn execute_close(ctx: Context<ExecuteClose>) -> Result<()> {
        instructions::trade::handle_execute_close(ctx)
    }

    pub fn cancel_order(ctx: Context<CancelOrder>) -> Result<()> {
        instructions::trade::handle_cancel_order(ctx)
    }

    pub fn liquidate(ctx: Context<Liquidate>) -> Result<()> {
        instructions::trade::handle_liquidate(ctx)
    }

    pub fn harvest<'info>(ctx: Context<'info, Harvest<'info>>) -> Result<()> {
        instructions::queue::handle_harvest(ctx)
    }
}
