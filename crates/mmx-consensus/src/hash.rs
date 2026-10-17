use crate::{input, Error, Hash, Record, Result, Value as V, Variant};
use sha2::{Digest, Sha256};

pub(crate) fn registry() -> &'static vnx::Registry {
    static REGISTRY: std::sync::LazyLock<vnx::Registry> =
        std::sync::LazyLock::new(|| mmx_types::vnx_registry().expect("checked-in MMX schema"));
    &REGISTRY
}
struct Writer {
    bytes: Vec<u8>,
    version: u32,
}
impl Writer {
    fn new(version: u32) -> Self {
        Self {
            bytes: Vec::new(),
            version,
        }
    }
    fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }
    fn uint(&mut self, value: u64) {
        self.raw(&value.to_le_bytes());
    }
    fn blob(&mut self, bytes: &[u8], string: bool) {
        self.raw(if string { b"string<>" } else { b"bytes<>" });
        self.uint(bytes.len() as u64);
        self.raw(bytes);
    }
    fn name(&mut self, name: &str) {
        self.raw(b"field<>");
        self.blob(name.as_bytes(), true);
    }
    fn field(&mut self, record: &Record, name: &str) -> Result<()> {
        self.name(name);
        self.value(record.field(name)?)
    }
    fn fields(&mut self, record: &Record, names: &[&str]) -> Result<()> {
        for name in names {
            self.field(record, name)?;
        }
        Ok(())
    }
    fn type_hash(&mut self, record: &Record) -> Result<()> {
        self.uint(registry().by_name(&record.name)?.type_hash);
        Ok(())
    }
    fn hash_field(&mut self, record: &Record, name: &str, full: bool, version: u32) -> Result<()> {
        self.name(name);
        let hash = optional_hash(record.field(name)?, full, version)?;
        self.blob(&hash, false);
        Ok(())
    }
    fn vector_header(&mut self, size: usize) {
        self.raw(b"vector<>");
        self.uint(size as u64);
    }
    fn value(&mut self, value: &V) -> Result<()> {
        match value {
            V::Null => self.raw(b"NULL"),
            V::Bool(v) => self.raw(&[u8::from(*v)]),
            V::Uint(v) => self.uint(*v),
            V::Int(v) => self.raw(&v.to_le_bytes()),
            V::String(v) => self.blob(v, true),
            V::Bytes(v) => self.blob(v, false),
            V::Sequence(values) => {
                self.vector_header(values.len());
                for value in values {
                    self.value(value)?;
                }
            }
            V::Map(values) => {
                self.vector_header(values.len());
                for (k, v) in values {
                    self.raw(b"pair<>");
                    self.value(k)?;
                    self.value(v)?;
                }
            }
            V::Object(values) => {
                let values: std::collections::BTreeMap<_, _> =
                    values.iter().map(|(k, v)| (k, v)).collect();
                self.raw(b"object<>");
                self.vector_header(values.len());
                for (k, v) in values {
                    self.raw(b"pair<>");
                    self.blob(k, true);
                    self.value(v)?;
                }
            }
            V::Optional(value) => {
                self.raw(b"optional<>");
                self.raw(&[u8::from(value.is_some())]);
                if let Some(value) = value {
                    self.value(value)?;
                }
            }
            V::Record(r) => match r.name.as_str() {
                "mmx.compile_flags_t" => {
                    self.raw(b"compile_flags_t<>");
                    self.fields(r, &["verbose", "opt_level", "catch_overflow"])?;
                }
                "mmx.contract.method_t" => {
                    self.type_hash(r)?;
                    self.fields(
                        r,
                        &[
                            "name",
                            "info",
                            "is_init",
                            "is_const",
                            "is_public",
                            "is_payable",
                            "entry_point",
                            "args",
                        ],
                    )?;
                }
                "mmx.txin_t" | "mmx.txout_t" => self.io(r, false)?,
                _ => return Err(Error::Unsupported(r.name.clone())),
            },
            V::Variant(v) => self.variant(v)?,
            V::Opaque(_) | V::Float32(_) | V::Float64(_) => {
                return Err(input("opaque value outside Variant"))
            }
        }
        Ok(())
    }
    fn variant(&mut self, variant: &Variant) -> Result<()> {
        variant.check_conversion()?;
        if self.version >= 1 && !variant.is_json() {
            self.raw(b"variant<>");
        }
        self.variant_value(&variant.value, &variant.raw)
    }
    fn variant_value(&mut self, value: &V, raw: &[u8]) -> Result<()> {
        match value {
            V::Null | V::String(_) | V::Object(_) => self.value(value)?,
            V::Bool(_) | V::Uint(_) | V::Int(_) => {
                if self.version >= 1 {
                    self.raw(match value {
                        V::Bool(_) => b"<bool>",
                        V::Uint(_) => b"<u64>",
                        _ => b"<i64>",
                    });
                }
                self.value(value)?;
            }
            V::Sequence(values) => {
                self.vector_header(values.len());
                for value in values {
                    if let V::Variant(v) = value {
                        self.variant(v)?;
                    } else {
                        // to<vector<Variant>> passes typed elements through
                        // ToObject. Native write_dynamic turns nonnegative
                        // signed integers into unsigned values and normalizes
                        // floating-point scalars to little-endian payloads.
                        match value {
                            V::Int(v) if *v >= 0 => self.variant_value(&V::Uint(*v as u64), &[])?,
                            V::Float32(v) => self.float_element(9, &v.to_le_bytes()),
                            V::Float64(v) => self.float_element(10, &v.to_le_bytes()),
                            V::Record(_) | V::Map(_) | V::Optional(_) | V::Opaque(_) => {
                                return Err(Error::Unsupported(
                                    "typed composite Variant array element".into(),
                                ))
                            }
                            _ => self.variant_value(value, &[])?,
                        }
                    }
                }
            }
            V::Bytes(values) => {
                // Variant's byte lists are arrays; ordinary vector<uchar>
                // fields use bytes<> instead.
                self.vector_header(values.len());
                for value in values {
                    self.variant_value(&V::Uint(*value as u64), &[])?;
                }
            }
            _ => self.blob(raw, false),
        }
        Ok(())
    }
    fn float_element(&mut self, code: u8, bytes: &[u8]) {
        if self.version >= 1 {
            self.raw(b"variant<>");
        }
        let mut raw = vec![1, 0, code, 0];
        raw.extend_from_slice(bytes);
        self.blob(&raw, false);
    }
    fn io(&mut self, record: &Record, full: bool) -> Result<()> {
        self.raw(if record.name == "mmx.txin_t" {
            b"txin_t<>"
        } else {
            b"txout_t<>"
        });
        self.value(record.field("address")?)?;
        self.value(record.field("contract")?)?;
        let amount = record.field("amount")?.bytes()?;
        if amount.len() != 16 {
            return Err(input("uint128 size"));
        }
        self.raw(amount);
        self.value(record.field("memo")?)?;
        if full && record.name == "mmx.txin_t" {
            self.value(record.field("solution")?)?;
            self.value(record.field("flags")?)?;
        }
        Ok(())
    }
}
fn optional_hash(value: &V, full: bool, version: u32) -> Result<Hash> {
    match value.optional()? {
        Some(v) => calc_hash(v.record()?, full, version),
        None => Ok([0; 32]),
    }
}
pub(crate) fn transaction(record: &Record, full: bool) -> Result<Vec<u8>> {
    if record.name != "mmx.Transaction" {
        return Err(input("expected transaction"));
    }
    let version = record.uint("version")? as u32;
    let mut out = Writer::new(version);
    out.type_hash(record)?;
    out.fields(
        record,
        &[
            "version",
            "expires",
            "fee_ratio",
            "max_fee_amount",
            "note",
            "nonce",
            "network",
            "sender",
        ],
    )?;
    out.name("inputs");
    let inputs = record.seq("inputs")?;
    out.vector_header(inputs.len());
    for value in inputs {
        out.io(value.record()?, full)?;
    }
    out.field(record, "outputs")?;
    out.name("execute");
    let ops = record.seq("execute")?;
    out.uint(ops.len() as u64);
    for op in ops {
        out.blob(&optional_hash(op, full, version)?, false);
    }
    out.hash_field(record, "deploy", full, version)?;
    if full {
        out.field(record, "static_cost")?;
        out.name("solutions");
        let solutions = record.seq("solutions")?;
        out.uint(solutions.len() as u64);
        for sol in solutions {
            out.blob(&optional_hash(sol, false, 0)?, false);
        }
        out.hash_field(record, "exec_result", false, 0)?;
    }
    Ok(out.bytes)
}
pub(crate) fn calc_hash(record: &Record, full: bool, version: u32) -> Result<Hash> {
    if record.name == "mmx.TransactionBase" {
        return record
            .field("id")?
            .bytes()?
            .try_into()
            .map_err(|_| input("hash size"));
    }
    if record.name == "mmx.Transaction" {
        return Ok(Sha256::digest(transaction(record, full)?).into());
    }
    let mut out = Writer::new(
        if record.name.starts_with("mmx.solution.") || record.name.starts_with("mmx.exec_") {
            0
        } else {
            version
        },
    );
    out.type_hash(record)?;
    match record.name.as_str() {
        "mmx.contract.TokenBase" => out.fields(
            record,
            &["version", "name", "symbol", "decimals", "meta_data"],
        )?,
        "mmx.contract.Executable" => out.fields(
            record,
            &[
                "version",
                "name",
                "symbol",
                "decimals",
                "meta_data",
                "binary",
                "init_method",
                "init_args",
                "depends",
            ],
        )?,
        "mmx.contract.Binary" => out.fields(
            record,
            &[
                "version",
                "name",
                "fields",
                "methods",
                "constant",
                "binary",
                "line_info",
                "source",
                "compiler",
                "build_flags",
            ],
        )?,
        "mmx.contract.PubKey" => out.fields(record, &["version", "address"])?,
        "mmx.contract.MultiSig" => {
            out.fields(record, &["version", "num_required"])
                .and_then(|_| {
                    out.name("owners");
                    let owners: std::collections::BTreeSet<_> =
                        record.seq("owners")?.iter().collect();
                    out.vector_header(owners.len());
                    for owner in owners {
                        out.value(owner)?;
                    }
                    Ok(())
                })?
        }
        "mmx.contract.Data" => out.fields(record, &["version", "value"])?,
        "mmx.contract.WebData" => out.fields(record, &["version", "mime_type", "payload"])?,
        "mmx.ProofOfSpaceOG" | "mmx.ProofOfSpaceNFT" => {
            out.fields(
                record,
                &[
                    "score",
                    "plot_id",
                    "challenge",
                    "difficulty",
                    "farmer_key",
                    "ksize",
                    "seed",
                    "proof_xs",
                ],
            )?;
            if record.name == "mmx.ProofOfSpaceNFT" {
                out.field(record, "contract")?;
            }
        }
        "mmx.operation.Execute" | "mmx.operation.Deposit" => {
            out.fields(record, &["version", "address", "method", "args", "user"])?;
            if record.name == "mmx.operation.Deposit" {
                out.field(record, "currency")?;
                out.name("amount");
                let amount = record.field("amount")?.bytes()?;
                if amount.len() != 16 {
                    return Err(input("uint128 size"));
                }
                out.raw(amount);
            }
            if full {
                out.field(record, "solution")?;
            }
        }
        "mmx.solution.PubKey" => out.fields(record, &["version", "pubkey", "signature"])?,
        "mmx.solution.MultiSig" => {
            out.field(record, "version")?;
            out.name("solutions");
            let values = record.map("solutions")?;
            out.uint(values.len() as u64);
            for (k, v) in values {
                out.raw(b"pair<>");
                out.value(k)?;
                out.blob(&optional_hash(v, false, 0)?, false);
            }
            out.field(record, "num_required")?;
        }
        "mmx.exec_result_t" => {
            out.fields(
                record,
                &["did_fail", "total_cost", "total_fee", "inputs", "outputs"],
            )?;
            out.hash_field(record, "error", false, 0)?;
        }
        "mmx.exec_error_t" => {
            out.fields(record, &["code", "address", "operation", "line", "message"])?
        }
        _ => return Err(Error::Unsupported(record.name.clone())),
    }
    Ok(Sha256::digest(out.bytes).into())
}

