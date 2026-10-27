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
    cache_path: Option<std::path::PathBuf>,
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
            timeout: Duration::from_secs(300),
            cache_path: None,
        };
        if !out.key_file.requires_passphrase() {
            out.unlock("")?;
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
        let old = std::mem::replace(&mut self.addresses, wallet.addresses.clone());
        if let Err(e) = self.save_addresses() {
            self.addresses = old;
            return Err(e);
        }
        self.unlocked = Some(wallet);
        self.last_unlock = Some(Instant::now());
        Ok(())
    }
    pub fn lock(&mut self) {
        if !self.key_file.requires_passphrase() {
            return;
        }
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
        &self.addresses[..self.addresses.len().min(self.count as usize)]
    }
    pub fn wallet(&mut self) -> Result<&Wallet> {
        self.expire();
        self.unlocked
            .as_ref()
            .ok_or_else(|| error("wallet is locked"))
    }
    pub fn touch_spending(&mut self) {
        if self.unlocked.is_some() {
            self.last_unlock = Some(Instant::now());
        }
    }

    pub fn set_count(&mut self, count: u32, passphrase: Option<&str>) -> Result<()> {
        if !(1..=10000).contains(&count) {
            return Err(error("address count must be 1..10000"));
        }
        if count as usize > self.addresses.len() {
            if self.key_file.requires_passphrase() && passphrase.is_none_or(str::is_empty) {
                return Err(error("passphrase required to derive missing addresses"));
            }
            let wallet = self
                .key_file
                .unlock(passphrase.unwrap_or(""), self.account, count)?;
            if self
                .addresses
                .iter()
                .zip(&wallet.addresses)
                .any(|(a, b)| a != b)
            {
                return Err(error("cached addresses do not match the wallet"));
            }
            let old = std::mem::replace(&mut self.addresses, wallet.addresses.clone());
            if let Err(e) = self.save_addresses() {
                self.addresses = old;
                return Err(e);
            }
            if self.unlocked.is_some() {
                self.unlocked = Some(wallet);
                self.last_unlock = Some(Instant::now());
            }
        }
        if self
            .unlocked
            .as_ref()
            .is_some_and(|w| w.addresses.len() < count as usize)
        {
            self.lock();
        }
        self.count = count;
        Ok(())
    }

    pub fn set_cache_path(&mut self, path: std::path::PathBuf) -> Result<()> {
        use std::io::Read;
        use vnx::VnxType;
        if let Ok(file) = std::fs::File::open(&path) {
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 1024 * 1024 {
                return Err(error("address cache exceeds size limit"));
            }
            let file = mmx_types::mmx::WalletFile::decode(&bytes)?;
            let addresses: Vec<_> = file.addresses.iter().map(|a| Address(a.0)).collect();
            if addresses.len() > 10000
                || addresses.iter().any(|a| *a == Address::default())
                || addresses
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != addresses.len()
            {
                return Err(error("invalid address cache"));
            }
            if !self.addresses.is_empty()
                && self.addresses.iter().zip(&addresses).any(|(a, b)| a != b)
            {
                return Err(error("address cache does not match the wallet"));
            }
            if addresses.len() > self.addresses.len() {
                self.addresses = addresses;
            }
        }
        self.cache_path = Some(path);
        if !self.addresses.is_empty() {
            self.save_addresses()?;
        }
        Ok(())
    }

    fn save_addresses(&self) -> Result<()> {
        use std::io::Write;
        use vnx::VnxType;
        let Some(path) = &self.cache_path else {
            return Ok(());
        };
        let file = mmx_types::mmx::WalletFile {
            addresses: self.addresses.iter().map(|a| a.0.into()).collect(),
        };
        let bytes = file.encode(&mmx_types::vnx_registry()?)?;
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        tmp.write_all(&bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spending_resets_timeout_and_lock_keeps_public_cache_and_farmer_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("addresses");
        let key = KeyFile::new([7; 32], Some("secret"));
        let mut account = Account::new(key, 2, 3, "test".into()).unwrap();
        assert_eq!(account.timeout, Duration::from_secs(300));
        account.set_cache_path(path.clone()).unwrap();
        account.unlock("secret").unwrap();
        let public = account.addresses().to_vec();
        let farmer = account.farmer_key().unwrap().public_key();
        account.last_unlock = Some(Instant::now() - Duration::from_secs(299));
        account.touch_spending();
        assert!(!account.is_locked());
        assert!(account.last_unlock.unwrap().elapsed() < Duration::from_secs(1));
        account.last_unlock = Some(Instant::now() - Duration::from_secs(301));
        assert!(account.is_locked());
        assert!(account.wallet().is_err());
        assert_eq!(account.addresses(), public);
        assert_eq!(account.farmer_key().unwrap().public_key(), farmer);
        assert!(account.set_count(4, None).is_err());
        assert!(account.set_count(4, Some("wrong")).is_err());
        account.set_count(4, Some("secret")).unwrap();
        assert!(
            account.is_locked(),
            "deriving addresses must not start a spending session"
        );
        account.lock();
        account.set_count(2, None).unwrap();
        account.set_count(4, None).unwrap();
        let mut restarted =
            Account::new(KeyFile::new([7; 32], Some("secret")), 2, 4, "test".into()).unwrap();
        restarted.set_cache_path(path).unwrap();
        assert!(restarted.is_locked());
        assert_eq!(&restarted.addresses()[..3], public);
        assert_eq!(restarted.addresses().len(), 4);
    }

    #[test]
    fn regular_disk_wallet_remains_usable_after_lock_and_timeout() {
        let mut account = Account::new(KeyFile::new([7; 32], None), 0, 2, "test".into()).unwrap();
        account.timeout = Duration::ZERO;
        account.lock();
        assert!(!account.is_locked());
        assert!(account.wallet().is_ok());
    }
}
