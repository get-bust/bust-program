//! LiteSVM harness shared by the integration tests. The program binary comes from
//! `cargo build-sbf` (target/deploy/bust.so).
#![allow(dead_code, unused_imports)]

pub use {
    anchor_lang::{
        prelude::{Clock, Pubkey},
        solana_program::{
            instruction::{AccountMeta, Instruction},
            program_pack::Pack,
            system_instruction, system_program,
        },
        AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
    },
    anchor_spl::token::spl_token,
    bust::{math, Bucket, DistParams, Params, PayoutQueue, Side},
    litesvm::LiteSVM,
    pyth_solana_receiver_sdk::price_update::{PriceFeedMessage, PriceUpdateV2, VerificationLevel},
    solana_account::Account,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
};

pub const NOW: i64 = 1_800_000_000;
pub const FEED: [u8; 32] = [7u8; 32];
pub const ETH_MULT: u64 = 483_979;

pub fn usdc(x: f64) -> u64 {
    (x * 1e6).round() as u64
}

pub fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &bust::id()).0
}
pub fn config() -> Pubkey {
    pda(&[bust::CONFIG_SEED])
}
pub fn vault() -> Pubkey {
    pda(&[bust::VAULT_SEED])
}
pub fn vault_tokens() -> Pubkey {
    pda(&[bust::VAULT_TOKENS_SEED])
}
pub fn deposit_pda(owner: &Pubkey) -> Pubkey {
    pda(&[bust::DEPOSIT_SEED, owner.as_ref()])
}
pub fn market_pda(index: u16) -> Pubkey {
    pda(&[bust::MARKET_SEED, &index.to_le_bytes()])
}
pub fn user_pda(owner: &Pubkey) -> Pubkey {
    pda(&[bust::USER_SEED, owner.as_ref()])
}
pub fn distributor() -> Pubkey {
    pda(&[bust::DISTRIBUTOR_SEED])
}
pub fn stake_pool() -> Pubkey {
    pda(&[bust::STAKE_POOL_SEED])
}
pub fn stake_tokens() -> Pubkey {
    pda(&[bust::STAKE_TOKENS_SEED])
}
pub fn stake_pda(owner: &Pubkey) -> Pubkey {
    pda(&[bust::STAKE_SEED, owner.as_ref()])
}
pub const UNSTAKE_SEC: i64 = 7 * 86_400;
pub const VEST_SEC: i64 = 30 * 86_400;
pub fn treasury() -> Pubkey {
    pda(&[bust::TREASURY_SEED])
}
pub fn treasury_tokens() -> Pubkey {
    pda(&[bust::TREASURY_TOKENS_SEED])
}
pub fn xbust_mint() -> Pubkey {
    pda(&[bust::XBUST_MINT_SEED])
}
pub fn epoch_pda(epoch: u64) -> Pubkey {
    pda(&[bust::EPOCH_SEED, &epoch.to_le_bytes()])
}
pub fn epoch_loss_pda(owner: &Pubkey, epoch: u64) -> Pubkey {
    pda(&[bust::EPOCH_LOSS_SEED, owner.as_ref(), &epoch.to_le_bytes()])
}
pub fn vest_pda(owner: &Pubkey) -> Pubkey {
    pda(&[bust::VEST_SEED, owner.as_ref()])
}
pub fn position_pda(owner: &Pubkey, id: u64) -> Pubkey {
    pda(&[bust::POSITION_SEED, owner.as_ref(), &id.to_le_bytes()])
}
pub fn order_pda(owner: &Pubkey, id: u64) -> Pubkey {
    pda(&[bust::ORDER_SEED, owner.as_ref(), &id.to_le_bytes()])
}

pub fn expect_err(res: Result<(), String>, name: &str) {
    let logs = res.expect_err(&format!("expected {name}"));
    assert!(logs.contains(name), "expected {name}, got:\n{logs}");
}

pub struct Trader {
    pub kp: Keypair,
    pub tokens: Pubkey,
}

pub struct Staker {
    pub kp: Keypair,
    /// $BUST token account
    pub tokens: Pubkey,
    pub usdc: Pubkey,
}

