use anchor_lang::prelude::*;

use crate::constants::{DAY_SEC, QUEUE_CAPACITY};

/// Tunable parameters (rates in ppm, ages in seconds)
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug, PartialEq, Eq)]
pub struct Params {
    pub max_leverage: u16,
    /// USDC base units (6 decimals)
    pub min_margin: u64,
    pub win_fee_ppm: u32,
    pub loss_fee_ppm: u32,
    /// Buffer on the bust price (~5 bps = 500)
    pub liq_buffer_ppm: u32,
    /// Dead band that worsens the exit price on a win (1/50000 = 20)
    pub deadband_ppm: u32,
    pub holdback_start_ppm: u32,
    pub holdback_end_ppm: u32,
    /// Impact haircut: (1 − base) / (1 + 1/(move·rate) + ref/(1e6·move·mult))
    pub impact_base_ppm: u32,
    pub impact_rate_multiplier: u32,
    /// USD, whole units
    pub impact_reference_notional: u64,
    /// Open interest cap = net vault × ratio (per market and side)
    pub oi_cap_ratio_ppm: u32,
    pub max_price_age_sec: u64,
    /// Upper bound on Pyth confidence / price
    pub max_conf_ppm: u32,
    pub max_session_sec: i64,
    /// Minimum verified Wormhole signatures on a Pyth update (partial verification; Full always passes)
    pub min_price_signatures: u8,
    /// An order fills with a price published at most this long after the second following the request
    pub max_fill_delay_sec: i64,
    /// An unfilled order can be cancelled and its margin refunded after this long
    pub order_timeout_sec: i64,
}

impl Params {
    pub const DEFAULT: Params = Params {
        max_leverage: 1000,
        min_margin: 1_000_000,
        win_fee_ppm: 20_000,
        loss_fee_ppm: 20_000,
        liq_buffer_ppm: 500,
        deadband_ppm: 20,
        holdback_start_ppm: 75,
        holdback_end_ppm: 600,
        impact_base_ppm: 100_000,
        impact_rate_multiplier: 15_000,
        impact_reference_notional: 100_000,
        oi_cap_ratio_ppm: 600_000,
        max_price_age_sec: 5,
        max_conf_ppm: 2_000,
        max_session_sec: 30 * 24 * 60 * 60,
        // Hermes VAAs carry 3 signatures as of 2026-10 (guardian set 1); all of them are verified
        min_price_signatures: 3,
        max_fill_delay_sec: 5,
        order_timeout_sec: 30,
    };

    pub fn validate(&self) -> bool {
        self.max_leverage >= 1
            && self.min_margin > 0
            && self.win_fee_ppm < 1_000_000
            && self.loss_fee_ppm < 1_000_000
            && self.liq_buffer_ppm < 1_000_000
            && self.holdback_start_ppm < self.holdback_end_ppm
            && self.impact_base_ppm < 1_000_000
            && self.impact_rate_multiplier > 0
            && self.oi_cap_ratio_ppm <= 1_000_000
            && self.max_price_age_sec > 0
            && self.max_session_sec > 0
            && self.min_price_signatures > 0
            && self.max_fill_delay_sec > 0
            && self.order_timeout_sec > self.max_fill_delay_sec
    }
}

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub admin: Pubkey,
    pub usdc_mint: Pubkey,
    /// Stops new positions; closes, liquidations and withdrawals keep working
    pub paused: bool,
    pub params: Params,
    /// The payout queue account (larger than 10 KB, created by the client)
    pub queue: Pubkey,
    pub bump: u8,
}

/// Vault ledger. Invariant: vault_tokens balance = cash + user_balances + open_margin + fees_accrued.
/// queue_total is owed to users and has no tokens behind it; harvest pays it as cash comes in.
#[account]
#[derive(InitSpace)]
pub struct Vault {
    pub cash: u64,
    pub user_balances: u64,
    pub open_margin: u64,
    pub fees_accrued: u64,
    pub queue_total: u64,
    pub bump: u8,
    pub tokens_bump: u8,
}

impl Vault {
    /// Vault net of the queue (may go negative)
    pub fn net_equity(&self) -> i128 {
        self.cash as i128 - self.queue_total as i128
    }
}

/// Epoch distribution. Rates in ppm; the four shares sum to 1e6
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug, PartialEq, Eq)]
pub struct DistParams {
    pub epoch_sec: i64,
    pub burn_ppm: u32,
    pub staker_ppm: u32,
    pub xbust_ppm: u32,
    pub dev_ppm: u32,
    /// Share of vault profit that stays in the vault: u = peak open interest utilisation of the last 7 days,
    /// clamped to this range
    pub keep_min_ppm: u32,
    pub keep_max_ppm: u32,
}

