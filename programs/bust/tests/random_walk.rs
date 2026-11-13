//! The whole protocol at once: trading, pending orders, deposits, sweeps, distributions, staking and xBUST, in random
//! order and across price shocks. invariants.rs covers the trading ledgers; this walk adds what crosses modules.
//!
//! Every step must leave these true:
//! - every USDC ever minted is accounted for, across the vault and every wallet
//! - the vault's and the distributor's ledgers match the tokens and the sum of the accounts behind them
//! - open interest equals the sum of the open positions, per market and per side
//! - pending notional equals the sum of the waiting open orders, per market and per side
//! - stake tokens back every stake, and the staker bucket covers every reward earned
//! - xBUST never loses backing: a convert or a funding cannot lower the $BUST behind each xBUST
//!
//! At the end everyone must be able to leave: no position, order or debt may be stuck.
//!
//! `BUST_RANDOM_SEEDS` and `BUST_RANDOM_STEPS` scale the run, `BUST_RANDOM_FIRST_SEED` picks where it starts. A failure
//! prints its seed, and rerunning that seed alone reproduces it.

mod common;

use {
    anchor_spl::associated_token::{get_associated_token_address, spl_associated_token_account},
    common::*,
    std::{collections::BTreeMap, thread},
};

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// A small deterministic generator so failures reproduce from the seed
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Prints where a walk died, since a panic inside a seed says nothing about the seed
struct Trail {
    seed: u64,
    ops: Vec<&'static str>,
}
impl Drop for Trail {
    fn drop(&mut self) {
        if thread::panicking() {
            let from = self.ops.len().saturating_sub(12);
            eprintln!("walk failed: seed {} step {}, last ops {:?}", self.seed, self.ops.len(), &self.ops[from..]);
        }
    }
}

fn alive(env: &Env, key: &Pubkey) -> bool {
    env.svm.get_account(key).is_some_and(|a| !a.data.is_empty())
}

/// Refusals the program is meant to give in the middle of a game; any other error fails the walk
fn expected(res: Result<(), String>, names: &[&str]) -> bool {
    match res {
        Ok(()) => true,
        Err(e) => {
            assert!(names.iter().any(|n| e.contains(n)), "unexpected error:\n{e}");
            false
        }
    }
}

const REQUEST: &[&str] = &["OpenInterestCap", "InsufficientBalance", "MarginTooSmall"];
const FILL: &[&str] = &["PythOutage", "MarginTooSmall"];
const CANCEL: &[&str] = &["OrderFillable"];

struct Walk {
    env: Env,
    rng: Lcg,
    traders: Vec<Trader>,
    stakers: Vec<Staker>,
    xbust: BTreeMap<Pubkey, Pubkey>,
    positions: Vec<Pubkey>,
    orders: Vec<Pubkey>,
    /// Sum of all USDC that entered the system since the walk began, on top of the wallets at the start
    usdc_expected: u128,
    /// $BUST and claims of the treasury at the last check
    backing: (u128, u128),
    paid_cumulative: u64,
    queued_cumulative: u64,
}

impl Walk {
    fn new(seed: u64) -> Self {
        // odd seeds trade with the open interest floors on, so debt and the debt brake come into play
        let mut env = if seed % 2 == 1 { Env::with_params(Params::DEFAULT) } else { Env::new() };
        env.seed(usdc(2_000.0));
        let traders: Vec<Trader> = (0..4).map(|_| env.trader(usdc(5_000.0))).collect();
        let stakers: Vec<Staker> = (0..3).map(|_| env.staker(1_000_000_000)).collect();
        let mut xbust = BTreeMap::new();
        for t in &traders {
            let owner = t.kp.pubkey();
            let account = env.xbust_account(&owner);
            xbust.insert(owner, account);
        }
        let mut w = Walk {
            env,
            rng: Lcg(seed),
            traders,
            stakers,
            xbust,
            positions: Vec::new(),
            orders: Vec::new(),
            usdc_expected: 0,
            backing: (0, 0),
            paid_cumulative: 0,
            queued_cumulative: 0,
        };
        w.usdc_expected = w.usdc_total();
        w
    }

