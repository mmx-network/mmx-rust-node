//! Native MMX key derivation, compatible key files and signed transfers.
//!
//! No node runtime, RPC transport, CLI state or C/C++ wallet bindings are used.
mod address;
mod amount;
mod fee;
mod keyfile;
mod keys;
mod mnemonic;
#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
mod spend;
mod transaction;

pub use address::Address;
pub use amount::{format_amount, parse_amount};
pub use fee::static_fee;
pub use keyfile::KeyFile;
pub use keys::{fingerprint, FarmerKey, Wallet};
pub use mnemonic::{seed_to_words, words_to_seed};
pub use transaction::{ChainParams, Input, Output, Solution, Transaction, TransferOptions};

/// Confirmed balances keyed by (owner address, currency contract).
pub type Balances = std::collections::BTreeMap<(Address, Address), u128>;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[cfg(feature = "native")]
    #[error(transparent)]
    Native(#[from] mmx_consensus::Error),
    #[error("{0}")]
    InvalidArgument(String),
    #[error("invalid payment amount: {0}")]
    InvalidAmount(&'static str),
    #[error("invalid mnemonic recovery words")]
    InvalidMnemonic,
    #[error("invalid wallet passphrase")]
    InvalidPassphrase,
    #[error("invalid or unsupported wallet key file")]
    InvalidKeyFile,
    #[error("invalid signed transfer: {0}")]
    InvalidTransaction(&'static str),
    #[error("insufficient funds: {0}")]
    InsufficientFunds(&'static str),
}

pub(crate) fn sha256(data: impl AsRef<[u8]>) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(data).into()
}

pub(crate) mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer, const N: usize>(v: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<[u8; N], D::Error> {
        let text = String::deserialize(d)?;
        let bytes = hex::decode(text).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("invalid hex length"))
    }
}
pub(crate) mod atomic {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &u128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let text = match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            _ => return Err(serde::de::Error::custom("expected atomic amount")),
        };
        text.parse().map_err(serde::de::Error::custom)
    }
}