pub(crate) fn header(record: &Record, hardfork2_height: u32) -> Result<Vec<u8>> {
    if !matches!(record.name.as_str(), "mmx.Block" | "mmx.BlockHeader") {
        return Err(input("expected block header"));
    }
    let mut out = Writer::new(0);
    // Native get_type_hash is virtual: a Block retains its derived type hash.
    out.type_hash(record)?;
    out.fields(
        record,
        &[
            "version",
            "support_flags",
            "prev",
            "height",
            "vdf_height",
            "nonce",
            "time_stamp",
            "time_diff",
            "space_diff",
        ],
    )?;
    for name in ["weight", "total_weight"] {
        out.name(name);
        let amount = record.field(name)?.bytes()?;
        if amount.len() != 16 {
            return Err(input("uint128 size"));
        }
        out.raw(amount);
    }
    out.fields(
        record,
        &[
            "vdf_count",
            "vdf_iters",
            "vdf_output",
            "vdf_reward_addr",
            "vdf_reward_payout",
        ],
    )?;
    out.name("proof");
    let proofs = record.seq("proof")?;
    out.vector_header(proofs.len());
    for proof in proofs {
        out.blob(&calc_hash(proof.record()?, false, 0)?, false);
    }
    out.field(record, "proof_hash")?;
    if record.uint("height")? >= hardfork2_height as u64 {
        out.field(record, "proof_chain")?;
    }
    out.fields(
        record,
        &[
            "challenge",
            "is_space_fork",
            "space_fork_len",
            "space_fork_proofs",
            "reward_amount",
            "reward_addr",
            "reward_contract",
            "reward_account",
            "reward_vote",
            "reward_vote_sum",
            "reward_vote_count",
            "base_reward",
            "project_addr",
            "static_cost",
            "total_cost",
            "tx_count",
            "tx_fees",
            "txfee_buffer",
            "tx_hash",
        ],
    )?;
    Ok(out.bytes)
}