pub struct Env {
    pub svm: LiteSVM,
    pub admin: Keypair,
    pub mint: Pubkey,
    pub queue: Pubkey,
    pub market: Pubkey,
    pub price: Pubkey,
    /// Chain clock (unix seconds) and the last price written
    pub now: i64,
    pub last_price: f64,
    pub level: VerificationLevel,
    pub keeper: Keypair,
    /// Registered wallets the distribution shares go to
    pub buyback_tokens: Pubkey,
    pub dev_tokens: Pubkey,
    pub bust_mint: Pubkey,
    /// Funds the treasury as the buyback authority
    pub buyback: Keypair,
}

impl Env {
    pub fn new() -> Self {
        let mut svm = LiteSVM::new();
        let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/bust.so"));
        svm.add_program(bust::id(), bytes).unwrap();
        let mut clock: Clock = svm.get_sysvar();
        clock.unix_timestamp = NOW;
        svm.set_sysvar(&clock);

        let admin = Keypair::new();
        svm.airdrop(&admin.pubkey(), 100_000_000_000).unwrap();
        let mut env = Env {
            svm,
            admin,
            mint: Pubkey::default(),
            queue: Pubkey::default(),
            market: market_pda(0),
            price: Pubkey::new_unique(),
            now: NOW,
            last_price: 100.0,
            level: VerificationLevel::Full,
            keeper: Keypair::new(),
            buyback_tokens: Pubkey::default(),
            dev_tokens: Pubkey::default(),
            bust_mint: Pubkey::default(),
            buyback: Keypair::new(),
        };
        let keeper = env.keeper.pubkey();
        env.svm.airdrop(&keeper, 10_000_000_000).unwrap();
        env.mint = env.create_mint();
        env.initialize(Params::DEFAULT).unwrap();
        let admin = env.admin.insecure_clone();
        env.add_market(&admin, 0, FEED, ETH_MULT).unwrap();

        env.buyback_tokens = env.token_account(&Pubkey::new_unique(), 0);
        env.dev_tokens = env.token_account(&Pubkey::new_unique(), 0);
        let init = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::InitDistributor { params: DistParams::DEFAULT }.data(),
            bust::accounts::InitDistributor {
                admin: env.admin.pubkey(),
                config: config(),
                vault: vault(),
                distributor: distributor(),
                buyback_tokens: env.buyback_tokens,
                dev_tokens: env.dev_tokens,
                usdc_mint: env.mint,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        env.send(&[init], &[&admin]).unwrap();

        env.bust_mint = env.create_mint();
        let init = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::InitStakePool { unstake_sec: UNSTAKE_SEC }.data(),
            bust::accounts::InitStakePool {
                admin: admin.pubkey(),
                config: config(),
                bust_mint: env.bust_mint,
                stake_pool: stake_pool(),
                stake_tokens: stake_tokens(),
                token_program: spl_token::ID,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        env.send(&[init], &[&admin]).unwrap();

        let buyback = env.buyback.pubkey();
        env.svm.airdrop(&buyback, 1_000_000_000).unwrap();
        let init = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::InitTreasury { vest_sec: VEST_SEC, buyback_authority: buyback }.data(),
            bust::accounts::InitTreasury {
                admin: admin.pubkey(),
                config: config(),
                bust_mint: env.bust_mint,
                treasury: treasury(),
                treasury_tokens: treasury_tokens(),
                xbust_mint: xbust_mint(),
                token_program: spl_token::ID,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        env.send(&[init], &[&admin]).unwrap();
        env.set_price(100.0);
        env
    }

    pub fn initialize(&mut self, params: Params) -> Result<(), String> {
        // The queue account is larger than 10 KB: the client creates it, initialize binds it
        let queue = Keypair::new();
        self.queue = queue.pubkey();
        let rent = self.svm.minimum_balance_for_rent_exemption(PayoutQueue::SPACE);
        let create = system_instruction::create_account(
            &self.admin.pubkey(),
            &queue.pubkey(),
            rent,
            PayoutQueue::SPACE as u64,
            &bust::id(),
        );
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::Initialize { params }.data(),
            bust::accounts::Initialize {
                admin: self.admin.pubkey(),
                usdc_mint: self.mint,
                config: config(),
                vault: vault(),
                vault_tokens: vault_tokens(),
                queue: queue.pubkey(),
                token_program: spl_token::ID,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        let admin = self.admin.insecure_clone();
        self.send(&[create, ix], &[&admin, &queue])
    }

    pub fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), String> {
        let blockhash = self.svm.latest_blockhash();
        let msg = Message::new_with_blockhash(ixs, Some(&signers[0].pubkey()), &blockhash);
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), signers).unwrap();
        let res = self.svm.send_transaction(tx);
        self.svm.expire_blockhash();
        res.map(|_| ()).map_err(|e| format!("{:?}\n{}", e.err, e.meta.logs.join("\n")))
    }

    pub fn create_mint(&mut self) -> Pubkey {
        let mint = Keypair::new();
        let rent = self.svm.minimum_balance_for_rent_exemption(spl_token::state::Mint::LEN);
        let ixs = [
            system_instruction::create_account(
                &self.admin.pubkey(),
                &mint.pubkey(),
                rent,
                spl_token::state::Mint::LEN as u64,
                &spl_token::ID,
            ),
            spl_token::instruction::initialize_mint2(&spl_token::ID, &mint.pubkey(), &self.admin.pubkey(), None, 6).unwrap(),
        ];
        let admin = self.admin.insecure_clone();
        self.send(&ixs, &[&admin, &mint]).unwrap();
        mint.pubkey()
    }

    pub fn token_account(&mut self, owner: &Pubkey, amount: u64) -> Pubkey {
        let mint = self.mint;
        self.token_account_of(&mint, owner, amount)
    }

    pub fn token_account_of(&mut self, mint: &Pubkey, owner: &Pubkey, amount: u64) -> Pubkey {
        let acc = Keypair::new();
        let rent = self.svm.minimum_balance_for_rent_exemption(spl_token::state::Account::LEN);
        let mut ixs = vec![
            system_instruction::create_account(
                &self.admin.pubkey(),
                &acc.pubkey(),
                rent,
                spl_token::state::Account::LEN as u64,
                &spl_token::ID,
            ),
            spl_token::instruction::initialize_account3(&spl_token::ID, &acc.pubkey(), mint, owner).unwrap(),
        ];
        // The xBUST mint authority is the treasury: open empty accounts without minting
        if amount > 0 {
            ixs.push(
                spl_token::instruction::mint_to(&spl_token::ID, mint, &acc.pubkey(), &self.admin.pubkey(), &[], amount).unwrap(),
            );
        }
        let admin = self.admin.insecure_clone();
        self.send(&ixs, &[&admin, &acc]).unwrap();
        acc.pubkey()
    }

    pub fn token_balance(&self, account: &Pubkey) -> u64 {
        spl_token::state::Account::unpack(&self.svm.get_account(account).unwrap().data).unwrap().amount
    }

    pub fn set_time(&mut self, t: i64) {
        self.now = t;
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp = t;
        self.svm.set_sysvar(&clock);
    }

    /// Writes the Pyth PriceUpdateV2 account directly
    pub fn set_price_full(&mut self, price: f64, conf: f64, publish_time: i64) {
        let update = PriceUpdateV2 {
            write_authority: Pubkey::new_unique(),
            verification_level: self.level,
            price_message: PriceFeedMessage {
                feed_id: FEED,
                price: (price * 1e8).round() as i64,
                conf: (conf * 1e8).round() as u64,
                exponent: -8,
                publish_time,
                prev_publish_time: publish_time - 1,
                ema_price: (price * 1e8).round() as i64,
                ema_conf: 0,
            },
            posted_slot: 0,
        };
        let mut data = Vec::new();
        update.try_serialize(&mut data).unwrap();
        self.svm
            .set_account(
                self.price,
                Account { lamports: 10_000_000, data, owner: pyth_solana_receiver_sdk::ID, executable: false, rent_epoch: 0 },
            )
            .unwrap();
    }

    /// Publishes a price at the current time
    pub fn set_price(&mut self, price: f64) {
        self.last_price = price;
        self.set_price_full(price, price * 0.0001, self.now);
    }

    /// Moves the clock one second and republishes the last price at the new time
    pub fn tick(&mut self) {
        self.set_time(self.now + 1);
        let p = self.last_price;
        self.set_price(p);
    }

    pub fn account<T: AccountDeserialize>(&self, key: &Pubkey) -> T {
        T::try_deserialize(&mut self.svm.get_account(key).unwrap().data.as_slice()).unwrap()
    }
    pub fn config_state(&self) -> bust::Config {
        self.account(&config())
    }
    pub fn vault_state(&self) -> bust::Vault {
        self.account(&vault())
    }

    pub fn user_state(&self, owner: &Pubkey) -> bust::UserAccount {
        self.account(&user_pda(owner))
    }

    pub fn dist_state(&self) -> bust::Distributor {
        self.account(&distributor())
    }

    /// Ledger invariant: token balance = cash + user_balances + open_margin + fees_accrued + distribution shares
    pub fn assert_invariant(&self) {
        let v = self.vault_state();
        let d = self.dist_state();
        assert_eq!(self.token_balance(&vault_tokens()), v.cash + v.user_balances + v.open_margin + v.fees_accrued + d.buckets());
    }

    pub fn distribute(&mut self) -> Result<(), String> {
        let c = self.caller();
        let epoch = self.dist_state().epoch;
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::Distribute {}.data(),
            bust::accounts::Distribute {
                caller: c.pubkey(),
                vault: vault(),
                distributor: distributor(),
                stake_pool: stake_pool(),
                epoch_record: epoch_pda(epoch),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&c])
    }