    fn owners(&self) -> Vec<Pubkey> {
        self.traders.iter().map(|t| t.kp.pubkey()).collect()
    }

    fn signer(&self, owner: &Pubkey) -> Keypair {
        self.traders.iter().find(|t| t.kp.pubkey() == *owner).unwrap().kp.insecure_clone()
    }

    fn deposit_address(&self, owner: &Pubkey) -> Pubkey {
        get_associated_token_address(&deposit_pda(owner), &self.env.mint)
    }

    fn balance_of(&self, account: &Pubkey) -> u64 {
        if alive(&self.env, account) {
            self.env.token_balance(account)
        } else {
            0
        }
    }

    fn usdc_total(&self) -> u128 {
        let e = &self.env;
        let mut total = e.token_balance(&vault_tokens()) as u128;
        total += e.token_balance(&e.buyback_tokens) as u128 + e.token_balance(&e.dev_tokens) as u128;
        for t in &self.traders {
            total += e.token_balance(&t.tokens) as u128 + self.balance_of(&self.deposit_address(&t.kp.pubkey())) as u128;
        }
        total + self.stakers.iter().map(|s| e.token_balance(&s.usdc) as u128).sum::<u128>()
    }

    fn seed(&mut self, amount: u64) {
        self.env.seed(amount);
        self.usdc_expected += amount as u128;
    }

    fn check(&mut self) {
        let owners = self.owners();
        let e = &self.env;
        e.assert_invariant();
        assert_eq!(self.usdc_total(), self.usdc_expected, "usdc conservation");

        let v = e.vault_state();
        let users: Vec<bust::UserAccount> = owners.iter().map(|o| e.user_state(o)).collect();
        assert_eq!(users.iter().map(|u| u.balance + u.reserved).sum::<u64>(), v.user_balances, "user balances");
        assert_eq!(users.iter().map(|u| u.queued).sum::<u64>(), v.queue_total + v.queue_unclaimed, "queue");
        assert_eq!(v.queued_cumulative - v.paid_cumulative, v.queue_total, "queue line");
        assert!(v.paid_cumulative >= self.paid_cumulative && v.queued_cumulative >= self.queued_cumulative, "queue ran back");
        (self.paid_cumulative, self.queued_cumulative) = (v.paid_cumulative, v.queued_cumulative);

        let d = e.dist_state();
        let losses: u64 = owners
            .iter()
            .map(|o| epoch_loss_pda(o, d.epoch))
            .filter(|k| alive(e, k))
            .map(|k| e.account::<bust::EpochLoss>(&k).loss())
            .sum();
        assert_eq!(losses, d.epoch_loss, "epoch loss");

        let open: Vec<bust::Position> = self.positions.iter().filter(|k| alive(e, k)).map(|k| e.account(k)).collect();
        let side_sum = |side: Side| open.iter().filter(|p| p.side == side).map(|p| p.notional).sum::<u64>();
        let market: bust::Market = e.account(&e.market);
        assert_eq!((market.oi_long, market.oi_short), (side_sum(Side::Long), side_sum(Side::Short)), "market oi");
        assert_eq!((v.oi_long_total, v.oi_short_total), (market.oi_long, market.oi_short), "total oi");
        assert_eq!(open.iter().map(|p| p.margin).sum::<u64>(), v.open_margin, "open margin");

        let waiting: Vec<bust::OrderRequest> = self.orders.iter().map(|k| e.account(k)).collect();
        let pending = |side: Side| {
            let opens = waiting.iter().filter(|o| o.kind == bust::OrderKind::Open && o.side == side);
            opens.map(|o| o.margin * o.leverage as u64).sum::<u64>()
        };
        assert_eq!((market.pending_long, market.pending_short), (pending(Side::Long), pending(Side::Short)), "market pending");
        assert_eq!((v.pending_long_total, v.pending_short_total), (market.pending_long, market.pending_short), "total pending");

        let pool: bust::StakePool = e.account(&stake_pool());
        let stakes: Vec<bust::StakeAccount> = self.stakers.iter().map(|s| e.account(&stake_pda(&s.kp.pubkey()))).collect();
        assert_eq!(stakes.iter().map(|s| s.staked).sum::<u64>(), pool.total_staked, "total staked");
        let held = stakes.iter().map(|s| s.staked + s.pending_unstake).sum::<u64>();
        assert_eq!(e.token_balance(&stake_tokens()), held, "stake tokens");
        let earned: u128 = stakes
            .iter()
            .map(|s| s.rewards as u128 + s.staked as u128 * (pool.reward_per_token - s.reward_per_token_paid) / 1_000_000_000_000)
            .sum();
        assert!(earned <= d.staker_usdc as u128, "rewards {earned} above the bucket {}", d.staker_usdc);

        let treasury: bust::Treasury = e.account(&treasury());
        let mint = spl_token::state::Mint::unpack(&e.svm.get_account(&xbust_mint()).unwrap().data).unwrap();
        let claims = mint.supply as u128 + treasury.total_locked as u128 + treasury.unclaimed as u128;
        let tokens = e.token_balance(&treasury_tokens()) as u128;
        let (old_tokens, old_claims) = self.backing;
        assert!(tokens * old_claims >= old_tokens * claims, "xbust backing fell");
        self.backing = (tokens, claims);
        let locked: u64 = owners
            .iter()
            .map(vest_pda)
            .filter(|k| alive(e, k))
            .map(|k| e.account::<bust::Vest>(&k))
            .map(|v| v.carried + v.amount - v.converted)
            .sum();
        assert_eq!(locked, treasury.total_locked, "vested xbust");
    }

