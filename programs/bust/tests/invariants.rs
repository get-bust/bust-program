//! Random sequences of opens, closes, price moves, liquidations, distributions, seeding and harvests.
//! After every step all ledgers must agree with the tokens and with each other.

mod common;

use common::*;

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

/// These errors are part of the game (cap reached, not liquidatable, epoch not over...); anything else fails the test
fn allowed(res: Result<(), String>) {
    if let Err(e) = res {
        let ok = ["OpenInterestCap", "NotLiquidatable", "QueueNotEmpty", "InsufficientBalance", "EpochNotOver", "MarginTooSmall"];
        assert!(ok.iter().any(|k| e.contains(k)), "unexpected error:\n{e}");
    }
}

fn check_ledgers(env: &Env, owners: &[Pubkey]) {
    env.assert_invariant();
    let v = env.vault_state();
    let users: Vec<bust::UserAccount> = owners.iter().map(|o| env.user_state(o)).collect();
    assert_eq!(users.iter().map(|u| u.balance + u.reserved).sum::<u64>(), v.user_balances, "user balances");
    assert_eq!(users.iter().map(|u| u.queued).sum::<u64>(), v.queue_total + v.queue_unclaimed, "queue");
    assert_eq!(v.queued_cumulative - v.paid_cumulative, v.queue_total, "queue line");
    let d = env.dist_state();
    let losses: u64 = owners
        .iter()
        .filter_map(|o| env.svm.get_account(&epoch_loss_pda(o, d.epoch)))
        .filter(|a| !a.data.is_empty())
        .map(|a| bust::EpochLoss::try_deserialize(&mut a.data.as_slice()).unwrap().loss())
        .sum();
    assert_eq!(losses, d.epoch_loss, "epoch loss = sum of the records");
}

#[test]
fn random_operations_keep_every_ledger_balanced() {
    for seed in [1u64, 7, 42] {
        let mut env = Env::new();
        let mut rng = Lcg(seed);
        env.seed(usdc(2_000.0));
        let traders: Vec<Trader> = (0..4).map(|_| env.trader(usdc(5_000.0))).collect();
        let owners: Vec<Pubkey> = traders.iter().map(|t| t.kp.pubkey()).collect();
        let mut open: Vec<Pubkey> = Vec::new();
        for _ in 0..150 {
            match rng.pick(7) {
                0 | 1 => {
                    let t = &traders[rng.pick(4) as usize];
                    let side = if rng.pick(2) == 0 { Side::Long } else { Side::Short };
                    let margin = usdc(10.0 + rng.pick(190) as f64);
                    let lev = [2u16, 10, 50, 200][rng.pick(4) as usize];
                    let kp = t.kp.insecure_clone();
                    match env.open(&kp.pubkey(), &kp, side, margin, lev) {
                        Ok(p) => open.push(p),
                        Err(e) => allowed(Err(e)),
                    }
                }
                2 if !open.is_empty() => {
                    let p = open.swap_remove(rng.pick(open.len() as u64) as usize);
                    if env.svm.get_account(&p).is_none_or(|a| a.data.is_empty()) {
                        continue;
                    }
                    let owner: bust::Position = env.account(&p);
                    let kp = traders.iter().find(|t| t.kp.pubkey() == owner.owner).unwrap().kp.insecure_clone();
                    let exit = env.last_price * (0.97 + rng.pick(60) as f64 / 1000.0);
                    allowed(env.close_at(&p, &kp, exit));
                }
                3 => {
                    // the price moves and whatever is bust gets liquidated
                    let price = (env.last_price * (0.99 + rng.pick(21) as f64 / 1000.0)).max(50.0);
                    env.tick();
                    env.set_price(price);
                    for p in open.clone() {
                        if env.svm.get_account(&p).is_some_and(|a| !a.data.is_empty()) {
                            allowed(env.liquidate(&p));
                        }
                    }
                }
                4 => {
                    env.next_epoch();
                    allowed(env.distribute());
                }
                5 => env.seed(usdc(50.0 + rng.pick(500) as f64)),
                _ => env.harvest_and_claim(&owners),
            }
            check_ledgers(&env, &owners);
        }
    }
}
