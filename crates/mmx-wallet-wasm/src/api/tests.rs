use super::*;
use serde_json::{json, Value};

const PASS: &str = "test 世界 🔑";
const MEMO_33: &str = "0123456789abcdef0123456789abcdef!";
const MEMO_64: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/parity.json");

fn reference() -> Value {
    serde_json::from_str(include_str!(
        "../../../mmx-wallet/tests/fixtures/reference.json"
    ))
    .unwrap()
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn mnemonic() -> String {
    text(&reference()["mnemonic"]).to_owned()
}

fn seed() -> [u8; 32] {
    std::array::from_fn(|i| (i + 1) as u8)
}

fn params() -> Value {
    // Non-default fee parameters prove they are read; unknown fields are ignored.
    json!({
        "network": "testnet13",
        "decimals": 6,
        "min_txfee": 100,
        "min_txfee_io": 120,
        "min_txfee_sign": 1100,
        "min_txfee_memo": 60,
        "min_txfee_exec": 2000,
        "block_interval_ms": 10000,
        "vdf_seed": "00",
    })
}

fn chain_params() -> ChainParams {
    serde_json::from_value(params()).unwrap()
}

fn token() -> Address {
    Address([0x11; 32])
}

fn recipient() -> Address {
    Address([0x22; 32])
}

fn wallet() -> Wallet {
    Wallet::derive(&seed(), PASS, 0, 3).unwrap()
}

fn balances(w: &Wallet) -> Value {
    let native = Address::default().to_string();
    let a = |i: usize| w.addresses[i].to_string();
    json!([
        {"address": a(0), "contract": native, "amount": "9000000"},
        {"address": a(1), "contract": native, "amount": "12000000"},
        {"address": a(2), "contract": native, "amount": "1500"},
        {"address": a(0), "contract": token().to_string(), "amount": "1000"},
        {"address": a(2), "contract": token().to_string(), "amount": "500"},
        {"address": Address([0x33; 32]).to_string(), "contract": native, "amount": "77000000"},
    ])
}

fn open_reference() -> VaultCore {
    let fp = text(&reference()["fingerprint_pass"]).to_owned();
    VaultCore::open(&mnemonic(), Some(PASS), Some(&fp), 3).unwrap()
}

/// Signs natively with the core wallet exactly as `mmxwallet send` does.
#[allow(clippy::too_many_arguments)]
fn native_transfer(
    w: &Wallet,
    height: u32,
    to: Address,
    currency: Address,
    amount: u128,
    memo: Option<&str>,
    fee_level: u32,
    expire_delta: u32,
    nonce: u64,
) -> String {
    let balances = parse_balances(&balances(w).to_string()).unwrap();
    let tx = w
        .transfer(
            &balances,
            &chain_params(),
            height,
            Output {
                address: to,
                contract: currency,
                amount,
                memo: memo.map(str::to_owned),
            },
            &TransferOptions {
                fee_ratio: fee_level * 1024,
                expire_delta,
                nonce: Some(nonce),
                ..Default::default()
            },
        )
        .unwrap();
    String::from_utf8(serde_json::to_vec(&tx).unwrap()).unwrap()
}

fn build_fixture() -> Value {
    let reference = reference();
    let w = wallet();
    let vault = open_reference();
    let plain = Wallet::derive(&seed(), "", 0, 3).unwrap();
    let balances_json = balances(&w).to_string();
    let params_json = params().to_string();
    let native = Address::default();
    let max_cases = [
        (native, None, 1),
        (native, Some(MEMO_33), 3),
        (native, Some(MEMO_64), 10),
        (token(), None, 1),
        (token(), Some(""), 10),
    ];
    let max: Vec<Value> = max_cases
        .iter()
        .map(|&(currency, memo, level)| {
            let core = w
                .max_transfer_amount(
                    &parse_balances(&balances_json).unwrap(),
                    &chain_params(),
                    currency,
                    memo.filter(|m| !m.is_empty()),
                    level * 1024,
                )
                .unwrap();
            let amount = vault
                .max_amount(
                    &balances_json,
                    &params_json,
                    &currency.to_string(),
                    memo.map(str::to_owned),
                    level,
                )
                .unwrap();
            assert_eq!(amount, core.to_string());
            json!({"currency": currency.to_string(), "memo": memo, "fee_level": level, "amount": amount})
        })
        .collect();
    let max_memo_33: u128 = text(&max[1]["amount"]).parse().unwrap();
    let transfers = [
        (
            5_050_000,
            recipient(),
            native,
            15_000_000,
            Some("memo 世界"),
            2,
            123,
            123456789,
        ),
        (100, w.addresses[2], native, 1_000, None, 1, 100, 1),
        (
            7,
            recipient(),
            token(),
            1_500,
            Some(MEMO_64),
            10,
            1000,
            u64::MAX,
        ),
        (
            5_050_000,
            recipient(),
            native,
            max_memo_33,
            Some(MEMO_33),
            3,
            100,
            987654321,
        ),
        (42, recipient(), native, 2_000_000, Some("x"), 5, 50, 31337),
    ];
    let transfers: Vec<Value> = transfers
        .iter()
        .map(
            |&(height, to, currency, amount, memo, level, expire, nonce)| {
                let expected =
                    native_transfer(&w, height, to, currency, amount, memo, level, expire, nonce);
                let signed = vault
                    .sign_transfer(Transfer {
                        balances_json: &balances_json,
                        params_json: &params_json,
                        height,
                        to: &to.to_string(),
                        currency: &currency.to_string(),
                        amount: &amount.to_string(),
                        memo: memo.map(str::to_owned),
                        fee_level: level,
                        expire_delta: expire,
                        nonce: Some(nonce),
                    })
                    .unwrap();
                assert_eq!(signed, expected);
                verify_transaction(&signed, &params_json).unwrap();
                json!({
                    "height": height,
                    "to": to.to_string(),
                    "currency": currency.to_string(),
                    "amount": amount.to_string(),
                    "memo": memo,
                    "fee_level": level,
                    "expire_delta": expire,
                    "nonce": nonce.to_string(),
                    "tx": signed,
                })
            },
        )
        .collect();
    let static_fees: Vec<Value> = [
        (1, 1, None, 1),
        (2, 3, Some(MEMO_33), 5),
        (1, 2, Some(""), 10),
    ]
    .iter()
    .map(|&(inputs, owners, memo, level)| {
        let fee = static_fee(&params_json, inputs, owners, memo.map(str::to_owned), level).unwrap();
        json!({"inputs": inputs, "owners": owners, "memo": memo, "fee_level": level, "fee": fee})
    })
    .collect();
    json!({
        "mnemonic": reference["mnemonic"],
        "passphrase": PASS,
        "fingerprint": reference["fingerprint"],
        "fingerprint_empty_pass": reference["fingerprint_empty_pass"],
        "fingerprint_pass": reference["fingerprint_pass"],
        "count": 3,
        "addresses": vault.addresses(),
        "addresses_no_passphrase": plain.addresses.iter().map(Address::to_string).collect::<Vec<_>>(),
        "params": params(),
        "balances": balances(&w),
        "token": token().to_string(),
        "recipient": recipient().to_string(),
        "max": max,
        "transfers": transfers,
        "static_fee": static_fees,
    })
}

/// Native signing and the WASM API agree byte for byte; the stored fixture is
/// what the WASM build is compared against under Node.
/// Regenerate with `UPDATE_PARITY_FIXTURE=1 cargo test -p mmx-wallet-wasm`.
#[test]
fn parity_fixture_matches_native_signing() {
    let fixture = build_fixture();
    if std::env::var_os("UPDATE_PARITY_FIXTURE").is_some() {
        let mut text = serde_json::to_string_pretty(&fixture).unwrap();
        text.push('\n');
        std::fs::write(FIXTURE, text).unwrap();
    }
    let stored = std::fs::read_to_string(FIXTURE)
        .expect("parity fixture missing: run with UPDATE_PARITY_FIXTURE=1");
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored, fixture);
}

