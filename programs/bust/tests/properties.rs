//! Property tests for the pure parts of the program: settlement and distribution math, the staking stream,
//! xBUST unlocks and queue tickets. Each property is checked on thousands of random inputs.

use bust::{math, DistParams, Params, Side, StakeAccount, StakePool, Ticket, Vest};
use proptest::prelude::*;

const SCALE: u128 = 1_000_000_000_000;

fn side() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::Long), Just(Side::Short)]
}

/// Prices from $0.0001 to $1,000,000 at 1e-8
fn price() -> impl Strategy<Value = u64> {
    10_000u64..100_000_000_000_000
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4_000))]

    // ---------- settlement math

    #[test]
    fn bust_price_sits_on_the_losing_side(side in side(), entry in price(), lev in 1u16..=1000) {
        let p = Params::DEFAULT;
        let bust = math::bust_price(side, entry, lev, p.liq_buffer_ppm);
        match side {
            Side::Long => prop_assert!(bust < entry),
            Side::Short => prop_assert!(bust > entry),
        }
        prop_assert!(!math::is_bust(side, entry, bust), "a position never opens already bust");
    }

    /// For any parameters the config accepts, a new position is never already bust
    #[test]
    fn valid_params_never_open_a_bust_position(side in side(), entry in price(), max_lev in 1u16..=u16::MAX, buf_frac in 0.0f64..1.0, lev_frac in 0.0f64..=1.0) {
        // any buffer up to the largest the validation accepts for this leverage
        let buffer = ((999_999 / max_lev as u64) as f64 * buf_frac) as u32;
        let p = Params { max_leverage: max_lev, liq_buffer_ppm: buffer, ..Params::DEFAULT };
        prop_assert!(p.validate());
        let lev = ((max_lev as f64 * lev_frac).round() as u16).clamp(1, max_lev);
        let bust = math::bust_price(side, entry, lev, p.liq_buffer_ppm);
        prop_assert!(!math::is_bust(side, entry, bust), "lev {lev} buffer {buffer}");
    }

    #[test]
    fn a_bust_position_has_lost_its_margin_less_the_buffer(side in side(), entry in price(), lev in 1u16..=1000, margin in 1_000_000u64..10_000_000_000) {
        let p = Params::DEFAULT;
        let notional = margin * lev as u64;
        let bust = math::bust_price(side, entry, lev, p.liq_buffer_ppm);
        let loss = -math::raw_pnl(side, entry, bust, notional);
        // At the bust price the loss is the margin minus the buffer, never more than the margin
        prop_assert!(loss <= margin as i128);
        let buffer = notional as i128 * p.liq_buffer_ppm as i128 / 1_000_000;
        // 1/leverage is rounded down at a 1e12 scale and the bust price to 1e-8: together they move the loss by
        // up to notional / 1e12 + notional / entry base units, a few millionths of a USDC
        let rounding = notional as i128 / 1_000_000_000_000 + notional as i128 / entry as i128 + 3;
        prop_assert!(loss >= margin as i128 - buffer - rounding, "loss {loss} margin {margin} buffer {buffer}");
    }

    #[test]
    fn raw_pnl_is_antisymmetric(entry in price(), exit in price(), notional in 0u64..1_000_000_000_000_000) {
        let long = math::raw_pnl(Side::Long, entry, exit, notional);
        let short = math::raw_pnl(Side::Short, entry, exit, notional);
        prop_assert_eq!(long, -short);
    }

    #[test]
    fn a_win_never_pays_more_than_the_raw_profit(
        entry in price(),
        up in 0u64..10_000_000,          // up to +1000% at 1e-4 steps
        notional in 0u64..1_000_000_000_000_000,
        mult in 1u64..10_000_000,
    ) {
        let p = Params::DEFAULT;
        let exit = (entry as u128 * (10_000_000 + up as u128) / 10_000_000) as u64;
        let w = math::win_breakdown(entry, exit, notional, mult, &p);
        let raw = math::raw_pnl(Side::Long, entry, exit, notional);
        prop_assert!(w.gain as i128 <= raw);
        prop_assert_eq!(w.net + w.fee, w.gain);
        prop_assert!(w.fee as u128 <= w.gain as u128 * p.win_fee_ppm as u128 / 1_000_000 + 1);
    }

    #[test]
    fn a_bigger_move_never_pays_less(entry in price(), a in 0u64..10_000_000, b in 0u64..10_000_000, notional in 0u64..1_000_000_000_000) {
        let p = Params::DEFAULT;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let at = |up: u64| (entry as u128 * (10_000_000 + up as u128) / 10_000_000) as u64;
        // Outside the holdback band the payout grows with the move
        let band_end = p.holdback_end_ppm as u64 * 10; // ppm → 1e-7 steps
        prop_assume!(lo >= band_end);
        let (wl, wh) = (math::win_breakdown(entry, at(lo), notional, 483_979, &p), math::win_breakdown(entry, at(hi), notional, 483_979, &p));
        prop_assert!(wh.net + 1 >= wl.net, "lo {lo} {} hi {hi} {}", wl.net, wh.net);
    }

    /// Any prices and size: a close must never panic, or a winning position could not be closed
    #[test]
    fn settlement_math_never_panics(side in side(), entry in 1u64..u64::MAX, exit in 1u64..u64::MAX, notional in any::<u64>(), mult in 1u64..u64::MAX) {
        let p = Params::DEFAULT;
        let raw = math::raw_pnl(side, entry, exit, notional);
        let w = math::win_breakdown(entry, exit, notional, mult, &p);
        prop_assert_eq!(w.net + w.fee, w.gain);
        // raw_pnl saturates only past ~10^38 (notional and move both near u64::MAX); below that the win stays under it
        if (notional as i128).checked_mul(exit as i128 - entry as i128).is_some_and(|x| x.abs() < i128::MAX / 2) {
            prop_assert!(raw <= 0 || w.gain as i128 <= raw);
        }
    }

    #[test]
    fn moves_past_the_cap_pay_as_the_cap(entry in 1_000_000u64..1_000_000_000_000, notional in 0u64..1_000_000_000_000_000, extra in 0u64..1_000_000) {
        let p = Params::DEFAULT;
        let at_cap = entry * 1_002; // clearly past 1000× once the dead band is taken off
        let w_cap = math::win_breakdown(entry, at_cap, notional, 483_979, &p);
        let w_far = math::win_breakdown(entry, at_cap.saturating_add(extra * entry), notional, 483_979, &p);
        prop_assert_eq!(w_cap, w_far);
    }

    #[test]
    fn fees_never_exceed_the_amount(amount in any::<u64>(), fee_ppm in 0u32..1_000_000) {
        prop_assert!(math::fee_of(amount, fee_ppm) <= amount);
    }

    // ---------- distribution math

    #[test]
    fn the_split_adds_up_to_the_pool(profit in 0u64..u64::MAX / 4, fees in 0u64..u64::MAX / 4, keep in 0u32..=1_000_000) {
        let s = math::split(profit, fees, keep, &DistParams::DEFAULT);
        prop_assert!(s.from_cash <= profit);
        prop_assert_eq!(s.pool, s.from_cash + fees);
        prop_assert_eq!(s.burn + s.staker + s.xbust + s.dev, s.pool);
    }

    #[test]
    fn utilization_is_a_share(long in any::<u64>(), short in any::<u64>(), cap in any::<u64>()) {
        prop_assert!(math::utilization(long, short, cap) <= 1_000_000);
    }

    #[test]
    fn the_oi_cap_never_exceeds_the_vault(equity in any::<i64>(), ratio in 0u32..=1_000_000) {
        let cap = math::oi_cap(equity as i128, ratio);
        prop_assert!(cap as i128 <= equity.max(0) as i128);
    }

    // ---------- queue tickets

    #[test]
    fn tickets_on_one_line_are_paid_in_order(amounts in prop::collection::vec(1u64..1_000_000_000, 1..40), paid in 0u64..40_000_000_000) {
        let mut start = 0;
        let tickets: Vec<Ticket> = amounts
            .iter()
            .enumerate()
            .map(|(i, &amount)| {
                let t = Ticket { owner: Default::default(), id: i as u64, start, amount, claimed: 0, rent_payer: Default::default(), bump: 0 };
                start += amount;
                t
            })
            .collect();
        let total: u64 = amounts.iter().sum();
        let paid_out: u64 = tickets.iter().map(|t| t.paid(paid)).sum();
        prop_assert_eq!(paid_out, paid.min(total), "the queue pays exactly what was harvested");
        // A ticket is only paid once every ticket before it is paid in full
        for w in tickets.windows(2) {
            if w[1].paid(paid) > 0 {
                prop_assert_eq!(w[0].paid(paid), w[0].amount);
            }
        }
    }

    // ---------- xBUST unlock

    #[test]
    fn a_lock_unlocks_linearly_and_never_more_than_it_holds(amount in 0u64..u64::MAX / 2, vest_sec in 1i64..100_000_000, steps in prop::collection::vec(0i64..10_000_000, 1..20)) {
        let mut v = Vest { owner: Default::default(), amount, start: 0, converted: 0, carried: 0, bump: 0 };
        let mut now = 0i64;
        let mut out = 0u64;
        for dt in steps {
            now += dt;
            let c = v.convertible(now, vest_sec);
            out += c;
            v.converted = v.vested(now, vest_sec);
            prop_assert!(out <= amount);
        }
        let all = v.convertible(vest_sec.max(now) + vest_sec, vest_sec);
        prop_assert_eq!(out + all, amount, "everything unlocks by the end");
    }
}

