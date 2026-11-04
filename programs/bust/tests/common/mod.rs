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
    bust::Params,
    litesvm::LiteSVM,
    solana_account::Account,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
};

pub const NOW: i64 = 1_800_000_000;

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

pub fn expect_err(res: Result<(), String>, name: &str) {
    let logs = res.expect_err(&format!("expected {name}"));
    assert!(logs.contains(name), "expected {name}, got:\n{logs}");
}

pub struct Env {
    pub svm: LiteSVM,
    pub admin: Keypair,
    pub mint: Pubkey,
    /// Chain clock (unix seconds)
    pub now: i64,
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
        let mut env = Env { svm, admin, mint: Pubkey::default(), now: NOW };
        env.mint = env.create_mint();
        env.initialize(Params::DEFAULT).unwrap();
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

    pub fn account<T: AccountDeserialize>(&self, key: &Pubkey) -> T {
        T::try_deserialize(&mut self.svm.get_account(key).unwrap().data.as_slice()).unwrap()
    }
    pub fn config_state(&self) -> bust::Config {
        self.account(&config())
    }
    pub fn vault_state(&self) -> bust::Vault {
        self.account(&vault())
    }

    pub fn admin_ix(&self, data: Vec<u8>) -> Instruction {
        Instruction::new_with_bytes(
            bust::id(),
            &data,
            bust::accounts::AdminOnly { admin: self.admin.pubkey(), config: config() }.to_account_metas(None),
        )
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
