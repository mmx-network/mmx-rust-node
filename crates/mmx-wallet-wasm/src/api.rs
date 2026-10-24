//! Platform-independent implementation of the browser API. Every value coming
//! from JavaScript is validated here again before anything is derived or signed.
use mmx_wallet::{
    fingerprint, seed_to_words, words_to_seed, Address, Balances, ChainParams, Output, Transaction,
    TransferOptions, Wallet,
};
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

/// Stable error code returned to JavaScript as the error message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Code(pub &'static str);

pub const INVALID_PASSPHRASE: Code = Code("invalid_passphrase");
pub const PASSPHRASE_REQUIRED: Code = Code("passphrase_required");
pub const INVALID_MNEMONIC: Code = Code("invalid_mnemonic");
pub const INVALID_ADDRESS: Code = Code("invalid_address");
pub const INVALID_AMOUNT: Code = Code("invalid_amount");
pub const INVALID_MEMO: Code = Code("invalid_memo");
pub const INSUFFICIENT_FUNDS: Code = Code("insufficient_funds");
pub const INSUFFICIENT_FEE: Code = Code("insufficient_fee");
pub const INVALID_ARGUMENT: Code = Code("invalid_argument");
pub const TX_REJECTED: Code = Code("tx_rejected");
pub const INTERNAL: Code = Code("internal");

pub type Res<T> = Result<T, Code>;

/// A seed on the heap, so moving its owner never copies the secret bytes.
type Seed = Box<Zeroizing<[u8; 32]>>;

pub const MAX_ADDRESSES: u32 = 100;
pub const MEMO_MAX_BYTES: usize = 64;

fn wallet_error(e: mmx_wallet::Error) -> Code {
    use mmx_wallet::Error as E;
    #[allow(unreachable_patterns)]
    match e {
        E::InvalidMnemonic => INVALID_MNEMONIC,
        E::InvalidPassphrase => INVALID_PASSPHRASE,
        E::InvalidAmount(_) => INVALID_AMOUNT,
        E::InsufficientFunds("transaction fee") => INSUFFICIENT_FEE,
        E::InsufficientFunds(_) => INSUFFICIENT_FUNDS,
        E::InvalidArgument(_) | E::InvalidKeyFile => INVALID_ARGUMENT,
        _ => INTERNAL,
    }
}

/// Canonical (lowercase `mmx1…`) address encoding only.
fn parse_address(text: &str) -> Res<Address> {
    let address: Address = text.parse().map_err(|_| INVALID_ADDRESS)?;
    if address.to_string() != text {
        return Err(INVALID_ADDRESS);
    }
    Ok(address)
}

/// Decimal atomic units without sign, separators or exponent.
fn parse_atomic(text: &str) -> Res<u128> {
    if text.is_empty() || text.len() > 39 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(INVALID_AMOUNT);
    }
    text.parse().map_err(|_| INVALID_AMOUNT)
}

/// An empty memo means no memo.
fn parse_memo(memo: Option<String>) -> Res<Option<String>> {
    match memo {
        Some(m) if m.len() > MEMO_MAX_BYTES => Err(INVALID_MEMO),
        Some(m) if !m.is_empty() => Ok(Some(m)),
        _ => Ok(None),
    }
}

/// Priority levels offered by the wallet: 1×, 2×, 3×, 5× and 10× the minimum.
fn fee_ratio(level: u32) -> Res<u32> {
    match level {
        1 | 2 | 3 | 5 | 10 => Ok(level * 1024),
        _ => Err(INVALID_ARGUMENT),
    }
}

/// The raw `/chain/info` object; fields other than the fee parameters are ignored.
fn parse_params(json: &str) -> Res<ChainParams> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|_| INVALID_ARGUMENT)?;
    if value["network"].as_str().is_none_or(str::is_empty) {
        return Err(INVALID_ARGUMENT);
    }
    let params: ChainParams = serde_json::from_value(value).map_err(|_| INVALID_ARGUMENT)?;
    if params.decimals > 18 {
        return Err(INVALID_ARGUMENT);
    }
    Ok(params)
}

#[derive(Deserialize)]
struct BalanceEntry {
    address: String,
    contract: String,
    amount: String,
}

/// `[{"address":"mmx1…","contract":"mmx1…","amount":"123"}]`
fn parse_balances(json: &str) -> Res<Balances> {
    let entries: Vec<BalanceEntry> = serde_json::from_str(json).map_err(|_| INVALID_ARGUMENT)?;
    let mut balances = Balances::new();
    for entry in entries {
        let address = parse_address(&entry.address).map_err(|_| INVALID_ARGUMENT)?;
        let contract = parse_address(&entry.contract).map_err(|_| INVALID_ARGUMENT)?;
        let amount = parse_atomic(&entry.amount).map_err(|_| INVALID_ARGUMENT)?;
        if balances.insert((address, contract), amount).is_some() {
            return Err(INVALID_ARGUMENT);
        }
    }
    Ok(balances)
}

