use crate::{input, Error, Record, Result, Value as V, Variant};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct ValidationParams {
    pub network: Vec<u8>,
    pub min_txfee: u64,
    pub min_txfee_io: u64,
    pub min_txfee_sign: u64,
    pub min_txfee_memo: u64,
    pub min_txfee_exec: u64,
    pub min_txfee_deploy: u64,
    pub min_txfee_depend: u64,
    pub min_txfee_byte: u64,
}
impl Default for ValidationParams {
    fn default() -> Self {
        Self::from(&mmx_types::mmx::ChainParams::default())
    }
}
impl From<&mmx_types::mmx::ChainParams> for ValidationParams {
    fn from(p: &mmx_types::mmx::ChainParams) -> Self {
        Self {
            network: p.network.as_bytes().to_vec(),
            min_txfee: p.min_txfee,
            min_txfee_io: p.min_txfee_io,
            min_txfee_sign: p.min_txfee_sign,
            min_txfee_memo: p.min_txfee_memo,
            min_txfee_exec: p.min_txfee_exec,
            min_txfee_deploy: p.min_txfee_deploy,
            min_txfee_depend: p.min_txfee_depend,
            min_txfee_byte: p.min_txfee_byte,
        }
    }
}
fn record(value: &V) -> Result<Option<&Record>> {
    value.optional()?.map(V::record).transpose()
}
fn json(value: &V) -> Result<bool> {
    match value {
        V::Variant(v) => Ok(v.is_json()),
        _ => Err(input("expected Variant")),
    }
}
fn memos(values: &[V]) -> Result<bool> {
    for value in values {
        if let Some(memo) = value.record()?.field("memo")?.optional()? {
            if memo.text()?.len() > 64 {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
fn all_json(values: &[V]) -> Result<bool> {
    for value in values {
        if !json(value)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn version0(r: &Record) -> Result<bool> {
    Ok(r.uint("version")? == 0)
}
fn token_valid(r: &Record) -> Result<bool> {
    let symbol = r.text("symbol")?;
    let V::Int(decimals) = r.field("decimals")? else {
        return Err(input("decimals"));
    };
    Ok(version0(r)?
        && r.text("name")?.len() <= 64
        && symbol.len() <= 6
        && symbol != b"MMX"
        && !symbol.iter().any(|b| b" \n\t\r\x08\x0c".contains(b))
        && (0..=18).contains(decimals)
        && json(r.field("meta_data")?)?)
}
pub(crate) fn is_valid(r: &Record, p: &ValidationParams) -> Result<bool> {
    Ok(match r.name.as_str() {
        "mmx.Transaction" => {
            if !memos(r.seq("inputs")?)? || !memos(r.seq("outputs")?)? {
                return Ok(false);
            }
            for value in r.seq("execute")?.iter().chain(r.seq("solutions")?) {
                let Some(value) = record(value)? else {
                    return Ok(false);
                };
                if !is_valid(value, p)? {
                    return Ok(false);
                }
            }
            let version = r.uint("version")?;
            if version >= 1 {
                let solutions = r.seq("solutions")?;
                let mut hashes = BTreeSet::new();
                for value in solutions {
                    if !hashes.insert(value.record()?.calc_hash(false, 0)?) {
                        return Ok(false);
                    }
                }
                let mut used = vec![false; solutions.len()];
                if r.field("sender")?.optional()?.is_some() && !used.is_empty() {
                    used[0] = true;
                }
                for value in r.seq("inputs")? {
                    if let Some(slot) = used.get_mut(value.record()?.uint("solution")? as usize) {
                        *slot = true;
                    }
                }
                for value in r.seq("execute")? {
                    let op = value.record()?;
                    if matches!(
                        op.name.as_str(),
                        "mmx.operation.Execute" | "mmx.operation.Deposit"
                    ) && op.field("user")?.optional()?.is_some()
                    {
                        if let Some(slot) = used.get_mut(op.uint("solution")? as usize) {
                            *slot = true;
                        }
                    }
                }
                if used.contains(&false) {
                    return Ok(false);
                }
            }
            version <= 1
                && r.uint("nonce")? != 0
                && r.uint("fee_ratio")? >= 1024
                && r.text("network")? == p.network
                && r.seq("solutions")?.len() <= 65535
                && match record(r.field("exec_result")?)? {
                    Some(e) => is_valid(e, p)?,
                    None => true,
                }
                && r.uint("static_cost")? == cost(r, p)?
                && r.field("id")?.bytes()? == r.calc_hash(false, 0)?
                && r.field("content_hash")?.bytes()? == r.calc_hash(true, 0)?
        }
        "mmx.Operation" | "mmx.Solution" | "mmx.Contract" | "mmx.solution.PubKey" => version0(r)?,
        "mmx.operation.Execute" | "mmx.operation.Deposit" => {
            version0(r)?
                && !r.text("method")?.is_empty()
                && all_json(r.seq("args")?)?
                && (r.name != "mmx.operation.Deposit"
                    || r.field("amount")?.bytes()?.iter().any(|b| *b != 0))
        }
        "mmx.solution.MultiSig" => {
            if !version0(r)? {
                return Ok(false);
            }
            for value in r.map("solutions")?.values() {
                let Some(value) = record(value)? else {
                    return Ok(false);
                };
                if !is_valid(value, p)? {
                    return Ok(false);
                }
            }
            true
        }
        "mmx.contract.PubKey" => {
            version0(r)? && r.field("address")?.bytes()?.iter().any(|b| *b != 0)
        }
        "mmx.contract.MultiSig" => {
            let owners: BTreeSet<_> = r.seq("owners")?.iter().collect();
            version0(r)?
                && r.uint("num_required")? > 0
                && owners.len() as u64 >= r.uint("num_required")?
        }
        "mmx.contract.TokenBase" => token_valid(r)?,
        "mmx.contract.Executable" => {
            if !all_json(r.seq("init_args")?)? {
                return Ok(false);
            }
            for name in r.map("depends")?.keys() {
                if name.text()?.len() > 64 {
                    return Ok(false);
                }
            }
            token_valid(r)? && r.field("binary")?.bytes()?.iter().any(|b| *b != 0)
        }
        "mmx.contract.Binary" => {
            for method in r.map("methods")?.values() {
                let m = method.record()?;
                let constant = m.field("is_const")?.boolean()?;
                let payable = m.field("is_payable")?.boolean()?;
                if constant && payable
                    || m.field("is_init")?.boolean()?
                        && (constant || payable || m.field("is_public")?.boolean()?)
                {
                    return Ok(false);
                }
            }
            version0(r)?
        }
        "mmx.contract.Data" => version0(r)? && json(r.field("value")?)?,
        "mmx.contract.WebData" => version0(r)? && r.text("mime_type")?.len() <= 256,
        "mmx.exec_result_t" => {
            memos(r.seq("inputs")?)?
                && memos(r.seq("outputs")?)?
                && match record(r.field("error")?)? {
                    Some(e) => is_valid(e, p)?,
                    None => true,
                }
        }
        "mmx.exec_error_t" => r.text("message")?.len() <= 160,
        _ => return Err(Error::Unsupported(r.name.clone())),
    })
}
fn sum(values: impl IntoIterator<Item = Result<u64>>) -> Result<u64> {
    values
        .into_iter()
        .try_fold(0u64, |a, b| Ok(a.wrapping_add(b?)))
}
pub(crate) fn cost(r: &Record, p: &ValidationParams) -> Result<u64> {
    Ok(match r.name.as_str() {
        "mmx.TransactionBase" | "mmx.Operation" => 0,
        "mmx.Transaction" => {
            let mut total = p.min_txfee as u128;
            total += (r.seq("execute")?.len() as u64).wrapping_mul(p.min_txfee_exec) as u128;
            if let Some(exec) = record(r.field("exec_result")?)? {
                total += cost(exec, p)? as u128;
            }
            for values in ["inputs", "outputs", "execute", "solutions"] {
                for value in r.seq(values)? {
                    if let Some(value) = record(value)? {
                        total += cost(value, p)? as u128;
                    }
                }
            }
            if let Some(deploy) = record(r.field("deploy")?)? {
                total += p.min_txfee_deploy.wrapping_add(cost(deploy, p)?) as u128;
            }
            total.try_into().map_err(|_| Error::CostOverflow)?
        }
        "mmx.txio_t" | "mmx.txin_t" | "mmx.txout_t" => {
            let memo = if let Some(memo) = r.field("memo")?.optional()? {
                (memo.text()?.len() as u64)
                    .div_ceil(32)
                    .max(1)
                    .wrapping_mul(p.min_txfee_memo)
            } else {
                0
            };
            p.min_txfee_io.wrapping_add(memo)
        }
        "mmx.exec_result_t" => sum(r
            .seq("inputs")?
            .iter()
            .chain(r.seq("outputs")?)
            .map(|v| cost(v.record()?, p)))?,
        "mmx.operation.Execute" | "mmx.operation.Deposit" => {
            let n = (r.text("method")?.len() as u64)
                .wrapping_add(sum(r.seq("args")?.iter().map(variant_size))?);
            n.wrapping_mul(p.min_txfee_byte)
        }
        "mmx.solution.PubKey" => p.min_txfee_sign,
        "mmx.solution.MultiSig" => p.min_txfee_sign.wrapping_add(sum(r
            .map("solutions")?
            .values()
            .map(|v| match record(v)? {
                Some(r) => cost(r, p),
                None => Ok(0),
            }))?),
        name if name.starts_with("mmx.contract.") || name == "mmx.Contract" => {
            let mut total = num_bytes(r)?.wrapping_mul(p.min_txfee_byte);
            if name == "mmx.contract.Executable" {
                total = total.wrapping_add(
                    (r.map("depends")?.len() as u64).wrapping_mul(p.min_txfee_depend),
                );
            }
            total
        }
        _ => return Err(Error::Unsupported(r.name.clone())),
    })
}
fn variant_size(v: &V) -> Result<u64> {
    if let V::Variant(variant @ Variant { value, raw, .. }) = v {
        variant.check_conversion()?;
        dynamic_size(value, raw)
    } else {
        Err(input("expected Variant"))
    }
}
fn dynamic_size(value: &V, raw: &[u8]) -> Result<u64> {
    Ok(match value {
        V::Null | V::Bool(_) => 1,
        V::Uint(_) | V::Int(_) => 8,
        V::Float32(_) if raw.is_empty() => 8,
        V::Float64(_) if raw.is_empty() => 12,
        V::String(s) => 4u64.wrapping_add(s.len() as u64),
        V::Bytes(s) => 4u64.wrapping_add((s.len() as u64).wrapping_mul(8)),
        V::Sequence(values) => 4u64.wrapping_add(sum(values.iter().map(|v| {
            if matches!(v, V::Variant(_)) {
                variant_size(v)
            } else {
                dynamic_size(v, &[])
            }
        }))?),
        V::Object(fields) => {
            let fields: std::collections::BTreeMap<_, _> =
                fields.iter().map(|(k, v)| (k, v)).collect();
            4u64.wrapping_add(sum(fields
                .iter()
                .map(|(k, v)| Ok((k.len() as u64).wrapping_add(variant_size(v)?))))?)
        }
        V::Record(_) | V::Map(_) | V::Optional(_) | V::Opaque(_) if raw.is_empty() => {
            return Err(Error::Unsupported(
                "typed composite Variant array element".into(),
            ))
        }
        _ => raw.len() as u64,
    })
}
pub(crate) fn num_bytes(r: &Record) -> Result<u64> {
    let text = |name| r.text(name).map(|s| s.len() as u64);
    Ok(match r.name.as_str() {
        "mmx.Contract" => 16,
        "mmx.contract.PubKey" => 48,
        "mmx.contract.MultiSig" => {
            16 + BTreeSet::<_>::from_iter(r.seq("owners")?.iter()).len() as u64 * 32
        }
        "mmx.contract.Data" => 16u64.wrapping_add(variant_size(r.field("value")?)?),
        "mmx.contract.WebData" => 16u64
            .wrapping_add(text("mime_type")?)
            .wrapping_add(r.field("payload")?.bytes()?.len() as u64),
        "mmx.contract.TokenBase" | "mmx.contract.Executable" => {
            let mut total = 16u64
                .wrapping_add(text("name")?)
                .wrapping_add(text("symbol")?)
                .wrapping_add(variant_size(r.field("meta_data")?)?);
            if r.name == "mmx.contract.Executable" {
                total = total
                    .wrapping_add(32)
                    .wrapping_add(text("init_method")?)
                    .wrapping_add(r.map("depends")?.len() as u64 * 32)
                    .wrapping_add(sum(r.seq("init_args")?.iter().map(variant_size))?)
                    .wrapping_add(sum(r
                        .map("depends")?
                        .keys()
                        .map(|v| Ok(v.text()?.len() as u64)))?);
            }
            total
        }
        "mmx.contract.Binary" => {
            let mut total = 16u64
                .wrapping_add(r.map("fields")?.len() as u64 * 4)
                .wrapping_add(r.map("line_info")?.len() as u64 * 8);
            total = total.wrapping_add(sum(r
                .map("fields")?
                .keys()
                .map(|k| Ok(k.text()?.len() as u64)))?);
            total = total.wrapping_add(sum(r.map("methods")?.iter().map(|(k, v)| {
                Ok((k.text()?.len() as u64).wrapping_add(num_bytes(v.record()?)?))
            }))?);
            for name in ["name", "source", "compiler"] {
                total = total.wrapping_add(text(name)?);
            }
            for name in ["constant", "binary"] {
                total = total.wrapping_add(r.field(name)?.bytes()?.len() as u64);
            }
            total
        }
        "mmx.contract.method_t" => 19u64
            .wrapping_add(text("name")?)
            .wrapping_add(text("info")?)
            .wrapping_add(r.seq("args")?.len() as u64 * 4)
            .wrapping_add(sum(r
                .seq("args")?
                .iter()
                .map(|v| Ok(v.text()?.len() as u64)))?),
        _ => return Err(Error::Unsupported(r.name.clone())),
    })
}
