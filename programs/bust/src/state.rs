use anchor_lang::prelude::*;

use crate::constants::DAY_SEC;

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
            // the buffer must stay inside the margin at max leverage, or positions would open already bust
            && (self.liq_buffer_ppm as u64) * (self.max_leverage as u64) < 1_000_000
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
    pub bump: u8,
}

/// Vault ledger. Invariant: vault_tokens balance = cash + user_balances + open_margin + fees_accrued + queue_unclaimed.
///
/// The payout queue is two ever-growing counters on one line, like Lido's withdrawal queue. Each win the vault cannot
/// cover takes the interval [queued_cumulative, queued_cumulative + amount) on it as a Ticket, and harvest moves
/// paid_cumulative forward with cash. A ticket is paid for the part of its interval below paid_cumulative, so wins are
/// paid strictly in order, with no list to store and no limit on how many wait.
#[account]
#[derive(InitSpace)]
pub struct Vault {
    pub cash: u64,
    pub user_balances: u64,
    pub open_margin: u64,
    pub fees_accrued: u64,
    /// Owed to ticket holders and not paid yet; no tokens behind it
    pub queue_total: u64,
    /// Paid to tickets and not yet claimed into balances; the tokens are in the vault
    pub queue_unclaimed: u64,
    /// Every amount ever queued, and every amount ever paid against the queue
    pub queued_cumulative: u64,
    pub paid_cumulative: u64,
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
    /// Sum of users' net losses this epoch (wins netted out); moves to the EpochRecord when the epoch closes
    pub epoch_loss: u64,
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
    /// Reward that streamed while nobody was staked (× SCALE). It goes back into the next distribution's stream
    pub orphaned: u128,
}

impl StakePool {
    /// Writes the reward streamed so far into reward_per_token, or into `orphaned` while nobody is staked
    pub fn update(&mut self, now: i64) -> Option<()> {
        let until = now.min(self.period_finish);
        if until > self.last_update {
            let streamed = ((until - self.last_update) as u128).checked_mul(self.reward_rate)?;
            if self.total_staked > 0 {
                self.reward_per_token = self.reward_per_token.checked_add(streamed / self.total_staked as u128)?;
            } else {
                self.orphaned = self.orphaned.checked_add(streamed)?;
            }
        }
        self.last_update = now.max(self.last_update);
        Some(())
    }

    /// Whole USDC base units of the orphaned reward, removed from it; the sub-unit remainder stays
    pub fn take_orphaned(&mut self) -> u64 {
        let units = self.orphaned / crate::constants::SCALE;
        self.orphaned -= units * crate::constants::SCALE;
        units as u64
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

/// xBUST treasury: the $BUST bought for conversion sits here.
/// Rate = treasury $BUST / (xBUST supply + locked xBUST + funded but unclaimed xBUST)
#[account]
#[derive(InitSpace)]
pub struct Treasury {
    pub xbust_mint: Pubkey,
    pub bust_mint: Pubkey,
    /// Owner of the buyback wallet (a multisig on mainnet): the only one who can fund an epoch
    pub buyback_authority: Pubkey,
    /// xBUST locked (burned) but not yet converted to $BUST
    pub total_locked: u64,
    pub vest_sec: i64,
    pub bump: u8,
    pub tokens_bump: u8,
    pub mint_bump: u8,
    /// xBUST rights in funded epochs that nobody has claimed yet. Without them in the denominator an early converter
    /// would also take the backing of losers who have not claimed. Last field: older accounts grow via migrate_treasury
    pub unclaimed: u64,
}

impl Treasury {
    /// Account size before unclaimed was added
    pub const V1_LEN: usize = 8 + 32 * 3 + 8 + 8 + 3;
}

/// A closed epoch: total net loss and the xBUST rights opened for it (the $BUST put in the treasury × the current rate)
#[account]
#[derive(InitSpace)]
pub struct EpochRecord {
    pub epoch: u64,
    pub total_loss: u64,
    pub pool: u64,
    pub funded: bool,
    pub claimed: u64,
    pub bump: u8,
}

impl EpochRecord {
    /// Whether the record can still be funded, given the distributor's current epoch
    pub fn in_funding_window(&self, current_epoch: u64) -> bool {
        // epoch N closes when the distributor moves to N + 1
        current_epoch.saturating_sub(self.epoch + 1) < crate::constants::FUNDING_WINDOW_EPOCHS
    }
}

/// A user's net result in one epoch (updated on close and liquidation). Closed when claimed
#[account]
#[derive(InitSpace)]
pub struct EpochLoss {
    pub owner: Pubkey,
    pub epoch: u64,
    /// Win +, loss − (USDC base units)
    pub net: i64,
    pub rent_payer: Pubkey,
    pub bump: u8,
}

impl EpochLoss {
    pub fn loss(&self) -> u64 {
        if self.net < 0 {
            self.net.unsigned_abs()
        } else {
            0
        }
    }
}

/// xBUST lock: the locked amount unlocks linearly over vest_sec. A new lock merges with what is left and restarts the clock;
/// the part already unlocked by then is kept as `carried`
#[account]
#[derive(InitSpace)]
pub struct Vest {
    pub owner: Pubkey,
    pub amount: u64,
    pub start: i64,
    pub converted: u64,
    pub carried: u64,
    pub bump: u8,
}

impl Vest {
    /// Unlocked so far (converted included)
    pub fn vested(&self, now: i64, vest_sec: i64) -> u64 {
        let elapsed = (now - self.start).clamp(0, vest_sec) as u128;
        (self.amount as u128 * elapsed / vest_sec as u128) as u64
    }

    pub fn convertible(&self, now: i64, vest_sec: i64) -> u64 {
        self.carried + self.vested(now, vest_sec) - self.converted
    }
}

/// Bust's LaunchLab platform. The program's fee wallet PDA claims the platform fee
#[account]
#[derive(InitSpace)]
pub struct LaunchConfig {
    pub launchpad_program: Pubkey,
    /// Our PlatformConfig account (platform_claim_fee_wallet = the fee wallet PDA)
    pub platform_config: Pubkey,
    pub quote_mint: Pubkey,
    /// Where claimed fees go when the quote token is not USDC (the swap wallet, a multisig)
    pub swap_destination: Pubkey,
    pub bump: u8,
    pub fee_wallet_bump: u8,
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
    /// Still owed on this user's open tickets, paid or not, until claimed into the balance
    pub queued: u64,
    /// Pubkey::default() when there is no session
    pub session_key: Pubkey,
    pub session_expires_at: i64,
    pub next_position_id: u64,
    pub next_order_id: u64,
    pub next_ticket_id: u64,
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

/// One queued win: the interval [start, start + amount) on the queue line, and how much of it was claimed
#[account]
#[derive(InitSpace)]
pub struct Ticket {
    pub owner: Pubkey,
    pub id: u64,
    pub start: u64,
    pub amount: u64,
    pub claimed: u64,
    /// The keeper that closed the position paid the rent and gets it back when the ticket closes
    pub rent_payer: Pubkey,
    pub bump: u8,
}

impl Ticket {
    /// How much of the ticket the queue has paid so far
    pub fn paid(&self, paid_cumulative: u64) -> u64 {
        paid_cumulative.saturating_sub(self.start).min(self.amount)
    }
}
