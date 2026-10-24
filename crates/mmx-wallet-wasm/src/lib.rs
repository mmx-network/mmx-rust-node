//! WebAssembly bindings of the MMX wallet core for the web wallet.
//!
//! Errors are thrown as `Error` objects whose message is a stable code
//! (`invalid_mnemonic`, `passphrase_required`, …). Recovery words, passphrases,
//! seeds and keys are only held inside WASM memory and cleared when released;
//! private keys are never returned.
mod api;

use js_sys::JsString;
use wasm_bindgen::prelude::*;
use zeroize::Zeroizing;

impl From<api::Code> for JsError {
    fn from(code: api::Code) -> Self {
        JsError::new(code.0)
    }
}

/// Copies a secret into a JavaScript string and clears the WASM copy.
fn secret(value: Zeroizing<String>) -> JsString {
    JsString::from(value.as_str())
}

/// 24 new recovery words from the browser's secure random source.
#[wasm_bindgen]
pub fn generate_mnemonic() -> Result<JsString, JsError> {
    Ok(secret(api::generate_mnemonic()?))
}

/// Lowercase, single-spaced recovery words; throws `invalid_mnemonic`.
#[wasm_bindgen]
pub fn normalize_mnemonic(words: String) -> Result<JsString, JsError> {
    let words = Zeroizing::new(words);
    Ok(secret(api::normalize_mnemonic(&words)?))
}

/// Wallet fingerprint of recovery words with an optional passphrase.
#[wasm_bindgen]
pub fn fingerprint(words: String, passphrase: Option<String>) -> Result<String, JsError> {
    let words = Zeroizing::new(words);
    let passphrase = passphrase.map(Zeroizing::new);
    Ok(api::fingerprint_of(
        &words,
        passphrase.as_deref().map(String::as_str),
    )?)
}

/// True for a canonical (lowercase) MMX address.
#[wasm_bindgen]
pub fn validate_address(address: &str) -> bool {
    api::validate_address(address)
}

/// Decimal text to atomic units; throws `invalid_amount`.
#[wasm_bindgen]
pub fn parse_amount(text: &str, decimals: u32) -> Result<String, JsError> {
    Ok(api::parse_amount(text, decimals)?)
}

/// Atomic units to decimal text without trailing zeros.
#[wasm_bindgen]
pub fn format_amount(atomic: &str, decimals: u32) -> Result<String, JsError> {
    Ok(api::format_amount(atomic, decimals)?)
}

/// Fee in atomic MMX for a transfer with `inputs` inputs, `owners` signatures,
/// one output with `memo`, at priority `fee_level` (1, 2, 3, 5 or 10).
#[wasm_bindgen]
pub fn static_fee(
    params_json: &str,
    inputs: u32,
    owners: u32,
    memo: Option<String>,
    fee_level: u32,
) -> Result<String, JsError> {
    Ok(api::static_fee(
        params_json,
        inputs,
        owners,
        memo,
        fee_level,
    )?)
}

/// Checks hashes, cost and signatures of a signed transfer; throws on failure.
#[wasm_bindgen]
pub fn verify_transaction(tx_json: &str, params_json: &str) -> Result<(), JsError> {
    Ok(api::verify_transaction(tx_json, params_json)?)
}

/// Addresses of any account (parity tests only).
#[cfg(feature = "test-nonce")]
#[wasm_bindgen]
pub fn derive_addresses(
    words: String,
    passphrase: Option<String>,
    account: u32,
    count: u32,
) -> Result<Vec<String>, JsError> {
    let words = Zeroizing::new(words);
    let passphrase = passphrase.map(Zeroizing::new);
    Ok(api::derive_addresses(
        &words,
        passphrase.as_deref().map(String::as_str),
        account,
        count,
    )?)
}

/// An unlocked wallet. Call `free()` to clear it from memory.
#[wasm_bindgen]
pub struct Vault(api::VaultCore);

#[wasm_bindgen]
impl Vault {
    /// Opens recovery words. `expected_fingerprint` is the wallet's known
    /// fingerprint, or undefined for a first import. Throws
    /// `passphrase_required`, `invalid_passphrase`, `invalid_mnemonic` or
    /// `invalid_argument` (count outside 1..=100).
    pub fn open(
        words: String,
        passphrase: Option<String>,
        expected_fingerprint: Option<String>,
        count: u32,
    ) -> Result<Vault, JsError> {
        let words = Zeroizing::new(words);
        let passphrase = passphrase.map(Zeroizing::new);
        Ok(Vault(api::VaultCore::open(
            &words,
            passphrase.as_deref().map(String::as_str),
            expected_fingerprint.as_deref(),
            count,
        )?))
    }

    pub fn fingerprint(&self) -> String {
        self.0.fingerprint()
    }

    pub fn with_passphrase(&self) -> bool {
        self.0.with_passphrase()
    }

    pub fn addresses(&self) -> Vec<String> {
        self.0.addresses()
    }

    /// Re-derives `count` addresses (1..=100).
    pub fn set_count(&mut self, count: u32) -> Result<(), JsError> {
        Ok(self.0.set_count(count)?)
    }

    pub fn mnemonic(&self) -> JsString {
        secret(self.0.mnemonic())
    }

    /// Largest sendable amount of `currency` in atomic units.
    pub fn max_amount(
        &self,
        balances_json: &str,
        params_json: &str,
        currency: &str,
        memo: Option<String>,
        fee_level: u32,
    ) -> Result<String, JsError> {
        Ok(self
            .0
            .max_amount(balances_json, params_json, currency, memo, fee_level)?)
    }

    /// Signed transfer JSON, ready for `/transaction/validate` and
    /// `/transaction/broadcast`.
    #[allow(clippy::too_many_arguments)]
    pub fn sign_transfer(
        &self,
        balances_json: &str,
        params_json: &str,
        height: u32,
        to: &str,
        currency: &str,
        amount_atomic: &str,
        memo: Option<String>,
        fee_level: u32,
        expire_delta: u32,
    ) -> Result<String, JsError> {
        Ok(self.0.sign_transfer(api::Transfer {
            balances_json,
            params_json,
            height,
            to,
            currency,
            amount: amount_atomic,
            memo,
            fee_level,
            expire_delta,
            nonce: None,
        })?)
    }

    /// `sign_transfer` with a fixed nonce (parity tests only).
    #[cfg(feature = "test-nonce")]
    #[allow(clippy::too_many_arguments)]
    pub fn sign_transfer_with_nonce(
        &self,
        balances_json: &str,
        params_json: &str,
        height: u32,
        to: &str,
        currency: &str,
        amount_atomic: &str,
        memo: Option<String>,
        fee_level: u32,
        expire_delta: u32,
        nonce: u64,
    ) -> Result<String, JsError> {
        Ok(self.0.sign_transfer(api::Transfer {
            balances_json,
            params_json,
            height,
            to,
            currency,
            amount: amount_atomic,
            memo,
            fee_level,
            expire_delta,
            nonce: Some(nonce),
        })?)
    }
}
