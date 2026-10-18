//! Local wallet state. Locking drops spending keys; the passphrase-independent
//! farmer key remains available, matching the C++ wallet.
use crate::{error, Result};
use mmx_wallet::{Address, FarmerKey, KeyFile, Wallet};
use std::time::{Duration, Instant};

pub struct Account {
    pub(crate) spending: crate::spending::Spending,
    pub key_file: KeyFile,
    pub account: u32,
    pub name: String,
    pub count: u32,
    addresses: Vec<Address>,
    unlocked: Option<Wallet>,
    last_unlock: Option<Instant>,
    pub timeout: Duration,
}
impl Account {
    pub fn new(key_file: KeyFile, account: u32, count: u32, name: String) -> Result<Self> {
        if !(1..=10000).contains(&count) {
            return Err(error("address count must be 1..10000"));
        }
        let mut out = Self {
            spending: Default::default(),
            key_file,
            account,
            count,
            name,
            addresses: Vec::new(),
            unlocked: None,
            last_unlock: None,
            timeout: Duration::from_secs(600),
        };
        if !out.key_file.requires_passphrase() {
            out.unlock("")?;
            out.lock();
        }
        Ok(out)
    }
    pub fn unlock(&mut self, passphrase: &str) -> Result<()> {
        let wallet = self.key_file.unlock(passphrase, self.account, self.count)?;
        if self
            .addresses
            .first()
            .is_some_and(|a| Some(a) != wallet.addresses.first())
        {
            return Err(error("invalid passphrase"));
        }
        self.addresses = wallet.addresses.clone();
        self.unlocked = Some(wallet);
        self.last_unlock = Some(Instant::now());
        Ok(())
    }
    pub fn lock(&mut self) {
        self.unlocked = None;
        self.last_unlock = None;
    }
    pub fn expire(&mut self) {
        if self
            .last_unlock
            .is_some_and(|t| t.elapsed() >= self.timeout)
        {
            self.lock();
        }
    }
    pub fn is_locked(&mut self) -> bool {
        self.expire();
        self.unlocked.is_none()
    }
    pub fn addresses(&self) -> &[Address] {
        &self.addresses
    }
    pub fn wallet(&mut self) -> Result<&Wallet> {
        self.expire();
        self.unlocked
            .as_ref()
            .ok_or_else(|| error("wallet is locked"))
    }
    pub fn farmer_key(&self) -> Result<FarmerKey> {
        Ok(FarmerKey::derive(&self.key_file.seed_value)?)
    }
    pub fn info(&self, index: u32) -> mmx_types::mmx::account_info_t {
        mmx_types::mmx::account_info_t {
            index: self.account,
            num_addresses: self.count,
            name: self.name.clone(),
            finger_print: self.key_file.fingerprint(),
            with_passphrase: self.key_file.requires_passphrase(),
            address: self.addresses.first().map(|a| a.0.into()),
            account: index,
            ..Default::default()
        }
    }
}