#[test]
fn derivation_matches_reference() {
    let r = reference();
    let m = mnemonic();
    assert_eq!(fingerprint_of(&m, None).unwrap(), text(&r["fingerprint"]));
    assert_eq!(
        fingerprint_of(&m, Some("")).unwrap(),
        text(&r["fingerprint_empty_pass"])
    );
    assert_eq!(
        fingerprint_of(&m, Some(PASS)).unwrap(),
        text(&r["fingerprint_pass"])
    );
    let messy = format!("  {}\n", m.to_uppercase().replace(' ', " \t\u{a0}"));
    assert_eq!(normalize_mnemonic(&messy).unwrap().as_str(), m);
    let vault = open_reference();
    assert_eq!(vault.mnemonic().as_str(), m);
    let expected: Vec<String> = wallet().addresses.iter().map(Address::to_string).collect();
    assert_eq!(vault.addresses(), expected);
}

#[cfg(feature = "test-nonce")]
#[test]
fn account_addresses_match_reference() {
    let r = reference();
    let addresses = derive_addresses(&mnemonic(), Some(PASS), 7, 3).unwrap();
    assert_eq!(json!(addresses), r["addresses"]);
}

#[test]
fn passphrase_detection() {
    let r = reference();
    let m = mnemonic();
    let none = text(&r["fingerprint"]);
    let empty = text(&r["fingerprint_empty_pass"]);
    let pass = text(&r["fingerprint_pass"]);
    let plain: Vec<String> = Wallet::derive(&seed(), "", 0, 2)
        .unwrap()
        .addresses
        .iter()
        .map(Address::to_string)
        .collect();

    // No passphrase: never asked, a typed one is ignored.
    for p in [None, Some(""), Some("anything")] {
        let v = VaultCore::open(&m, p, Some(none), 2).unwrap();
        assert!(!v.with_passphrase());
        assert_eq!(v.fingerprint(), none);
        assert_eq!(v.addresses(), plain);
    }
    // Stored with an empty passphrase: opens silently with the same keys.
    let v = VaultCore::open(&m, None, Some(empty), 2).unwrap();
    assert!(!v.with_passphrase());
    assert_eq!(v.fingerprint(), empty);
    assert_eq!(v.addresses(), plain);
    // Locked: the passphrase is required and checked against the fingerprint.
    for p in [None, Some("")] {
        assert_eq!(
            VaultCore::open(&m, p, Some(pass), 2).err(),
            Some(PASSPHRASE_REQUIRED)
        );
    }
    for p in ["wrong", "test 世界", " test 世界 🔑"] {
        assert_eq!(
            VaultCore::open(&m, Some(p), Some(pass), 2).err(),
            Some(INVALID_PASSPHRASE)
        );
    }
    let v = VaultCore::open(&m, Some(PASS), Some(pass), 2).unwrap();
    assert!(v.with_passphrase());
    assert_eq!(v.fingerprint(), pass);
    assert_ne!(v.addresses(), plain);
    // First import: the passphrase (if any) defines the fingerprint.
    for p in [None, Some("")] {
        let v = VaultCore::open(&m, p, None, 2).unwrap();
        assert!(!v.with_passphrase());
        assert_eq!(v.fingerprint(), none);
    }
    let v = VaultCore::open(&m, Some(PASS), None, 2).unwrap();
    assert!(v.with_passphrase());
    assert_eq!(v.fingerprint(), pass);
    // Unknown fingerprint of another wallet.
    assert_eq!(
        VaultCore::open(&m, None, Some("12345"), 1).err(),
        Some(PASSPHRASE_REQUIRED)
    );
    assert_eq!(
        VaultCore::open(&m, Some(PASS), Some("12345"), 1).err(),
        Some(INVALID_PASSPHRASE)
    );
    for bad in ["", "x", "012", "4294967296", "-1", " 1"] {
        assert_eq!(
            VaultCore::open(&m, None, Some(bad), 1).err(),
            Some(INVALID_ARGUMENT)
        );
    }
}