impl DistParams {
    pub const DEFAULT: DistParams = DistParams {
        epoch_sec: DAY_SEC,
        burn_ppm: 400_000,
        staker_ppm: 300_000,
        xbust_ppm: 150_000,
        dev_ppm: 150_000,
        keep_min_ppm: 200_000,
        keep_max_ppm: 800_000,
    };

    pub fn validate(&self) -> bool {
        let sum = self.burn_ppm as u64 + self.staker_ppm as u64 + self.xbust_ppm as u64 + self.dev_ppm as u64;
        self.epoch_sec > 0 && sum == 1_000_000 && self.keep_min_ppm <= self.keep_max_ppm && self.keep_max_ppm <= 1_000_000
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug, PartialEq, Eq)]
pub enum Bucket {
    Burn,
    Staker,
    Xbust,
    Dev,
}

/// Epoch distribution state. Distributed USDC stays in the vault token account until withdrawn, so the invariant is:
/// vault_tokens balance = cash + user_balances + open_margin + fees_accrued + Distributor::buckets()
#[account]
#[derive(InitSpace)]
pub struct Distributor {
    pub params: DistParams,
    pub epoch: u64,
    pub epoch_started_at: i64,
    /// Vault cash after the last distribution (high-water mark); profit is what sits above it. seed_vault raises it too
    pub cash_mark: u64,
    /// Peak open interest utilisation (ppm) per day and the day number; a 7-day ring
    pub util_ppm: [u32; 7],
    pub util_day: [i64; 7],
    pub burn_usdc: u64,
    pub staker_usdc: u64,
    pub xbust_usdc: u64,
    pub dev_usdc: u64,
    /// USDC token accounts of the buyback wallet (burn and xBUST purchases, a multisig) and the developer
    pub buyback_tokens: Pubkey,
    pub dev_tokens: Pubkey,
    pub bump: u8,
}

impl Distributor {
    pub fn buckets(&self) -> u64 {
        self.burn_usdc + self.staker_usdc + self.xbust_usdc + self.dev_usdc
    }

    /// Peak utilisation of the last 7 days
    pub fn peak_util(&self, today: i64) -> u32 {
        (0..7).filter(|&i| self.util_day[i] > today - 7).map(|i| self.util_ppm[i]).max().unwrap_or(0)
    }

    pub fn record_util(&mut self, today: i64, util_ppm: u32) {
        let slot = today.rem_euclid(7) as usize;
        if self.util_day[slot] != today {
            self.util_day[slot] = today;
            self.util_ppm[slot] = 0;
        }
        self.util_ppm[slot] = self.util_ppm[slot].max(util_ppm);
    }
}

/// $BUST staking (Synthetix StakingRewards): each distribution's staker share streams second by second over one epoch.
/// reward_per_token and reward_rate are scaled by SCALE (1e12). The USDC sits in the vault token account (Distributor::staker_usdc)
#[account]
#[derive(InitSpace)]
pub struct StakePool {
    pub bust_mint: Pubkey,
    pub total_staked: u64,
    /// USDC base units / s × SCALE
    pub reward_rate: u128,
    pub period_finish: i64,
    pub last_update: i64,
    /// USDC accrued per $BUST base unit × SCALE
    pub reward_per_token: u128,
    pub unstake_sec: i64,
    pub bump: u8,
    pub tokens_bump: u8,
}

impl StakePool {
    /// Writes the reward streamed so far into reward_per_token
    pub fn update(&mut self, now: i64) -> Option<()> {
        let until = now.min(self.period_finish);
        if self.total_staked > 0 && until > self.last_update {
            let elapsed = (until - self.last_update) as u128;
            let add = elapsed.checked_mul(self.reward_rate)? / self.total_staked as u128;
            self.reward_per_token = self.reward_per_token.checked_add(add)?;
        }
        self.last_update = now.max(self.last_update);
        Some(())
    }

    /// Spreads a new reward (plus what is still streaming) over the next `duration` seconds. Call update first
    pub fn notify(&mut self, amount: u64, now: i64, duration: i64) -> Option<()> {
        let mut total = (amount as u128).checked_mul(crate::constants::SCALE)?;
        if now < self.period_finish {
            total = total.checked_add((self.period_finish - now) as u128 * self.reward_rate)?;
        }
        self.reward_rate = total / duration as u128;
        self.period_finish = now.checked_add(duration)?;
        self.last_update = now;
        Some(())
    }
}

