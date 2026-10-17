use super::*;
use k256::ecdsa::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Copy, PartialEq, Eq)]
struct SignatureEntry {
    message: [u8; 32],
    key: [u8; 33],
    signature: [u8; 64],
}
static SIGNATURE_CACHE: LazyLock<Mutex<Vec<SignatureEntry>>> = LazyLock::new(|| {
    Mutex::new(vec![
        SignatureEntry {
            message: [0; 32],
            key: [0; 33],
            signature: [0; 64]
        };
        16384
    ])
});

fn signature_slot(signature: &[u8; 64]) -> usize {
    let mut hash = vnx::Crc64::new();
    hash.update(vnx::hash64(signature).to_le_bytes());
    // For fixed-length CRC inputs, replacing the native random salt by zero
    // XOR-permutes all slot indices. Collisions and cache behavior are identical,
    // including eviction of the initially zero entry; no RNG is required.
    hash.update([0; 8]);
    hash.as_u64() as usize % 16384
}

/// Staged transaction output. Memos are bytes because native VM strings need
/// not be UTF-8. A transaction executor publishes outputs only on success.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Output {
    pub contract: Address,
    pub address: Address,
    pub amount: u128,
    pub memo: Option<Vec<u8>>,
}

/// Trusted node services. Callbacks share the VM gas meter and may read VM
/// values, copy return values, or invoke another engine. Errors abort execution.
/// Disabled LOG/EVENT deliberately do not read their message/name operands.
pub trait Host<S: Storage> {
    fn logs_enabled(&self) -> bool {
        false
    }
    fn events_enabled(&self) -> bool {
        false
    }
    fn remote_calls_enabled(&self) -> bool {
        false
    }
    fn contract_reads_enabled(&self) -> bool {
        false
    }
    fn log(&mut self, _vm: &mut Engine<'_, S>, _level: u32, _message: &[u8]) -> Result<()> {
        Ok(())
    }
    fn event(&mut self, _vm: &mut Engine<'_, S>, _name: &[u8], _data: u64) -> Result<()> {
        Ok(())
    }
    /// Arguments start at stack_address()+1; the return slot is stack_address().
    fn remote_call(
        &mut self,
        _vm: &mut Engine<'_, S>,
        _name: &[u8],
        _method: &[u8],
        _nargs: u32,
    ) -> Result<()> {
        Err(Error::HostUnavailable("remote calls"))
    }
    fn read_contract(
        &mut self,
        _vm: &mut Engine<'_, S>,
        _address: &Address,
        _field: &[u8],
        _dst: u64,
    ) -> Result<()> {
        Err(Error::HostUnavailable("contract reads"))
    }
}
pub struct NoHost;
impl<S: Storage> Host<S> for NoHost {}

