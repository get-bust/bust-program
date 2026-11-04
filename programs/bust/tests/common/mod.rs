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
    bust::{math, Params, Side},
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
pub fn market_pda(index: u16) -> Pubkey {
    pda(&[bust::MARKET_SEED, &index.to_le_bytes()])
}
pub fn user_pda(owner: &Pubkey) -> Pubkey {
    pda(&[bust::USER_SEED, owner.as_ref()])
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

pub struct Env {
    pub svm: LiteSVM,
    pub admin: Keypair,
    pub mint: Pubkey,
    pub market: Pubkey,
    pub price: Pubkey,
    /// Chain clock (unix seconds) and the last price written
    pub now: i64,
    pub last_price: f64,
    pub level: VerificationLevel,
    pub keeper: Keypair,
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
            market: market_pda(0),
            price: Pubkey::new_unique(),
            now: NOW,
            last_price: 100.0,
            level: VerificationLevel::Full,
            keeper: Keypair::new(),
        };
        let keeper = env.keeper.pubkey();
        env.svm.airdrop(&keeper, 10_000_000_000).unwrap();
        env.mint = env.create_mint();
        env.initialize(Params::DEFAULT).unwrap();
        let admin = env.admin.insecure_clone();
        env.add_market(&admin, 0, FEED, ETH_MULT).unwrap();
        env.set_price(100.0);
        env
    }

    pub fn initialize(&mut self, params: Params) -> Result<(), String> {
        let ix = Instruction::new_with_bytes(
            bust::id(),
            &bust::instruction::Initialize { params }.data(),
            bust::accounts::Initialize {
                admin: self.admin.pubkey(),
                usdc_mint: self.mint,
                config: config(),
                vault: vault(),
                vault_tokens: vault_tokens(),
                token_program: spl_token::ID,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        let admin = self.admin.insecure_clone();
        self.send(&[ix], &[&admin])
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

    /// Ledger invariant: token balance = cash + user_balances + open_margin + fees_accrued
    pub fn assert_invariant(&self) {
        let v = self.vault_state();
        assert_eq!(self.token_balance(&vault_tokens()), v.cash + v.user_balances + v.open_margin + v.fees_accrued);
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
                price_update: self.price,
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