    fn request_open(&mut self) -> Result<Pubkey, String> {
        let t = self.traders[self.rng.pick(4) as usize].kp.insecure_clone();
        let side = if self.rng.pick(2) == 0 { Side::Long } else { Side::Short };
        let margin = usdc(1.0 + self.rng.pick(300) as f64);
        let leverage = [2u16, 10, 50, 200, 1000][self.rng.pick(5) as usize];
        self.env.request_open(&t.pubkey(), &t, side, margin, leverage)
    }

    fn fill_or_cancel(&mut self, order: Pubkey) -> bool {
        let o: bust::OrderRequest = self.env.account(&order);
        if self.env.now < o.fill_at {
            let wait = o.fill_at - self.env.now;
            self.env.set_time(self.env.now + wait);
        }
        // An order the walk left waiting for more than a day (epochs pass in between) can no longer fill. It must
        // cancel with no price, or it would be stuck
        if self.env.now > o.fill_at + bust::ORDER_EXPIRY_SEC {
            self.env.cancel_without_price(&order).unwrap();
            return true;
        }
        // Until then a keeper can fetch an order's fill message from Hermes, however late; now and then Pyth was down
        // at T and the message came too late to fill at
        let gap = if self.rng.pick(8) == 0 { Params::DEFAULT.max_fill_gap_sec + 1 } else { 0 };
        let (account, latest) = (self.env.price, self.env.last_price);
        let mantissa = (latest * 1e8).round() as i64;
        self.env.write_price_after(account, mantissa, o.fill_at + gap, o.fill_at - 1, pyth_solana_receiver_sdk::ID);
        let filled = match o.kind {
            bust::OrderKind::Open => match self.env.execute_open(&order) {
                Ok(position) => {
                    self.positions.push(position);
                    true
                }
                Err(e) => expected(Err(e), FILL),
            },
            bust::OrderKind::Close if alive(&self.env, &o.position) => expected(self.env.execute_close(&order), FILL),
            bust::OrderKind::Close => false,
        };
        if !filled && alive(&self.env, &order) {
            expected(self.env.cancel(&order), CANCEL);
        }
        self.env.set_price(latest);
        if alive(&self.env, &order) {
            self.orders.push(order);
            return false;
        }
        true
    }

    fn liquidate_all(&mut self) {
        for p in self.positions.clone() {
            if alive(&self.env, &p) {
                expected(self.env.liquidate(&p), &["NotLiquidatable", "PythOutage", "PriceConfidence"]);
            }
        }
    }

