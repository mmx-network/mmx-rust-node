//! External links: only block explorer pages for a transaction or an address.
use mmx_wallet::Address;
use mmxwallet::service::{Error, Result};

const EXPLORER: &str = "https://explore.mmx.network/#/explore/";
const MAX_URL: usize = 256;

/// Returns the URL if it is an explorer page for a transaction ID (64
/// hexadecimal digits) or a canonical MMX address; anything else is refused.
pub fn explorer_url(url: &str) -> Result<&str> {
    let refused = || Error::new("invalid_argument", "unsupported link");
    if url.len() > MAX_URL {
        return Err(refused());
    }
    let (kind, id) = url
        .strip_prefix(EXPLORER)
        .and_then(|rest| rest.split_once('/'))
        .ok_or_else(refused)?;
    let valid = match kind {
        "transaction" => id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "address" => id
            .parse::<Address>()
            .is_ok_and(|address| address.to_string() == id),
        _ => false,
    };
    if valid {
        Ok(url)
    } else {
        Err(refused())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev";

    #[test]
    fn accepts_explorer_pages() {
        let tx = format!("{EXPLORER}transaction/{}", "AB01".repeat(16));
        assert_eq!(explorer_url(&tx).unwrap(), tx);
        let tx = format!("{EXPLORER}transaction/{}", "ab01".repeat(16));
        assert!(explorer_url(&tx).is_ok());
        let address = format!("{EXPLORER}address/{ADDRESS}");
        assert!(explorer_url(&address).is_ok());
    }

    #[test]
    fn refuses_everything_else() {
        let txid = "AB01".repeat(16);
        for url in [
            String::new(),
            "https://explore.mmx.network/".into(),
            format!("{EXPLORER}transaction/{}", &txid[1..]),
            format!("{EXPLORER}transaction/{txid}0"),
            format!("{EXPLORER}transaction/{}", "G".repeat(64)),
            format!("{EXPLORER}transaction/{txid}/"),
            format!("{EXPLORER}transaction/{txid}?x=1"),
            format!("{EXPLORER}block/{txid}"),
            format!("{EXPLORER}address/MMX"),
            format!("{EXPLORER}address/{}", ADDRESS.to_uppercase()),
            format!("{EXPLORER}address/{}x", &ADDRESS[..ADDRESS.len() - 1]),
            format!("{EXPLORER}address/{ADDRESS}/../../x"),
            format!("http://explore.mmx.network/#/explore/address/{ADDRESS}"),
            format!("https://explore.mmx.network.evil/#/explore/address/{ADDRESS}"),
            format!("https://explore.mmx.network@evil/#/explore/address/{ADDRESS}"),
            format!(" {EXPLORER}address/{ADDRESS}"),
            "file:///etc/passwd".into(),
            "javascript:alert(1)".into(),
        ] {
            assert!(explorer_url(&url).is_err(), "{url}");
        }
    }
}
