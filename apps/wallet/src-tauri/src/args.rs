//! Command arguments sent by the wallet UI.
//!
//! Every command receives one JSON object. Unknown fields are rejected so a
//! misspelled field (for example a passphrase) can never be silently dropped.
//! Recovery words and passphrases are held in `Zeroizing` buffers from the
//! moment they are decoded. Decoding errors never echo the request: a
//! malformed request only yields `invalid_argument`.
use mmxwallet::service::{AppState, Error, Result, SendRequest};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use zeroize::Zeroizing;

/// Decodes a command's arguments.
pub fn parse<T: DeserializeOwned>(body: &Value) -> Result<T> {
    T::deserialize(body).map_err(|_| invalid_request())
}

pub fn invalid_request() -> Error {
    Error::new("invalid_argument", "invalid request")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Wallet {
    pub id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveState {
    pub state: AppState,
}

/// A new wallet from the words the user has just written down and
/// confirmed.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Create {
    pub name: String,
    pub mnemonic: Zeroizing<String>,
    #[serde(default)]
    pub passphrase: Option<Zeroizing<String>>,
    pub address_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Import {
    pub name: String,
    pub mnemonic: Zeroizing<String>,
    #[serde(default)]
    pub passphrase: Option<Zeroizing<String>>,
    pub address_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportSeedFile {
    pub name: String,
    pub address_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Unlock {
    pub id: String,
    #[serde(default)]
    pub passphrase: Option<Zeroizing<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rename {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddressCount {
    pub id: String,
    pub count: u32,
    #[serde(default)]
    pub passphrase: Option<Zeroizing<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpcGet {
    pub path: String,
}

/// The largest amount that can be sent; the recipient is not needed (and may
/// still be empty while the user types).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaxSend {
    pub wallet_id: String,
    #[serde(default, rename = "to")]
    pub _to: Option<String>,
    pub contract: String,
    #[serde(default)]
    pub memo: Option<String>,
    pub fee_level: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Send {
    pub wallet_id: String,
    pub to: String,
    pub contract: String,
    pub amount: String,
    #[serde(default)]
    pub memo: Option<String>,
    pub fee_level: u32,
}

impl From<Send> for SendRequest {
    fn from(s: Send) -> Self {
        SendRequest {
            wallet_id: s.wallet_id,
            to: s.to,
            contract: s.contract,
            amount: s.amount,
            memo: s.memo,
            fee_level: s.fee_level,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Broadcast {
    pub wallet_id: String,
    pub txid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Txid {
    pub txid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopyText {
    pub text: Zeroizing<String>,
    #[serde(default)]
    pub sensitive: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenExternal {
    pub url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportFile {
    pub suggested_name: String,
    pub contents: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WORDS: &str = "cactus wedding mansion deal random arrive gate tissue bench party \
                         setup captain abstract mandate crouch lift anxiety embark achieve army \
                         expect account amount claim";

    #[test]
    fn decodes_requests() {
        let c: Create =
            parse(&json!({"name": "Main", "mnemonic": WORDS, "addressCount": 10})).unwrap();
        assert_eq!(
            (c.name.as_str(), c.mnemonic.as_str(), c.address_count),
            ("Main", WORDS, 10)
        );
        assert!(c.passphrase.is_none());

        let c: Create = parse(&json!({
            "name": "Main", "mnemonic": WORDS, "passphrase": "pw", "addressCount": 1
        }))
        .unwrap();
        assert_eq!(c.passphrase.as_deref().map(String::as_str), Some("pw"));

        let u: Unlock = parse(&json!({"id": "wallet.dat", "passphrase": null})).unwrap();
        assert!(u.passphrase.is_none());

        let m: MaxSend = parse(&json!({
            "walletId": "wallet.dat", "to": "", "contract": "mmx1", "feeLevel": 1
        }))
        .unwrap();
        assert_eq!(
            (m.wallet_id.as_str(), m.memo, m.fee_level),
            ("wallet.dat", None, 1)
        );

        let s: Send = parse(&json!({
            "walletId": "wallet.dat", "to": "mmx1a", "contract": "mmx1b",
            "amount": "5", "memo": "hi", "feeLevel": 2
        }))
        .unwrap();
        let r = SendRequest::from(s);
        assert_eq!(
            (
                r.to.as_str(),
                r.amount.as_str(),
                r.memo.as_deref(),
                r.fee_level
            ),
            ("mmx1a", "5", Some("hi"), 2)
        );

        let t: CopyText = parse(&json!({"text": "abc"})).unwrap();
        assert!(!t.sensitive);

        let s: SaveState = parse(&json!({"state": {"contacts": []}})).unwrap();
        assert_eq!(s.state.settings.fee_level, 1);
    }

    #[test]
    fn rejects_unknown_or_malformed_fields() {
        // A misspelled passphrase must not create an unprotected wallet.
        assert!(parse::<Create>(&json!({
            "name": "a", "mnemonic": WORDS, "passPhrase": "x", "addressCount": 1
        }))
        .is_err());
        // A new wallet is only saved with the words the user confirmed.
        assert!(parse::<Create>(&json!({"name": "a", "addressCount": 1})).is_err());
        assert!(parse::<Create>(&json!({"name": "a", "mnemonic": 5, "addressCount": 1})).is_err());
        // Web-only fields are not accepted by the desktop backend.
        assert!(parse::<Unlock>(&json!({"id": "w", "mnemonic": "words"})).is_err());
        assert!(parse::<Import>(&json!({
            "name": "a", "mnemonic": "w", "addressCount": 1, "expectedFingerprint": "1"
        }))
        .is_err());
        assert!(parse::<Create>(&json!({"name": "a", "mnemonic": WORDS})).is_err());
        for count in [json!(-1), json!(1.5), json!("1")] {
            assert!(parse::<Create>(&json!({
                "name": "a", "mnemonic": WORDS, "addressCount": count
            }))
            .is_err());
        }
        assert!(parse::<Wallet>(&json!([])).is_err());
        assert!(parse::<Wallet>(&Value::Null).is_err());
        assert!(parse::<CopyText>(&json!({"text": 5})).is_err());
    }

    #[test]
    fn errors_do_not_echo_input() {
        let secret = "abandon ability able about above absent";
        let Err(e) = parse::<AddressCount>(&json!({"id": "w", "count": secret})) else {
            panic!("decoded an invalid request");
        };
        assert_eq!(e.code, "invalid_argument");
        assert!(!e.message.contains("abandon"));

        let Err(e) = parse::<Create>(&json!({"name": "a", "mnemonic": WORDS, "addressCount": "x"}))
        else {
            panic!("decoded an invalid request");
        };
        assert!(!e.message.contains("cactus"));
    }
}
