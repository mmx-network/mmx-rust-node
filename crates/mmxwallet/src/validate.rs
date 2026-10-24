//! Validation of values supplied by users and by the wallet UI.
use crate::error::{invalid, Error, Result};
use mmx_wallet::Address;

pub const NAME_MAX_CHARS: usize = 64;
pub const NOTE_MAX_CHARS: usize = 256;
pub const MEMO_MAX_BYTES: usize = 64;
pub const MIN_NUM_ADDRESSES: u32 = 1;
pub const MAX_NUM_ADDRESSES: u32 = 100;
pub const DEFAULT_NUM_ADDRESSES: u32 = 10;
pub const FEE_LEVELS: [u32; 5] = [1, 2, 3, 5, 10];

/// Characters that never belong in single-line user text: control characters
/// (including line breaks and tabs), line/paragraph separators, bidirectional
/// embeddings/overrides/isolates/marks and invisible formatting characters.
/// Matches the UI's `hasUnsafeCharacters`.
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{061c}'
                | '\u{200b}'
                | '\u{2060}'
                | '\u{feff}'
                | '\u{00ad}'
        )
}

/// Wallet, contact and address label names: trimmed, 1..=64 characters, safe text.
pub fn name(value: &str) -> Result<String> {
    let value = value.trim();
    let chars = value.chars().count();
    if chars == 0 || chars > NAME_MAX_CHARS || value.chars().any(is_unsafe_char) {
        return Err(invalid(
            "name must be 1 to 64 characters without control characters",
        ));
    }
    Ok(value.to_owned())
}

/// Optional contact note: trimmed, at most 256 characters, safe text; empty is none.
pub fn note(value: &str) -> Result<Option<String>> {
    let value = value.trim();
    if value.chars().count() > NOTE_MAX_CHARS || value.chars().any(is_unsafe_char) {
        return Err(invalid(
            "note must be at most 256 characters without control characters",
        ));
    }
    Ok((!value.is_empty()).then(|| value.to_owned()))
}

pub fn num_addresses(count: u32) -> Result<u32> {
    if !(MIN_NUM_ADDRESSES..=MAX_NUM_ADDRESSES).contains(&count) {
        return Err(invalid("number of addresses must be between 1 and 100"));
    }
    Ok(count)
}

/// A canonical (lowercase `mmx1…`) address, as printed by `Address`.
pub fn canonical_address(value: &str) -> Option<Address> {
    if value.len() != 62 || !value.starts_with("mmx1") {
        return None;
    }
    value
        .parse::<Address>()
        .ok()
        .filter(|a| a.to_string() == value)
}

/// Payment recipient: canonical and not the zero (native currency) address.
pub fn recipient(value: &str) -> Result<Address> {
    canonical_address(value)
        .filter(|a| *a != Address::default())
        .ok_or_else(|| Error::new("invalid_address", "invalid recipient address"))
}

/// Currency contract: canonical; the zero address is native MMX.
pub fn currency(value: &str) -> Result<Address> {
    canonical_address(value).ok_or_else(|| invalid("invalid currency contract address"))
}

/// Positive amount in atomic units as a plain decimal string.
pub fn atomic_amount(value: &str) -> Result<u128> {
    let bad = || {
        Error::new(
            "invalid_amount",
            "amount must be a positive whole number of atomic units",
        )
    };
    if value.is_empty() || value.len() > 39 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    value
        .parse::<u128>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(bad)
}

/// Optional memo, at most 64 UTF-8 bytes of safe text; empty is none.
pub fn memo(value: Option<&str>) -> Result<Option<String>> {
    match value {
        None | Some("") => Ok(None),
        Some(m) if m.len() > MEMO_MAX_BYTES || m.chars().any(is_unsafe_char) => Err(Error::new(
            "invalid_memo",
            "memo must be at most 64 bytes without control characters",
        )),
        Some(m) => Ok(Some(m.to_owned())),
    }
}

/// Fee priority level → signed fee ratio (level × 1024).
pub fn fee_ratio(level: u32) -> Result<u32> {
    if !FEE_LEVELS.contains(&level) {
        return Err(invalid("fee level must be 1, 2, 3, 5 or 10"));
    }
    Ok(level * 1024)
}

/// Transaction ID: 64 hexadecimal characters, returned in uppercase (the
/// node's display form).
pub fn txid(value: &str) -> Result<String> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("transaction ID must be 64 hexadecimal characters"));
    }
    Ok(value.to_ascii_uppercase())
}

/// Key file names the wallet directory listing recognises.
pub fn is_wallet_file_name(name: &str) -> bool {
    name.len() <= 255
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
        && (name == "wallet.dat"
            || (name.ends_with(".dat")
                && (name.starts_with("wallet_") || name.starts_with("mmxwallet_"))))
}

pub fn wallet_id(id: &str) -> Result<&str> {
    if !is_wallet_file_name(id) {
        return Err(invalid("invalid wallet ID"));
    }
    Ok(id)
}