/// Lowercase, single-spaced words and their seed; `invalid_mnemonic` unless
/// they form a valid 24-word phrase (of a nonzero seed, see `words_to_seed`).
fn normalize(words: &str) -> Res<(Zeroizing<String>, Seed)> {
    let mut lower = Zeroizing::new(words.to_owned());
    lower.make_ascii_lowercase();
    // Capacity never grows: the normalized phrase is at most as long as the input.
    let mut phrase = Zeroizing::new(String::with_capacity(lower.len()));
    for word in lower.split_whitespace() {
        if !phrase.is_empty() {
            phrase.push(' ');
        }
        phrase.push_str(word);
    }
    let seed = Box::new(Zeroizing::new(
        words_to_seed(&phrase).map_err(|_| INVALID_MNEMONIC)?,
    ));
    Ok((phrase, seed))
}

/// An empty passphrase means no passphrase.
fn given(passphrase: Option<&str>) -> Option<&str> {
    passphrase.filter(|p| !p.is_empty())
}

pub fn generate_mnemonic() -> Res<Zeroizing<String>> {
    let mut seed: Seed = Box::new(Zeroizing::new([0u8; 32]));
    getrandom::getrandom(&mut **seed).map_err(|_| INTERNAL)?;
    // An all-zero seed is invalid and only comes from a broken random source.
    if **seed == [0; 32] {
        return Err(INTERNAL);
    }
    // The browser source stages random bytes in a reused buffer: overwrite it.
    let mut scratch = Zeroizing::new([0u8; 32]);
    getrandom::getrandom(&mut *scratch).map_err(|_| INTERNAL)?;
    Ok(Zeroizing::new(seed_to_words(&seed)))
}

pub fn normalize_mnemonic(words: &str) -> Res<Zeroizing<String>> {
    Ok(normalize(words)?.0)
}

pub fn fingerprint_of(words: &str, passphrase: Option<&str>) -> Res<String> {
    let (_, seed) = normalize(words)?;
    Ok(fingerprint(&seed, passphrase))
}

pub fn validate_address(text: &str) -> bool {
    parse_address(text).is_ok()
}

/// Plain decimal amounts (`123`, `0.5`, `1.25`); no signs, separators or exponents.
pub fn parse_amount(text: &str, decimals: u32) -> Res<String> {
    if decimals > 18 {
        return Err(INVALID_ARGUMENT);
    }
    let digits = text.bytes().filter(u8::is_ascii_digit).count();
    let points = text.bytes().filter(|b| *b == b'.').count();
    if digits == 0 || points > 1 || digits + points != text.len() {
        return Err(INVALID_AMOUNT);
    }
    mmx_wallet::parse_amount(text, decimals)
        .map(|n| n.to_string())
        .map_err(|_| INVALID_AMOUNT)
}

pub fn format_amount(atomic: &str, decimals: u32) -> Res<String> {
    if decimals > 18 {
        return Err(INVALID_ARGUMENT);
    }
    mmx_wallet::format_amount(parse_atomic(atomic)?, decimals).map_err(wallet_error)
}

pub fn static_fee(
    params_json: &str,
    inputs: u32,
    owners: u32,
    memo: Option<String>,
    fee_level: u32,
) -> Res<String> {
    if !(1..=65535).contains(&inputs) || !(1..=65535).contains(&owners) {
        return Err(INVALID_ARGUMENT);
    }
    let memo = parse_memo(memo)?;
    let ratio = fee_ratio(fee_level)?;
    let params = parse_params(params_json)?;
    let cost = mmx_wallet::static_fee(&params, inputs as usize, owners as usize, memo.as_deref());
    Ok((cost * u128::from(ratio) / 1024).to_string())
}

pub fn verify_transaction(tx_json: &str, params_json: &str) -> Res<()> {
    let params = parse_params(params_json)?;
    let tx: Transaction = serde_json::from_str(tx_json).map_err(|_| INVALID_ARGUMENT)?;
    tx.verify(&params).map_err(|_| TX_REJECTED)
}

fn check_count(count: u32) -> Res<()> {
    if (1..=MAX_ADDRESSES).contains(&count) {
        Ok(())
    } else {
        Err(INVALID_ARGUMENT)
    }
}

fn derive(seed: &[u8; 32], passphrase: &str, account: u32, count: u32) -> Res<Wallet> {
    Wallet::derive(seed, passphrase, account, count).map_err(wallet_error)
}

/// Addresses of any account, for parity tests against the C++ reference.
#[cfg(feature = "test-nonce")]
pub fn derive_addresses(
    words: &str,
    passphrase: Option<&str>,
    account: u32,
    count: u32,
) -> Res<Vec<String>> {
    check_count(count)?;
    let (_, seed) = normalize(words)?;
    let wallet = derive(&seed, passphrase.unwrap_or(""), account, count)?;
    Ok(wallet.addresses.iter().map(Address::to_string).collect())
}

/// An opened seed. The seed and passphrase stay in zeroizing heap buffers and
/// the derived keys inside `Wallet`, which clears them on drop.
pub struct VaultCore {
    seed: Seed,
    passphrase: Zeroizing<String>,
    with_passphrase: bool,
    fingerprint: String,
    wallet: Wallet,
}