// ---------- staking stream (a model run, not a single function)

#[derive(Debug, Clone)]
enum Op {
    Stake(usize, u64),
    Unstake(usize, u64),
    Notify(u64),
    Wait(i64),
    Claim(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0usize..4, 1u64..1_000_000_000_000).prop_map(|(i, a)| Op::Stake(i, a)),
        (0usize..4, 1u64..1_000_000_000_000).prop_map(|(i, a)| Op::Unstake(i, a)),
        (1u64..10_000_000_000).prop_map(Op::Notify),
        (1i64..200_000).prop_map(Op::Wait),
        (0usize..4).prop_map(Op::Claim),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// Whatever stakers do, they are never paid more than the rewards put into the pool
    #[test]
    fn stakers_never_earn_more_than_was_streamed(ops in prop::collection::vec(op(), 1..80)) {
        let epoch = 86_400;
        let mut pool = StakePool {
            bust_mint: Default::default(),
            total_staked: 0,
            reward_rate: 0,
            period_finish: 0,
            last_update: 0,
            reward_per_token: 0,
            unstake_sec: 1,
            bump: 0,
            tokens_bump: 0,
        };
        let mut accounts: Vec<StakeAccount> = (0..4)
            .map(|_| StakeAccount { owner: Default::default(), staked: 0, reward_per_token_paid: 0, rewards: 0, pending_unstake: 0, unstake_at: 0, bump: 0 })
            .collect();
        let (mut now, mut funded, mut paid) = (1i64, 0u128, 0u128);
        for op in ops {
            match op {
                Op::Stake(i, a) => {
                    pool.update(now).unwrap();
                    accounts[i].settle(&pool).unwrap();
                    accounts[i].staked += a;
                    pool.total_staked += a;
                }
                Op::Unstake(i, a) => {
                    pool.update(now).unwrap();
                    accounts[i].settle(&pool).unwrap();
                    let a = a.min(accounts[i].staked);
                    accounts[i].staked -= a;
                    pool.total_staked -= a;
                }
                Op::Notify(a) => {
                    pool.update(now).unwrap();
                    // as in distribute: with nobody staked the share is burned, not streamed
                    if pool.total_staked > 0 {
                        pool.notify(a, now, epoch).unwrap();
                        funded += a as u128;
                    }
                }
                Op::Wait(dt) => now += dt,
                Op::Claim(i) => {
                    pool.update(now).unwrap();
                    accounts[i].settle(&pool).unwrap();
                    paid += std::mem::take(&mut accounts[i].rewards) as u128;
                }
            }
            let owed: u128 = accounts.iter().map(|a| a.rewards as u128).sum();
            prop_assert!(paid + owed <= funded, "paid {paid} + owed {owed} > funded {funded}");
        }
        let _ = SCALE;
    }
}