#[test]
fn mnemonic_validation() {
    let m = mnemonic();
    let words: Vec<&str> = m.split(' ').collect();
    let short = words[..23].join(" ");
    let mut swapped = words.clone();
    swapped.swap(0, 1);
    let unknown = format!("{} notaword", words[..23].join(" "));
    let zero = seed_to_words(&[0; 32]);
    for bad in [
        String::new(),
        short,
        swapped.join(" "),
        unknown,
        zero,
        format!("{m} abandon"),
    ] {
        assert_eq!(normalize_mnemonic(&bad).err(), Some(INVALID_MNEMONIC));
        assert_eq!(
            VaultCore::open(&bad, None, None, 1).err(),
            Some(INVALID_MNEMONIC)
        );
        assert_eq!(fingerprint_of(&bad, None).err(), Some(INVALID_MNEMONIC));
    }
    let a = generate_mnemonic().unwrap();
    let b = generate_mnemonic().unwrap();
    assert_ne!(a.as_str(), b.as_str());
    assert_eq!(a.split(' ').count(), 24);
    assert_eq!(normalize_mnemonic(&a).unwrap().as_str(), a.as_str());
    let v = VaultCore::open(&a, None, None, 1).unwrap();
    assert_eq!(v.mnemonic().as_str(), a.as_str());
}