    fn fund_and_claim_last_epoch(&mut self) {
        let epoch = self.env.dist_state().epoch - 1;
        if !alive(&self.env, &epoch_pda(epoch)) || self.env.account::<bust::EpochRecord>(&epoch_pda(epoch)).total_loss == 0 {
            return;
        }
        let amount = 1_000_000 + self.rng.pick(1_000_000_000);
        if !expected(self.env.fund_epoch(epoch, amount), &["ZeroAmount"]) {
            return;
        }
        for o in self.owners() {
            let record = epoch_loss_pda(&o, epoch);
            if alive(&self.env, &record) && self.env.account::<bust::EpochLoss>(&record).loss() > 0 {
                let dest = self.xbust[&o];
                expected(self.env.claim_xbust(&o, epoch, dest), &["ZeroAmount"]);
            }
        }
        let record: bust::EpochRecord = self.env.account(&epoch_pda(epoch));
        assert!(record.claimed <= record.pool, "xBUST claimed past the pool");
    }

    fn step(&mut self, op: usize) -> bool {
        match OPS[op] {
            "open" => match self.request_open() {
                Ok(order) => self.fill_or_cancel(order),
                Err(e) => expected(Err(e), REQUEST),
            },
            "pending open" => match self.request_open() {
                Ok(order) => {
                    self.orders.push(order);
                    true
                }
                Err(e) => expected(Err(e), REQUEST),
            },
            "pending close" => {
                let live: Vec<Pubkey> = self.positions.iter().filter(|k| alive(&self.env, k)).copied().collect();
                if live.is_empty() {
                    return false;
                }
                let position = live[self.rng.pick(live.len() as u64) as usize];
                let owner = self.env.account::<bust::Position>(&position).owner;
                let signer = self.signer(&owner);
                match self.env.request_close(&position, &signer) {
                    Ok(order) => {
                        self.orders.push(order);
                        true
                    }
                    Err(e) => expected(Err(e), REQUEST),
                }
            }
            "settle order" => {
                if self.orders.is_empty() {
                    return false;
                }
                let order = self.orders.swap_remove(self.rng.pick(self.orders.len() as u64) as usize);
                self.fill_or_cancel(order)
            }
            "close" => {
                let live: Vec<Pubkey> = self.positions.iter().filter(|k| alive(&self.env, k)).copied().collect();
                if live.is_empty() {
                    return false;
                }
                let position = live[self.rng.pick(live.len() as u64) as usize];
                let owner = self.env.account::<bust::Position>(&position).owner;
                let signer = self.signer(&owner);
                let exit = self.env.last_price * (0.97 + self.rng.pick(60) as f64 / 1000.0);
                expected(self.env.close_at(&position, &signer, exit), FILL)
            }
            "price" => {
                let price = (self.env.last_price * (0.99 + self.rng.pick(21) as f64 / 1000.0)).max(50.0);
                self.env.tick();
                self.env.set_price(price);
                self.liquidate_all();
                true
            }
            "shock" => {
                let price = (self.env.last_price * (0.7 + self.rng.pick(61) as f64 / 100.0)).clamp(50.0, 5_000.0);
                self.env.tick();
                self.env.set_price(price);
                self.liquidate_all();
                true
            }
            "distribute" => {
                self.env.next_epoch();
                let done = expected(self.env.distribute(), &["EpochNotOver", "QueueNotEmpty"]);
                if done {
                    self.fund_and_claim_last_epoch();
                }
                done
            }
            "seed" => {
                let amount = usdc(50.0 + self.rng.pick(500) as f64);
                self.seed(amount);
                true
            }
            "harvest" => {
                let owners = self.owners();
                self.env.harvest_and_claim(&owners);
                true
            }
            "deposit" => {
                let t = &self.traders[self.rng.pick(4) as usize];
                let held = self.env.token_balance(&t.tokens);
                if held == 0 {
                    return false;
                }
                let ix = self.env.deposit_ix(t, 1 + self.rng.0 % held, true);
                let kp = t.kp.insecure_clone();
                self.env.send(&[ix], &[&kp]).unwrap();
                true
            }
            "withdraw" => {
                let t = &self.traders[self.rng.pick(4) as usize];
                let balance = self.env.user_state(&t.kp.pubkey()).balance;
                if balance == 0 {
                    return false;
                }
                let ix = self.env.deposit_ix(t, 1 + self.rng.0 % balance, false);
                let kp = t.kp.insecure_clone();
                self.env.send(&[ix], &[&kp]).unwrap();
                true
            }
            "sweep" => {
                let owner = self.traders[self.rng.pick(4) as usize].kp.pubkey();
                let amount = 500_000 + self.rng.pick(100_000_000);
                let ata = self.deposit_address(&owner);
                let admin = self.env.admin.insecure_clone();
                let create = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
                    &admin.pubkey(),
                    &deposit_pda(&owner),
                    &self.env.mint,
                    &spl_token::ID,
                );
                let mint = spl_token::instruction::mint_to(&spl_token::ID, &self.env.mint, &ata, &admin.pubkey(), &[], amount);
                self.env.send(&[create, mint.unwrap()], &[&admin]).unwrap();
                self.usdc_expected += amount as u128;
                expected(self.env.sweep_deposit(&owner, ata), &["DepositTooSmall"])
            }
            "bucket" => {
                let (bucket, dest) = match self.rng.pick(3) {
                    0 => (Bucket::Burn, self.env.buyback_tokens),
                    1 => (Bucket::Xbust, self.env.buyback_tokens),
                    _ => (Bucket::Dev, self.env.dev_tokens),
                };
                expected(self.env.withdraw_bucket(bucket, dest), &["NothingToWithdraw"])
            }
            "stake" => {
                let s = self.rng.pick(3) as usize;
                let held = self.env.token_balance(&self.stakers[s].tokens);
                if held == 0 {
                    return false;
                }
                let amount = 1 + self.rng.0 % held;
                let (env, st) = (&mut self.env, &self.stakers[s]);
                env.stake(st, amount).unwrap();
                true
            }
            "unstake" => {
                let s = self.rng.pick(3) as usize;
                let staked = self.env.account::<bust::StakeAccount>(&stake_pda(&self.stakers[s].kp.pubkey())).staked;
                if staked == 0 {
                    return false;
                }
                let amount = 1 + self.rng.0 % staked;
                let (env, st) = (&mut self.env, &self.stakers[s]);
                env.request_unstake(st, amount).unwrap();
                true
            }
            "withdraw stake" => {
                let s = self.rng.pick(3) as usize;
                let (env, st) = (&mut self.env, &self.stakers[s]);
                expected(env.withdraw_unstaked(st), &["NothingToWithdraw", "UnstakeNotReady"])
            }
            "claim rewards" => {
                let s = self.rng.pick(3) as usize;
                let (env, st) = (&mut self.env, &self.stakers[s]);
                expected(env.claim(st), &["NothingToWithdraw"])
            }
            "lock xbust" => {
                let t = self.traders[self.rng.pick(4) as usize].kp.insecure_clone();
                let account = self.xbust[&t.pubkey()];
                let held = self.env.token_balance(&account);
                if held == 0 {
                    return false;
                }
                let amount = 1 + self.rng.0 % held;
                self.env.lock_xbust(&t, account, amount).unwrap();
                true
            }
            "convert" => {
                let t = self.traders[self.rng.pick(4) as usize].kp.insecure_clone();
                if !alive(&self.env, &vest_pda(&t.pubkey())) {
                    return false;
                }
                let mint = self.env.bust_mint;
                let dest = self.env.token_account_of(&mint, &t.pubkey(), 0);
                expected(self.env.convert(&t, dest), &["NothingToWithdraw"])
            }
            op => unreachable!("{op}"),
        }
    }

    /// Everyone leaves: no position, order or debt may be stuck, and every balance must come out
    fn drain(&mut self) {
        for _ in 0..3 {
            self.env.tick();
            for order in std::mem::take(&mut self.orders) {
                self.fill_or_cancel(order);
            }
        }
        assert!(self.orders.is_empty(), "orders stuck: {:?}", self.orders);

        for position in self.positions.clone() {
            if !alive(&self.env, &position) {
                continue;
            }
            let owner = self.env.account::<bust::Position>(&position).owner;
            let signer = self.signer(&owner);
            let exit = self.env.last_price;
            self.env.close_at(&position, &signer, exit).unwrap_or_else(|e| panic!("position stuck:\n{e}"));
        }
        self.check();

        let v = self.env.vault_state();
        if v.queue_total > v.cash {
            self.seed(v.queue_total - v.cash);
        }
        let owners = self.owners();
        self.env.harvest_and_claim(&owners);
        self.check();
        for i in 0..self.traders.len() {
            let t = &self.traders[i];
            let balance = self.env.user_state(&t.kp.pubkey()).balance;
            if balance > 0 {
                let ix = self.env.deposit_ix(t, balance, false);
                let kp = t.kp.insecure_clone();
                self.env.send(&[ix], &[&kp]).unwrap();
            }
        }
        let v = self.env.vault_state();
        assert_eq!(
            (v.user_balances, v.open_margin, v.queue_total, v.queue_unclaimed, v.oi_long_total, v.oi_short_total),
            (0, 0, 0, 0, 0, 0),
            "something stayed in the vault"
        );

        for i in 0..self.stakers.len() {
            let staked = self.env.account::<bust::StakeAccount>(&stake_pda(&self.stakers[i].kp.pubkey())).staked;
            if staked > 0 {
                let (env, st) = (&mut self.env, &self.stakers[i]);
                env.request_unstake(st, staked).unwrap();
            }
        }
        let t = self.env.now + UNSTAKE_SEC;
        self.env.set_time(t);
        for i in 0..self.stakers.len() {
            let (env, st) = (&mut self.env, &self.stakers[i]);
            expected(env.withdraw_unstaked(st), &["NothingToWithdraw"]);
            expected(env.claim(st), &["NothingToWithdraw"]);
        }
        self.check();
        assert_eq!(self.env.token_balance(&stake_tokens()), 0, "stake left behind");
    }
}