/// Contact IDs are short opaque tokens created by the UI.
pub fn contact_id(id: &str) -> Result<&str> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(invalid("invalid contact ID"));
    }
    Ok(id)
}

/// Chain server reads the UI may make: a fixed set of endpoints with a plain
/// query string.
pub fn rpc_path(path: &str) -> Result<&str> {
    const ALLOWED: [&str; 6] = [
        "/node/info",
        "/chain/info",
        "/address",
        "/balance",
        "/address/history",
        "/transaction",
    ];
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    if path.len() > 512
        || !ALLOWED.contains(&route)
        || !query
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'=' | b'&' | b'_' | b'-'))
    {
        return Err(invalid("unsupported chain request"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO: &str = "mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev";

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(name("  Savings ").unwrap(), "Savings");
        assert_eq!(name(&"é".repeat(64)).unwrap().chars().count(), 64);
        for bad in [
            "",
            "   ",
            &"a".repeat(65),
            "a\nb",
            "a\tb",
            "evil\u{202e}gnp",
            "zero\u{200b}width",
            "del\u{7f}",
            "c1\u{85}x",
        ] {
            assert_eq!(name(bad).unwrap_err().code, "invalid_argument", "{bad:?}");
        }
    }

    #[test]
    fn notes_allow_empty_and_reject_unsafe_text() {
        assert_eq!(note("  ").unwrap(), None);
        assert_eq!(note(" hi ").unwrap().as_deref(), Some("hi"));
        assert!(note(&"x".repeat(256)).unwrap().is_some());
        assert!(note(&"x".repeat(257)).is_err());
        assert!(note("line\nbreak").is_err());
    }

    #[test]
    fn addresses_must_be_canonical() {
        assert_eq!(canonical_address(ZERO), Some(Address::default()));
        assert_eq!(recipient(ZERO).unwrap_err().code, "invalid_address");
        assert!(currency(ZERO).is_ok());
        assert!(canonical_address(&ZERO.to_ascii_uppercase()).is_none());
        assert!(canonical_address("MMX").is_none());
        assert!(canonical_address(&format!(" {ZERO}")).is_none());
        let mut typo = ZERO.to_owned();
        typo.replace_range(10..11, "p");
        assert!(canonical_address(&typo).is_none());
    }

    #[test]
    fn amounts_memos_and_fee_levels() {
        assert_eq!(atomic_amount("1").unwrap(), 1);
        assert_eq!(
            atomic_amount("340282366920938463463374607431768211455").unwrap(),
            u128::MAX
        );
        for bad in [
            "",
            "0",
            "000",
            "-1",
            "+1",
            "1.5",
            "1e3",
            " 1",
            "340282366920938463463374607431768211456",
        ] {
            assert_eq!(
                atomic_amount(bad).unwrap_err().code,
                "invalid_amount",
                "{bad:?}"
            );
        }
        assert_eq!(memo(Some("")).unwrap(), None);
        assert_eq!(memo(Some(&"é".repeat(32))).unwrap().unwrap().len(), 64);
        assert_eq!(
            memo(Some(&"é".repeat(33))).unwrap_err().code,
            "invalid_memo"
        );
        assert_eq!(memo(Some("a\u{0}")).unwrap_err().code, "invalid_memo");
        assert_eq!(fee_ratio(10).unwrap(), 10240);
        for bad in [0, 4, 11, u32::MAX] {
            assert!(fee_ratio(bad).is_err());
        }
    }

    #[test]
    fn wallet_ids_are_plain_key_file_names() {
        for good in ["wallet.dat", "wallet_123.dat", "mmxwallet_x.dat"] {
            assert!(wallet_id(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "wallet",
            "wallet.json",
            "../wallet.dat",
            "wallet_/../../x.dat",
            "wallet_a\\b.dat",
            "wallet_a:b.dat",
            "wallet_\n.dat",
            "keys.dat",
            "mmxwallet.json",
        ] {
            assert!(wallet_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rpc_paths_are_allowlisted() {
        for good in [
            "/node/info",
            "/chain/info",
            "/address?id=mmx1abc&limit=1000",
            "/balance?id=mmx1abc",
            "/address/history?id=mmx1abc&limit=25&until=99",
            "/transaction?id=ABCDEF0123",
        ] {
            assert!(rpc_path(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "/",
            "/wallet/keys",
            "/transaction/broadcast",
            "/node/info/",
            "//node/info",
            "/address/../wallet",
            "/address?id=a%2F",
            "/address?id=a b",
            "/address?id=a#x",
            "/address?id=a.b",
            "/address?id=a/b",
            "/address?id=a?b",
            "http://evil/node/info",
        ] {
            assert!(rpc_path(bad).is_err(), "{bad:?}");
        }
        assert!(rpc_path(&format!("/address?id={}", "a".repeat(600))).is_err());
    }
}