#[test]
fn address_count() {
    let m = mnemonic();
    for count in [0, 101, u32::MAX] {
        assert_eq!(
            VaultCore::open(&m, None, None, count).err(),
            Some(INVALID_ARGUMENT)
        );
    }
    let mut v = VaultCore::open(&m, None, None, 3).unwrap();
    let first = v.addresses();
    v.set_count(5).unwrap();
    let more = v.addresses();
    assert_eq!(more.len(), 5);
    assert_eq!(more[..3], first[..]);
    v.set_count(1).unwrap();
    assert_eq!(v.addresses(), first[..1]);
    assert_eq!(v.set_count(0).err(), Some(INVALID_ARGUMENT));
    assert_eq!(v.set_count(101).err(), Some(INVALID_ARGUMENT));
    assert_eq!(v.addresses().len(), 1);
    v.set_count(100).unwrap();
    assert_eq!(v.addresses().len(), 100);
}

#[test]
fn amounts_and_addresses() {
    for (input, decimals, atomic) in [
        ("1", 6, "1000000"),
        ("0.000001", 6, "1"),
        ("1.5", 6, "1500000"),
        ("1.", 6, "1000000"),
        (".5", 6, "500000"),
        ("007", 0, "7"),
        (
            "340282366920938463463374607431768.211455",
            6,
            &u128::MAX.to_string(),
        ),
    ] {
        assert_eq!(parse_amount(input, decimals).unwrap(), atomic, "{input}");
    }
    for bad in [
        "",
        "0",
        "0.0",
        ".",
        "-1",
        "+1",
        "1e3",
        "1E3",
        "1,5",
        "1 000",
        " 1",
        "1.2.3",
        "0x10",
        "1.0000001",
        "١",
        "340282366920938463463374607431768.211456",
    ] {
        assert_eq!(parse_amount(bad, 6).err(), Some(INVALID_AMOUNT), "{bad}");
    }
    assert_eq!(parse_amount("1", 19).err(), Some(INVALID_ARGUMENT));
    assert_eq!(format_amount("1500000", 6).unwrap(), "1.5");
    assert_eq!(format_amount("0", 6).unwrap(), "0");
    assert_eq!(format_amount("1", 0).unwrap(), "1");
    for bad in [
        "",
        "-1",
        "1.5",
        "1e3",
        " 1",
        "340282366920938463463374607431768211456",
    ] {
        assert_eq!(format_amount(bad, 6).err(), Some(INVALID_AMOUNT), "{bad}");
    }
    assert_eq!(format_amount("1", 19).err(), Some(INVALID_ARGUMENT));

    let a = wallet().addresses[0].to_string();
    assert!(validate_address(&a));
    assert!(validate_address(&Address::default().to_string()));
    for bad in [
        String::new(),
        "MMX".into(),
        a.to_uppercase(),
        format!(" {a}"),
        a[..61].to_owned(),
        format!("{}q", &a[..61]),
        a.replacen("mmx1", "bc1q", 1),
    ] {
        assert!(!validate_address(&bad), "{bad}");
    }
}

#[test]
fn fees() {
    let p = params().to_string();
    // 100 + 2 × 120 + 1100 = 1440 at 1×.
    assert_eq!(static_fee(&p, 1, 1, None, 1).unwrap(), "1440");
    assert_eq!(
        static_fee(&p, 1, 1, Some(String::new()), 3).unwrap(),
        "4320"
    );
    // 100 + 3 × 120 + 2 × 60 + 3 × 1100 = 3880 at 5×.
    assert_eq!(
        static_fee(&p, 2, 3, Some(MEMO_33.into()), 5).unwrap(),
        "19400"
    );
    assert_eq!(static_fee(&p, 0, 1, None, 1).err(), Some(INVALID_ARGUMENT));
    assert_eq!(static_fee(&p, 1, 0, None, 1).err(), Some(INVALID_ARGUMENT));
    assert_eq!(
        static_fee(&p, 1, 1, Some("x".repeat(65)), 1).err(),
        Some(INVALID_MEMO)
    );
    for level in [0, 4, 6, 11, 1024] {
        assert_eq!(
            static_fee(&p, 1, 1, None, level).err(),
            Some(INVALID_ARGUMENT)
        );
    }
    for bad in [
        "",
        "[]",
        "{}",
        r#"{"network":""}"#,
        r#"{"network":"mainnet","decimals":19}"#,
        r#"{"network":"mainnet","min_txfee":"100"}"#,
    ] {
        assert_eq!(
            static_fee(bad, 1, 1, None, 1).err(),
            Some(INVALID_ARGUMENT),
            "{bad}"
        );
    }
}