impl<S: Storage> Engine<'_, S> {
    pub fn stack_address(&self) -> Result<u64> {
        Ok(MEM_STACK
            + self
                .frames
                .last()
                .ok_or(Error::State("empty call stack"))?
                .stack)
    }

    /// Host work uses the same meter as bytecode instructions.
    pub fn charge_gas(&mut self, amount: u64) -> Result<()> {
        self.charge(amount)
    }

    /// Transfers remain private until execution has completed successfully.
    pub fn outputs(&self) -> Result<(&[Output], &[Output])> {
        if self.failed || !self.initialized || !self.frames.is_empty() {
            return Err(Error::State(
                "outputs require a healthy initialized idle engine",
            ));
        }
        Ok((&self.outputs, &self.mint_outputs))
    }

    pub(super) fn string(&mut self, address: u64) -> Result<Vec<u8>> {
        match self.load(address)? {
            Value::String(v) => Ok(v),
            _ => Err(Error::Type("string")),
        }
    }
    fn binary(&mut self, address: u64) -> Result<Vec<u8>> {
        match self.load(address)? {
            Value::Binary(v) => Ok(v),
            _ => Err(Error::Type("binary")),
        }
    }
    pub(super) fn binary_address(&mut self, address: u64) -> Result<Address> {
        self.binary(address)?
            .try_into()
            .map_err(|_| Error::Type("32-byte address"))
    }

    pub(super) fn verify(&mut self, dst: u64, msg: u64, pubkey: u64, signature: u64) -> Result<()> {
        self.charge(20_000)?;
        let signature: [u8; 64] = self
            .binary(signature)?
            .try_into()
            .map_err(|_| Error::Signature)?;
        let msg: [u8; 32] = self.binary(msg)?.try_into().map_err(|_| Error::Signature)?;
        let pubkey: [u8; 33] = self
            .binary(pubkey)?
            .try_into()
            .map_err(|_| Error::Signature)?;
        let entry = SignatureEntry {
            message: msg,
            key: pubkey,
            signature,
        };
        let slot = signature_slot(&signature);
        // Native signature_t checks its zero-initialized cache before parsing.
        let verified = if SIGNATURE_CACHE.lock().unwrap()[slot] == entry {
            true
        } else {
            const ORDER: [u8; 32] = [
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c,
                0xd0, 0x36, 0x41, 0x41,
            ];
            if signature[..32] >= ORDER[..] || signature[32..] >= ORDER[..] {
                return Err(Error::Signature);
            }
            let key = VerifyingKey::from_sec1_bytes(&pubkey).map_err(|_| Error::Signature)?;
            // libsecp256k1 parses zero scalars, then verification returns false.
            match Signature::from_slice(&signature) {
                Ok(sig) => {
                    let verified = key.verify_prehash(&msg, &sig).is_ok();
                    if verified {
                        SIGNATURE_CACHE.lock().unwrap()[slot] = entry;
                    }
                    verified
                }
                Err(_) => false,
            }
        };
        self.write_value(dst, Value::Bool(verified), false)
    }

    pub(super) fn balance(&mut self, currency: u64) -> Result<(Address, u128)> {
        let currency = self.binary_address(currency)?;
        if let Some(balance) = self.balances.get(&currency) {
            return Ok((currency, *balance));
        }
        self.spend(WRITE_COST);
        self.charge(STORAGE_READ_COST)?;
        let balance = self
            .backend
            .get_balance(&self.contract, &currency)?
            .unwrap_or(0);
        self.balances.insert(currency, balance);
        Ok((currency, balance))
    }
    fn memo(&mut self, address: u64) -> Result<Option<Vec<u8>>> {
        match self.load(address)? {
            Value::Nil => Ok(None),
            Value::String(v) if v.len() <= 64 => Ok((!v.is_empty()).then_some(v)),
            _ => Err(Error::Transfer(
                "memo must be nil or a string of at most 64 bytes",
            )),
        }
    }
    pub(super) fn send(
        &mut self,
        address: u64,
        amount: u64,
        currency: u64,
        memo: u64,
    ) -> Result<()> {
        let amount = self.uint(amount)?;
        if amount.is_zero() {
            return Ok(());
        }
        if amount.bits() > 128 {
            return Err(Error::Transfer("amount exceeds 128 bits"));
        }
        if currency == 0 {
            return Err(Error::Transfer("currency == null"));
        }
        let (currency, balance) = self.balance(currency)?;
        let amount = amount.low_u128();
        if balance < amount {
            return Err(Error::Transfer("insufficient funds"));
        }
        self.balances.insert(currency, balance - amount);
        let address = self.binary_address(address)?;
        let memo = self.memo(memo)?;
        self.outputs.push(Output {
            contract: currency,
            address,
            amount,
            memo,
        });
        Ok(())
    }
    pub(super) fn mint(&mut self, address: u64, amount: u64, memo: u64) -> Result<()> {
        let amount = self.uint(amount)?;
        if amount.is_zero() {
            return Ok(());
        }
        if amount.bits() > 80 {
            return Err(Error::Transfer("mint amount exceeds 80 bits"));
        }
        let address = self.binary_address(address)?;
        let memo = self.memo(memo)?;
        self.mint_outputs.push(Output {
            contract: self.contract,
            address,
            amount: amount.low_u128(),
            memo,
        });
        Ok(())
    }
}