pub struct Transfer<'a> {
    pub balances_json: &'a str,
    pub params_json: &'a str,
    pub height: u32,
    pub to: &'a str,
    pub currency: &'a str,
    pub amount: &'a str,
    pub memo: Option<String>,
    pub fee_level: u32,
    pub expire_delta: u32,
    pub nonce: Option<u64>,
}

impl VaultCore {
    /// Opens `words` for a wallet whose fingerprint is `expected` (None on a
    /// first import):
    /// - fingerprint without passphrase matches: no passphrase, never asked;
    /// - fingerprint with the empty passphrase matches: opens silently;
    /// - otherwise the passphrase must reproduce the fingerprint
    ///   (`passphrase_required` when missing, `invalid_passphrase` when wrong).
    ///
    /// Without `expected`, a non-empty passphrase defines a new fingerprint.
    pub fn open(
        words: &str,
        passphrase: Option<&str>,
        expected: Option<&str>,
        count: u32,
    ) -> Res<Self> {
        check_count(count)?;
        if let Some(expected) = expected {
            if expected.parse::<u32>().map(|n| n.to_string()).as_deref() != Ok(expected) {
                return Err(INVALID_ARGUMENT);
            }
        }
        let (_, seed) = normalize(words)?;
        let (used, with_passphrase, fp) = match expected {
            Some(stored) => {
                if fingerprint(&seed, None) == stored || fingerprint(&seed, Some("")) == stored {
                    ("", false, stored.to_owned())
                } else {
                    let p = given(passphrase).ok_or(PASSPHRASE_REQUIRED)?;
                    if fingerprint(&seed, Some(p)) != stored {
                        return Err(INVALID_PASSPHRASE);
                    }
                    (p, true, stored.to_owned())
                }
            }
            None => match given(passphrase) {
                Some(p) => (p, true, fingerprint(&seed, Some(p))),
                None => ("", false, fingerprint(&seed, None)),
            },
        };
        let wallet = derive(&seed, used, 0, count)?;
        Ok(Self {
            seed,
            passphrase: Zeroizing::new(used.to_owned()),
            with_passphrase,
            fingerprint: fp,
            wallet,
        })
    }

    pub fn fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    pub fn with_passphrase(&self) -> bool {
        self.with_passphrase
    }

    pub fn addresses(&self) -> Vec<String> {
        self.wallet
            .addresses
            .iter()
            .map(Address::to_string)
            .collect()
    }

    pub fn set_count(&mut self, count: u32) -> Res<()> {
        check_count(count)?;
        if count as usize != self.wallet.addresses.len() {
            self.wallet = derive(&self.seed, &self.passphrase, 0, count)?;
        }
        Ok(())
    }

    pub fn mnemonic(&self) -> Zeroizing<String> {
        Zeroizing::new(seed_to_words(&self.seed))
    }

    pub fn max_amount(
        &self,
        balances_json: &str,
        params_json: &str,
        currency: &str,
        memo: Option<String>,
        fee_level: u32,
    ) -> Res<String> {
        let currency = parse_address(currency).map_err(|_| INVALID_ARGUMENT)?;
        let memo = parse_memo(memo)?;
        let ratio = fee_ratio(fee_level)?;
        let params = parse_params(params_json)?;
        let balances = parse_balances(balances_json)?;
        self.wallet
            .max_transfer_amount(&balances, &params, currency, memo.as_deref(), ratio)
            .map(|n| n.to_string())
            .map_err(wallet_error)
    }

    /// Signs a transfer and returns the transaction JSON exactly as `mmxwallet`
    /// sends it to `/transaction/broadcast`.
    pub fn sign_transfer(&self, t: Transfer<'_>) -> Res<String> {
        let to = parse_address(t.to)?;
        if to == Address::default() {
            return Err(INVALID_ADDRESS);
        }
        let currency = parse_address(t.currency).map_err(|_| INVALID_ARGUMENT)?;
        let amount = parse_atomic(t.amount)?;
        if amount == 0 {
            return Err(INVALID_AMOUNT);
        }
        let memo = parse_memo(t.memo)?;
        let fee_ratio = fee_ratio(t.fee_level)?;
        if t.expire_delta == 0 || t.height.checked_add(t.expire_delta).is_none() {
            return Err(INVALID_ARGUMENT);
        }
        let params = parse_params(t.params_json)?;
        let balances = parse_balances(t.balances_json)?;
        let tx = self
            .wallet
            .transfer(
                &balances,
                &params,
                t.height,
                Output {
                    address: to,
                    contract: currency,
                    amount,
                    memo,
                },
                &TransferOptions {
                    fee_ratio,
                    expire_delta: t.expire_delta,
                    nonce: t.nonce,
                    ..Default::default()
                },
            )
            .map_err(wallet_error)?;
        if !tx.fee_payer_covered(&balances) {
            return Err(INSUFFICIENT_FEE);
        }
        serde_json::to_string(&tx).map_err(|_| INTERNAL)
    }
}

impl Drop for VaultCore {
    fn drop(&mut self) {
        self.seed.zeroize();
        self.passphrase.zeroize();
    }
}

#[cfg(test)]
mod tests;