fn transfer<'a>(
    balances_json: &'a str,
    params_json: &'a str,
    to: &'a str,
    currency: &'a str,
    amount: &'a str,
) -> Transfer<'a> {
    Transfer {
        balances_json,
        params_json,
        height: 1000,
        to,
        currency,
        amount,
        memo: None,
        fee_level: 1,
        expire_delta: 100,
        nonce: None,
    }
}

#[test]
fn transfer_validation() {
    let v = open_reference();
    let w = wallet();
    let b = balances(&w).to_string();
    let p = params().to_string();
    let to = recipient().to_string();
    let mmx = Address::default().to_string();
    let t = || transfer(&b, &p, &to, &mmx, "1000");

    let tx: Transaction = serde_json::from_str(&v.sign_transfer(t()).unwrap()).unwrap();
    tx.verify(&chain_params()).unwrap();
    assert_ne!(tx.nonce, 0);
    let other: Transaction = serde_json::from_str(&v.sign_transfer(t()).unwrap()).unwrap();
    assert_ne!(tx.nonce, other.nonce);
    assert_eq!(tx.expires, 1100);
    assert_eq!(tx.fee_ratio, 1024);

    let zero = Address::default().to_string();
    let upper = to.to_uppercase();
    for bad_to in ["", "MMX", zero.as_str(), upper.as_str(), "mmx1qqq"] {
        let r = v.sign_transfer(Transfer { to: bad_to, ..t() });
        assert_eq!(r.err(), Some(INVALID_ADDRESS), "{bad_to}");
    }
    for bad in ["", "0", "-1", "1.5", " 1", "1e3", "0x1"] {
        let r = v.sign_transfer(Transfer { amount: bad, ..t() });
        assert_eq!(r.err(), Some(INVALID_AMOUNT), "{bad}");
    }
    let r = v.sign_transfer(Transfer {
        memo: Some("é".repeat(33)),
        ..t()
    });
    assert_eq!(r.err(), Some(INVALID_MEMO));
    let ok: Transaction = serde_json::from_str(
        &v.sign_transfer(Transfer {
            memo: Some("é".repeat(32)),
            ..t()
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(ok.outputs[0].memo.as_deref().map(str::len), Some(64));
    let empty: Transaction = serde_json::from_str(
        &v.sign_transfer(Transfer {
            memo: Some(String::new()),
            ..t()
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(empty.outputs[0].memo, None);
    for level in [0, 4, 7, 20] {
        let r = v.sign_transfer(Transfer {
            fee_level: level,
            ..t()
        });
        assert_eq!(r.err(), Some(INVALID_ARGUMENT));
    }
    let r = v.sign_transfer(Transfer {
        expire_delta: 0,
        ..t()
    });
    assert_eq!(r.err(), Some(INVALID_ARGUMENT));
    let r = v.sign_transfer(Transfer {
        height: u32::MAX,
        ..t()
    });
    assert_eq!(r.err(), Some(INVALID_ARGUMENT));
    let r = v.sign_transfer(Transfer {
        currency: "MMX",
        ..t()
    });
    assert_eq!(r.err(), Some(INVALID_ARGUMENT));

    // Funds: 21_001_500 MMX owned (the foreign balance does not count).
    let r = v.sign_transfer(Transfer {
        amount: "21001501",
        ..t()
    });
    assert_eq!(r.err(), Some(INSUFFICIENT_FUNDS));
    let r = v.sign_transfer(Transfer {
        amount: "21001500",
        ..t()
    });
    assert_eq!(r.err(), Some(INSUFFICIENT_FEE));
    let tok = token().to_string();
    let r = v.sign_transfer(Transfer {
        currency: &tok,
        amount: "1501",
        ..t()
    });
    assert_eq!(r.err(), Some(INSUFFICIENT_FUNDS));
    // Token transfer with no MMX to pay the fee.
    let a0 = w.addresses[0].to_string();
    let no_mmx = json!([{"address": a0, "contract": tok, "amount": "10"}]).to_string();
    let r = v.sign_transfer(Transfer {
        balances_json: &no_mmx,
        currency: &tok,
        amount: "10",
        ..t()
    });
    assert_eq!(r.err(), Some(INSUFFICIENT_FEE));
    assert_eq!(
        v.max_amount(&no_mmx, &p, &tok, None, 1).err(),
        Some(INSUFFICIENT_FEE)
    );

    let duplicate = json!([
        {"address": a0, "contract": mmx, "amount": "1"},
        {"address": a0, "contract": mmx, "amount": "2"},
    ])
    .to_string();
    let bad_balances = [
        "".to_owned(),
        "{}".to_owned(),
        duplicate,
        json!([{"address": "MMX", "contract": mmx, "amount": "1"}]).to_string(),
        json!([{"address": a0, "contract": mmx, "amount": 1}]).to_string(),
        json!([{"address": a0, "contract": mmx, "amount": "-1"}]).to_string(),
        json!([{"address": a0, "contract": mmx}]).to_string(),
    ];
    for bad in &bad_balances {
        let r = v.sign_transfer(Transfer {
            balances_json: bad,
            ..t()
        });
        assert_eq!(r.err(), Some(INVALID_ARGUMENT), "{bad}");
        assert_eq!(
            v.max_amount(bad, &p, &mmx, None, 1).err(),
            Some(INVALID_ARGUMENT)
        );
    }
    let r = v.sign_transfer(Transfer {
        params_json: "{}",
        ..t()
    });
    assert_eq!(r.err(), Some(INVALID_ARGUMENT));
}

#[test]
fn maximum_is_signable_and_one_more_is_not() {
    let v = open_reference();
    let w = wallet();
    let b = balances(&w).to_string();
    let p = params().to_string();
    let to = recipient().to_string();
    for currency in [Address::default().to_string(), token().to_string()] {
        for memo in [None, Some("x"), Some(MEMO_33), Some(MEMO_64)] {
            for level in [1, 2, 3, 5, 10] {
                let max = v
                    .max_amount(&b, &p, &currency, memo.map(str::to_owned), level)
                    .unwrap();
                let at = |amount: &str| {
                    v.sign_transfer(Transfer {
                        memo: memo.map(str::to_owned),
                        fee_level: level,
                        ..transfer(&b, &p, &to, &currency, amount)
                    })
                };
                let tx = at(&max).unwrap();
                verify_transaction(&tx, &p).unwrap();
                let over = (max.parse::<u128>().unwrap() + 1).to_string();
                assert!(matches!(
                    at(&over).err(),
                    Some(INSUFFICIENT_FEE | INSUFFICIENT_FUNDS)
                ));
            }
        }
    }
    assert_eq!(
        v.max_amount(
            &b,
            &p,
            &Address::default().to_string(),
            Some("x".repeat(65)),
            1
        )
        .err(),
        Some(INVALID_MEMO)
    );
    assert_eq!(
        v.max_amount(&b, &p, "MMX", None, 1).err(),
        Some(INVALID_ARGUMENT)
    );
    assert_eq!(
        v.max_amount("[]", &p, &Address::default().to_string(), None, 1)
            .unwrap(),
        "0"
    );
}

#[test]
fn verification_rejects_tampering() {
    let v = open_reference();
    let w = wallet();
    let b = balances(&w).to_string();
    let p = params().to_string();
    let to = recipient().to_string();
    let mmx = Address::default().to_string();
    let signed = v
        .sign_transfer(transfer(&b, &p, &to, &mmx, "1000"))
        .unwrap();
    verify_transaction(&signed, &p).unwrap();
    let tampered = signed.replace("\"amount\":\"1000\"", "\"amount\":\"1001\"");
    assert_ne!(tampered, signed);
    assert_eq!(verify_transaction(&tampered, &p).err(), Some(TX_REJECTED));
    let other_network = params_with("network", json!("mainnet"));
    assert_eq!(
        verify_transaction(&signed, &other_network).err(),
        Some(TX_REJECTED)
    );
    assert_eq!(verify_transaction("{}", &p).err(), Some(INVALID_ARGUMENT));
    assert_eq!(
        verify_transaction(&signed, "{}").err(),
        Some(INVALID_ARGUMENT)
    );
}

fn params_with(key: &str, value: Value) -> String {
    let mut p = params();
    p[key] = value;
    p.to_string()
}
