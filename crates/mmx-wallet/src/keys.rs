use crate::{sha256, Address, Error, Result};
use hmac::{Hmac, Mac};
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use sha2::Sha512;
use zeroize::{Zeroize, Zeroizing};

pub fn fingerprint(seed: &[u8; 32], passphrase: Option<&str>) -> String {
    let pass_hash = passphrase.map_or([0; 32], |p| {
        let text = Zeroizing::new(format!("MMX/fingerprint/{p}"));
        sha256(text.as_bytes())
    });
    let mut data = Zeroizing::new([0u8; 96]);
    data[32..64].copy_from_slice(seed);
    data[64..].copy_from_slice(&pass_hash);
    for _ in 0..16384 {
        let hash = sha256(data.as_slice());
        data[..32].copy_from_slice(&hash);
    }
    u32::from_le_bytes(data[..4].try_into().unwrap()).to_string()
}
fn hmac(seed: &[u8], key: &[u8], index: Option<u32>) -> Zeroizing<[u8; 64]> {
    let mut mac = Hmac::<Sha512>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(seed);
    if let Some(i) = index {
        mac.update(&i.to_be_bytes());
    }
    Zeroizing::new(mac.finalize().into_bytes().into())
}
/// An unlocked wallet; private signing keys are cleared by k256 on drop.
pub struct Wallet {
    pub(crate) keys: Vec<SigningKey>,
    pub addresses: Vec<Address>,
}
impl Wallet {
    pub fn derive(seed: &[u8; 32], passphrase: &str, account: u32, count: u32) -> Result<Self> {
        if *seed == [0; 32] || !(1..=10000).contains(&count) {
            return Err(Error::InvalidArgument(
                "nonzero seed and 1..10000 addresses required".into(),
            ));
        }
        let mut pass = Zeroizing::new(format!("MMX/seed/{passphrase}"));
        let pass_hash = Zeroizing::new(sha256(pass.as_bytes()));
        pass.zeroize();
        let mut master = hmac(seed, &*pass_hash, None);
        for _ in 1..4096 {
            master = hmac(seed, &*master, None);
        }
        let chain = hmac(&master[..32], &master[32..], Some(11337));
        let account = hmac(&chain[..32], &chain[32..], Some(account));
        let mut keys = Vec::new();
        let mut addresses = Vec::new();
        for i in 0..count {
            let key = hmac(&account[..32], &account[32..], Some(i));
            let signing = SigningKey::from_slice(&key[..32]).map_err(|_| {
                Error::InvalidArgument("derived secp256k1 key is out of range".into())
            })?;
            addresses.push(Address(sha256(
                signing.verifying_key().to_encoded_point(true).as_bytes(),
            )));
            keys.push(signing);
        }
        Ok(Self { keys, addresses })
    }
}

/// Passphrase-independent farmer identity, matching ECDSA_Wallet::create_farmer_key.
/// Never implements Debug or Serialize; k256 clears private scalars on drop.
pub struct FarmerKey(SigningKey);
impl FarmerKey {
    pub fn derive(seed: &[u8; 32]) -> Result<Self> {
        if *seed == [0; 32] {
            return Err(Error::InvalidArgument("seed == zero".into()));
        }
        let salt = sha256(b"MMX/farmer_keys");
        let mut master = hmac(seed, &salt, None);
        for _ in 1..4096 {
            master = hmac(seed, &*master, None);
        }
        let key = hmac(&master[..32], &master[32..], Some(0));
        Self::from_bytes(&key[..32])
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self(SigningKey::from_slice(bytes).map_err(|_| {
            Error::InvalidArgument("invalid farmer secret key".into())
        })?))
    }
    pub fn public_key(&self) -> [u8; 33] {
        self.0
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap()
    }
    pub fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes().into())
    }
    pub fn sign(&self, hash: &[u8; 32]) -> Result<[u8; 64]> {
        let sig: Signature = self
            .0
            .sign_prehash(hash)
            .map_err(|_| Error::InvalidArgument("farmer signing failed".into()))?;
        Ok(sig.to_bytes().into())
    }
}
impl Wallet {
    pub fn public_key(&self, offset: usize) -> Result<[u8; 33]> {
        let key = self
            .keys
            .get(offset)
            .ok_or_else(|| Error::InvalidArgument("address index out of range".into()))?;
        Ok(key
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap())
    }
    pub fn sign_message(&self, address: Address, hash: &[u8; 32]) -> Result<crate::Solution> {
        let index = self
            .addresses
            .iter()
            .position(|a| *a == address)
            .ok_or_else(|| Error::InvalidArgument("address not owned by wallet".into()))?;
        let sig: Signature = self.keys[index]
            .sign_prehash(hash)
            .map_err(|_| Error::InvalidArgument("signing failed".into()))?;
        Ok(crate::Solution {
            type_name: "mmx.solution.PubKey".into(),
            version: 0,
            pubkey: self.public_key(index)?,
            signature: sig.to_bytes().into(),
        })
    }
}