#[account]
#[derive(InitSpace)]
pub struct StakeAccount {
    pub owner: Pubkey,
    pub staked: u64,
    pub reward_per_token_paid: u128,
    /// Unclaimed USDC
    pub rewards: u64,
    /// $BUST in the waiting period (earns nothing)
    pub pending_unstake: u64,
    pub unstake_at: i64,
    pub bump: u8,
}

impl StakeAccount {
    /// Credits what was earned up to the pool's current reward_per_token
    pub fn settle(&mut self, pool: &StakePool) -> Option<()> {
        let delta = pool.reward_per_token.checked_sub(self.reward_per_token_paid)?;
        let earned = (self.staked as u128).checked_mul(delta)? / crate::constants::SCALE;
        self.rewards = self.rewards.checked_add(u64::try_from(earned).ok()?)?;
        self.reward_per_token_paid = pool.reward_per_token;
        Some(())
    }
}

#[account]
#[derive(InitSpace)]
pub struct Market {
    pub index: u16,
    pub symbol: [u8; 8],
    /// Pyth price feed id
    pub feed_id: [u8; 32],
    /// Size parameter of the impact haircut × 1000 (BTC 814.598 → 814_598)
    pub position_multiplier_milli: u64,
    pub oi_long: u64,
    pub oi_short: u64,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug, PartialEq, Eq)]
pub enum Side {
    Long,
    Short,
}

#[account]
#[derive(InitSpace)]
pub struct UserAccount {
    pub owner: Pubkey,
    pub balance: u64,
    /// Margin locked in pending open orders (counted in user_balances)
    pub reserved: u64,
    pub queued: u64,
    /// Pubkey::default() when there is no session
    pub session_key: Pubkey,
    pub session_expires_at: i64,
    pub next_position_id: u64,
    pub next_order_id: u64,
    pub bump: u8,
}

impl UserAccount {
    pub fn can_trade(&self, signer: &Pubkey, now: i64) -> bool {
        *signer == self.owner
            || (self.session_key != Pubkey::default() && *signer == self.session_key && now < self.session_expires_at)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug, PartialEq, Eq)]
pub enum OrderKind {
    Open,
    Close,
}

/// Two-step order: the user writes the request with a timestamp, a keeper fills it with a price
/// published AFTER the request. Nobody gets to pick a stale price they already know.
#[account]
#[derive(InitSpace)]
pub struct OrderRequest {
    pub owner: Pubkey,
    pub id: u64,
    pub kind: OrderKind,
    pub market: Pubkey,
    /// The position a close order closes
    pub position: Pubkey,
    pub side: Side,
    pub margin: u64,
    pub leverage: u16,
    pub requested_at: i64,
    pub rent_payer: Pubkey,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Position {
    pub owner: Pubkey,
    pub id: u64,
    pub market: Pubkey,
    pub side: Side,
    pub margin: u64,
    pub leverage: u16,
    pub notional: u64,
    /// 1e-8 scale
    pub entry_price: u64,
    pub bust_price: u64,
    pub opened_at: i64,
    /// Whoever paid the rent (relayer, keeper or user) gets it back on close
    pub rent_payer: Pubkey,
    pub bump: u8,
}

#[zero_copy]
pub struct QueueEntry {
    pub owner: Pubkey,
    pub amount: u64,
}

/// FIFO ring buffer of winnings the vault could not pay.
/// Larger than 10 KB, so the client creates the account and initialize binds it.
#[account(zero_copy)]
pub struct PayoutQueue {
    pub head: u64,
    pub len: u64,
    pub entries: [QueueEntry; QUEUE_CAPACITY],
}

impl PayoutQueue {
    pub const SPACE: usize = 8 + 8 + 8 + QUEUE_CAPACITY * (32 + 8);

    pub fn push(&mut self, owner: Pubkey, amount: u64) -> bool {
        if self.len as usize >= QUEUE_CAPACITY {
            return false;
        }
        let slot = ((self.head + self.len) as usize) % QUEUE_CAPACITY;
        self.entries[slot] = QueueEntry { owner, amount };
        self.len += 1;
        true
    }

    pub fn front_mut(&mut self) -> Option<&mut QueueEntry> {
        if self.len == 0 {
            return None;
        }
        Some(&mut self.entries[self.head as usize % QUEUE_CAPACITY])
    }

    pub fn pop(&mut self) {
        if self.len > 0 {
            self.head = (self.head + 1) % QUEUE_CAPACITY as u64;
            self.len -= 1;
        }
    }
}