    pub fn record_utilization(&mut self, markets: &[Pubkey]) -> Result<(), String> {
        let c = self.caller();
        let mut metas = bust::accounts::RecordUtilization { config: config(), vault: vault(), distributor: distributor() }
            .to_account_metas(None);
        metas.extend(markets.iter().map(|m| AccountMeta::new_readonly(*m, false)));
        let ix = Instruction::new_with_bytes(bust::id(), &bust::instruction::RecordUtilization {}.data(), metas);
        self.send(&[ix], &[&c])
    }

    pub fn withdraw_bucket(&mut self, bucket: Bucket, destination: Pubkey) -> Result<(), String> {
        let c = self.caller();
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::WithdrawBucket { bucket }.data(),
            bust::accounts::WithdrawBucket {
                config: config(),
                vault: vault(),
                vault_tokens: vault_tokens(),
                distributor: distributor(),
                destination,
                usdc_mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&c])
    }

    /// A staker with $BUST and a stake account
    pub fn staker(&mut self, bust_amount: u64) -> Staker {
        let kp = self.caller();
        let mint = self.bust_mint;
        let tokens = self.token_account_of(&mint, &kp.pubkey(), bust_amount);
        let usdc = self.token_account(&kp.pubkey(), 0);
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::CreateStakeAccount {}.data(),
            bust::accounts::CreateStakeAccount {
                owner: kp.pubkey(),
                payer: kp.pubkey(),
                stake_account: stake_pda(&kp.pubkey()),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&kp]).unwrap();
        Staker { kp, tokens, usdc }
    }

    pub fn move_stake_ix(&self, st: &Staker, data: Vec<u8>) -> Instruction {
        Instruction::new_with_bytes(
            bust::id(),
            &data,
            bust::accounts::MoveStake {
                owner: st.kp.pubkey(),
                stake_pool: stake_pool(),
                stake_tokens: stake_tokens(),
                stake_account: stake_pda(&st.kp.pubkey()),
                owner_tokens: st.tokens,
                bust_mint: self.bust_mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
        )
    }

    pub fn stake(&mut self, st: &Staker, amount: u64) -> Result<(), String> {
        let ix = self.move_stake_ix(st, bust::instruction::Stake { amount }.data());
        self.send(&[ix], &[&st.kp])
    }

    pub fn request_unstake(&mut self, st: &Staker, amount: u64) -> Result<(), String> {
        let ix = self.move_stake_ix(st, bust::instruction::RequestUnstake { amount }.data());
        self.send(&[ix], &[&st.kp])
    }

    pub fn withdraw_unstaked(&mut self, st: &Staker) -> Result<(), String> {
        let ix = self.move_stake_ix(st, bust::instruction::WithdrawUnstaked {}.data());
        self.send(&[ix], &[&st.kp])
    }

    /// The signer may point at `account` instead of its own stake account (attack tests)
    pub fn claim_as(&mut self, signer: &Keypair, account: Pubkey, usdc_dest: Pubkey) -> Result<(), String> {
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::ClaimRewards {}.data(),
            bust::accounts::ClaimRewards {
                owner: signer.pubkey(),
                config: config(),
                vault: vault(),
                vault_tokens: vault_tokens(),
                distributor: distributor(),
                stake_pool: stake_pool(),
                stake_account: account,
                owner_usdc: usdc_dest,
                usdc_mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[signer])
    }

    pub fn claim(&mut self, st: &Staker) -> Result<(), String> {
        let kp = st.kp.insecure_clone();
        self.claim_as(&kp, stake_pda(&st.kp.pubkey()), st.usdc)
    }

    /// The buyback authority (or `signer`) funds the epoch with $BUST
    pub fn fund_epoch_as(&mut self, signer: &Keypair, epoch: u64, amount: u64) -> Result<(), String> {
        let mint = self.bust_mint;
        let src = self.token_account_of(&mint, &signer.pubkey(), amount);
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::FundEpoch { epoch, amount }.data(),
            bust::accounts::FundEpoch {
                authority: signer.pubkey(),
                treasury: treasury(),
                treasury_tokens: treasury_tokens(),
                epoch_record: epoch_pda(epoch),
                authority_bust: src,
                bust_mint: mint,
                xbust_mint: xbust_mint(),
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[signer])
    }

    pub fn fund_epoch(&mut self, epoch: u64, amount: u64) -> Result<(), String> {
        let bb = self.buyback.insecure_clone();
        self.fund_epoch_as(&bb, epoch, amount)
    }

    pub fn xbust_account(&mut self, owner: &Pubkey) -> Pubkey {
        self.token_account_of(&xbust_mint(), owner, 0)
    }

    pub fn claim_xbust(&mut self, owner: &Pubkey, epoch: u64, dest: Pubkey) -> Result<(), String> {
        let rec: bust::EpochLoss = self.account(&epoch_loss_pda(owner, epoch));
        self.claim_xbust_with(owner, epoch, dest, rec.rent_payer)
    }

    /// Claim once the record is closed (its rent payer is unknown, so the keeper is passed)
    pub fn claim_xbust_closed(&mut self, owner: &Pubkey, epoch: u64, dest: Pubkey) -> Result<(), String> {
        let keeper = self.keeper.pubkey();
        self.claim_xbust_with(owner, epoch, dest, keeper)
    }

    fn claim_xbust_with(&mut self, owner: &Pubkey, epoch: u64, dest: Pubkey, rent_payer: Pubkey) -> Result<(), String> {
        let c = self.caller();
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::ClaimXbust { epoch }.data(),
            bust::accounts::ClaimXbust {
                caller: c.pubkey(),
                treasury: treasury(),
                epoch_record: epoch_pda(epoch),
                epoch_loss: epoch_loss_pda(owner, epoch),
                rent_payer,
                xbust_mint: xbust_mint(),
                owner_xbust: dest,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&c])
    }

    pub fn lock_xbust(&mut self, owner: &Keypair, xbust: Pubkey, amount: u64) -> Result<(), String> {
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::LockXbust { amount }.data(),
            bust::accounts::LockXbust {
                owner: owner.pubkey(),
                treasury: treasury(),
                vest: vest_pda(&owner.pubkey()),
                xbust_mint: xbust_mint(),
                owner_xbust: xbust,
                token_program: spl_token::ID,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[owner])
    }

    pub fn convert(&mut self, owner: &Keypair, bust_dest: Pubkey) -> Result<(), String> {
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::ConvertVested {}.data(),
            bust::accounts::ConvertVested {
                owner: owner.pubkey(),
                treasury: treasury(),
                treasury_tokens: treasury_tokens(),
                vest: vest_pda(&owner.pubkey()),
                xbust_mint: xbust_mint(),
                bust_mint: self.bust_mint,
                owner_bust: bust_dest,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[owner])
    }

    /// Moves one epoch ahead (the price is republished at the new time)
    pub fn next_epoch(&mut self) {
        let t = self.now + DistParams::DEFAULT.epoch_sec;
        self.set_time(t);
        let p = self.last_price;
        self.set_price(p);
    }

    pub fn caller(&mut self) -> Keypair {
        let kp = Keypair::new();
        self.svm.airdrop(&kp.pubkey(), 1_000_000_000).unwrap();
        kp
    }

    /// A trader with an account and `deposit` USDC in it
    pub fn trader(&mut self, deposit: u64) -> Trader {
        let kp = Keypair::new();
        self.svm.airdrop(&kp.pubkey(), 10_000_000_000).unwrap();
        let tokens = self.token_account(&kp.pubkey(), deposit);
        let create = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::CreateUser {}.data(),
            bust::accounts::CreateUser {
                owner: kp.pubkey(),
                payer: kp.pubkey(),
                user: user_pda(&kp.pubkey()),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        let t = Trader { kp, tokens };
        let mut ixs = vec![create];
        if deposit > 0 {
            ixs.push(self.deposit_ix(&t, deposit, true));
        }
        self.send(&ixs, &[&t.kp]).unwrap();
        t
    }

    pub fn deposit_ix(&self, t: &Trader, amount: u64, deposit: bool) -> Instruction {
        let accounts = bust::accounts::MoveFunds {
            owner: t.kp.pubkey(),
            config: config(),
            vault: vault(),
            vault_tokens: vault_tokens(),
            user: user_pda(&t.kp.pubkey()),
            owner_tokens: t.tokens,
            usdc_mint: self.mint,
            token_program: spl_token::ID,
        }
        .to_account_metas(None);
        let data =
            if deposit { bust::instruction::Deposit { amount }.data() } else { bust::instruction::Withdraw { amount }.data() };
        Instruction::new_with_bytes(bust::id(), &data, accounts)
    }

    /// Sweeps `tokens` (owned by `owner`'s deposit PDA) into the balance of the user account `credit`
    pub fn sweep_deposit_to(&mut self, credit: &Pubkey, depositor: &Pubkey, tokens: Pubkey) -> Result<(), String> {
        let c = self.caller();
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::SweepDeposit {}.data(),
            bust::accounts::SweepDeposit {
                config: config(),
                vault: vault(),
                vault_tokens: vault_tokens(),
                user: user_pda(credit),
                deposit_authority: deposit_pda(depositor),
                deposit_tokens: tokens,
                usdc_mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&c])
    }

    pub fn sweep_deposit(&mut self, owner: &Pubkey, tokens: Pubkey) -> Result<(), String> {
        self.sweep_deposit_to(owner, owner, tokens)
    }

    pub fn seed(&mut self, amount: u64) {
        let funder = self.caller();
        let tokens = self.token_account(&funder.pubkey(), amount);
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::SeedVault { amount }.data(),
            bust::accounts::SeedVault {
                funder: funder.pubkey(),
                config: config(),
                vault: vault(),
                distributor: distributor(),
                vault_tokens: vault_tokens(),
                funder_tokens: tokens,
                usdc_mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&funder]).unwrap();
    }

    pub fn session_ix(&self, owner: &Keypair, data: Vec<u8>) -> Instruction {
        Instruction::new_with_bytes(
            bust::id(),
            &data,
            bust::accounts::ManageSession { owner: owner.pubkey(), config: config(), user: user_pda(&owner.pubkey()) }
                .to_account_metas(None),
        )
    }

    pub fn set_session(&mut self, owner: &Keypair, session_key: Pubkey, expires_at: i64) -> Result<(), String> {
        let ix = self.session_ix(owner, bust::instruction::SetSession { session_key, expires_at }.data());
        self.send(&[ix], &[owner])
    }

    /// signer: the owner or a session key; the signer pays the order's rent
    pub fn request_open(
        &mut self,
        owner: &Pubkey,
        signer: &Keypair,
        side: Side,
        margin: u64,
        leverage: u16,
    ) -> Result<Pubkey, String> {
        let id = self.user_state(owner).next_order_id;
        let order = order_pda(owner, id);
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::RequestOpen { side, margin, leverage }.data(),
            bust::accounts::RequestOpen {
                authority: signer.pubkey(),
                payer: signer.pubkey(),
                config: config(),
                market: self.market,
                user: user_pda(owner),
                order,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[signer])?;
        Ok(order)
    }

    /// The keeper fills and pays the position's rent
    pub fn execute_open(&mut self, order: &Pubkey) -> Result<Pubkey, String> {
        let o: bust::OrderRequest = self.account(order);
        let position = position_pda(&o.owner, self.user_state(&o.owner).next_position_id);
        let keeper = self.keeper.insecure_clone();
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::ExecuteOpen {}.data(),
            bust::accounts::ExecuteOpen {
                keeper: keeper.pubkey(),
                rent_payer: o.rent_payer,
                config: config(),
                vault: vault(),
                market: o.market,
                user: user_pda(&o.owner),
                order: *order,
                position,
                price_update: self.price,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&keeper])?;
        Ok(position)
    }

    /// Request, then fill with a price published one second later
    pub fn open(&mut self, owner: &Pubkey, signer: &Keypair, side: Side, margin: u64, leverage: u16) -> Result<Pubkey, String> {
        let order = self.request_open(owner, signer, side, margin, leverage)?;
        self.tick();
        self.execute_open(&order)
    }

    pub fn request_close(&mut self, position: &Pubkey, signer: &Keypair) -> Result<Pubkey, String> {
        let p: bust::Position = self.account(position);
        let order = order_pda(&p.owner, self.user_state(&p.owner).next_order_id);
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::RequestClose {}.data(),
            bust::accounts::RequestClose {
                authority: signer.pubkey(),
                payer: signer.pubkey(),
                user: user_pda(&p.owner),
                position: *position,
                order,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[signer])?;
        Ok(order)
    }

    pub fn execute_close(&mut self, order: &Pubkey) -> Result<(), String> {
        let o: bust::OrderRequest = self.account(order);
        let p: bust::Position = self.account(&o.position);
        let keeper = self.keeper.insecure_clone();
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::ExecuteClose {}.data(),
            bust::accounts::ExecuteClose {
                keeper: keeper.pubkey(),
                order_rent_payer: o.rent_payer,
                position_rent_payer: p.rent_payer,
                config: config(),
                vault: vault(),
                market: p.market,
                user: user_pda(&p.owner),
                order: *order,
                position: o.position,
                queue: self.queue,
                price_update: self.price,
                distributor: distributor(),
                epoch_loss: epoch_loss_pda(&p.owner, self.dist_state().epoch),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&keeper])
    }

    /// Close request, then the exit price is published one second later
    pub fn close_at(&mut self, position: &Pubkey, signer: &Keypair, exit_price: f64) -> Result<(), String> {
        let order = self.request_close(position, signer)?;
        self.set_time(self.now + 1);
        self.set_price(exit_price);
        self.execute_close(&order)
    }

    pub fn cancel(&mut self, order: &Pubkey) -> Result<(), String> {
        let o: bust::OrderRequest = self.account(order);
        let caller = self.keeper.insecure_clone();
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::CancelOrder {}.data(),
            bust::accounts::CancelOrder {
                caller: caller.pubkey(),
                config: config(),
                rent_payer: o.rent_payer,
                user: user_pda(&o.owner),
                order: *order,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&caller])
    }

    pub fn liquidate(&mut self, position: &Pubkey) -> Result<(), String> {
        let p: bust::Position = self.account(position);
        let keeper = self.caller();
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::Liquidate {}.data(),
            bust::accounts::Liquidate {
                keeper: keeper.pubkey(),
                rent_payer: p.rent_payer,
                config: config(),
                vault: vault(),
                market: p.market,
                user: user_pda(&p.owner),
                position: *position,
                queue: self.queue,
                price_update: self.price,
                distributor: distributor(),
                epoch_loss: epoch_loss_pda(&p.owner, self.dist_state().epoch),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[&keeper])
    }

    pub fn harvest(&mut self, owners: &[Pubkey]) -> Result<(), String> {
        let keeper = self.caller();
        let mut metas = bust::accounts::Harvest { config: config(), vault: vault(), queue: self.queue }.to_account_metas(None);
        metas.extend(owners.iter().map(|o| AccountMeta::new(user_pda(o), false)));
        let ix = Instruction::new_with_bytes(bust::id(), &bust::instruction::Harvest {}.data(), metas);
        self.send(&[ix], &[&keeper])
    }

    pub fn admin_ix(&self, data: Vec<u8>) -> Instruction {
        Instruction::new_with_bytes(
            bust::id(),
            &data,
            bust::accounts::AdminOnly { admin: self.admin.pubkey(), config: config() }.to_account_metas(None),
        )
    }

    pub fn add_market(&mut self, signer: &Keypair, index: u16, feed_id: [u8; 32], mult: u64) -> Result<(), String> {
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::AddMarket { index, symbol: *b"ETH\0\0\0\0\0", feed_id, position_multiplier_milli: mult }.data(),
            bust::accounts::AddMarket {
                admin: signer.pubkey(),
                config: config(),
                market: market_pda(index),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix], &[signer])
    }

    pub fn update_config(&mut self, signer: &Keypair, params: Params) -> Result<(), String> {
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::UpdateConfig { params }.data(),
            bust::accounts::AdminOnly { admin: signer.pubkey(), config: config() }.to_account_metas(None),
        );
        self.send(&[ix], &[signer])
    }
}