/// Weighted by how often each happens in real use
const OPS: [&str; 20] = [
    "open",
    "pending open",
    "pending close",
    "settle order",
    "close",
    "price",
    "shock",
    "distribute",
    "seed",
    "harvest",
    "deposit",
    "withdraw",
    "sweep",
    "bucket",
    "stake",
    "unstake",
    "withdraw stake",
    "claim rewards",
    "lock xbust",
    "convert",
];

fn walk(seed: u64, steps: u64) -> Vec<u64> {
    let mut w = Walk::new(seed);
    let mut trail = Trail { seed, ops: Vec::new() };
    let mut done = vec![0u64; OPS.len()];
    for _ in 0..steps {
        let op = match w.rng.pick(OPS.len() as u64 + 6) as usize {
            n if n < OPS.len() => n,
            20 | 21 => 0,
            22 => 4,
            23 => 5,
            24 => 3,
            _ => 7,
        };
        trail.ops.push(OPS[op]);
        if w.step(op) {
            done[op] += 1;
        }
        w.check();
    }
    trail.ops.push("drain");
    w.drain();
    done
}

#[test]
fn the_whole_protocol_survives_random_use_and_everyone_can_leave() {
    let (first, seeds, steps) =
        (env_u64("BUST_RANDOM_FIRST_SEED", 1), env_u64("BUST_RANDOM_SEEDS", 8), env_u64("BUST_RANDOM_STEPS", 400));
    let workers = thread::available_parallelism().map_or(1, |n| n.get()) as u64;
    let done = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers.min(seeds))
            .map(|w| {
                thread::Builder::new()
                    .stack_size(16 << 20)
                    .spawn_scoped(scope, move || {
                        let mut done = vec![0u64; OPS.len()];
                        for seed in (first..first + seeds).skip(w as usize).step_by(workers as usize) {
                            done.iter_mut().zip(walk(seed, steps)).for_each(|(a, b)| *a += b);
                        }
                        done
                    })
                    .unwrap()
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
    });
    let total: Vec<u64> = (0..OPS.len()).map(|i| done.iter().map(|d| d[i]).sum()).collect();
    println!("completed operations: {:?}", OPS.iter().zip(&total).collect::<Vec<_>>());
    // a walk where something never happens proves nothing about it
    for (op, n) in OPS.iter().zip(&total) {
        assert!(*n > 0, "{op} never succeeded");
    }
}
