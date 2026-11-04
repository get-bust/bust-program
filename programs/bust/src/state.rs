use anchor_lang::prelude::*;

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
    pub params: Params,
    pub bump: u8,
}

/// Vault ledger. Invariant: vault_tokens balance = cash + user_balances + open_margin + fees_accrued.
#[account]
#[derive(InitSpace)]
pub struct Vault {
    pub cash: u64,
    pub user_balances: u64,
    pub open_margin: u64,
    pub fees_accrued: u64,
    pub bump: u8,
    pub tokens_bump: u8,
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
    /// Pubkey::default() when there is no session
    pub session_key: Pubkey,
    pub session_expires_at: i64,
    pub bump: u8,
}

impl UserAccount {
    pub fn can_trade(&self, signer: &Pubkey, now: i64) -> bool {
        *signer == self.owner
            || (self.session_key != Pubkey::default() && *signer == self.session_key && now < self.session_expires_at)
    }
}
