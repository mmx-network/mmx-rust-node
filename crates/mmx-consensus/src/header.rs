//! BlockHeader hashes, static validity, farmer authentication and the contextual
//! checks in Node::validate. Space/VDF work, challenge/difficulty transitions,
//! weight verification and NFT reward-target execution remain separate stages.
use crate::{hash, input, proof, Error, Hash, Record, Result, Value};
use mmx_types::{mmx, VnxType};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Input(#[from] Error),
    #[error(transparent)]
    Crypto(#[from] mmx_crypto::VerifyError),
}
fn supported(record: &Record) -> Result<()> {
    if matches!(record.name.as_str(), "mmx.Block" | "mmx.BlockHeader") {
        Ok(())
    } else {
        Err(input("expected block header"))
    }
}
fn bytes<const N: usize>(value: &Value) -> Result<[u8; N]> {
    value
        .bytes()?
        .try_into()
        .map_err(|_| input("header byte length"))
}
fn signed(record: &Record, name: &str) -> Result<i64> {
    match record.field(name)? {
        Value::Int(v) => Ok(*v),
        _ => Err(input("signed header field")),
    }
}

/// Convert the complete native header, including fields outside its hash.
/// Validate the compiled wire layout before constructing its generated view;
/// ordinary space proofs avoid decoding another set of schema declarations.
pub fn to_typed(record: &Record) -> Result<mmx::BlockHeader> {
    supported(record)?;
    let header = Record {
        name: "mmx.BlockHeader".into(),
        fields: record
            .fields
            .iter()
            .filter(|(name, _)| name.as_str() != "tx_list")
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    };
    // Enforce the same missing-field, width, signedness, array and dynamic-class
    // checks as the previous self-describing encode/decode conversion.
    let wire = header.encode()?;
    let limits = vnx::binary::Limits::default();
    if wire.len() > limits.max_value_bytes
        || header.seq("proof")?.len() > limits.max_container
        || header.seq("vdf_reward_addr")?.len() > limits.max_container
    {
        return Ok(mmx::BlockHeader::decode(&wire)?);
    }
    let Some(proofs) = header
        .seq("proof")?
        .iter()
        .map(|value| typed_value(value, limits.max_container))
        .collect::<Option<_>>()
    else {
        // Unusual dynamic values retain the general VNX conversion semantics.
        return Ok(mmx::BlockHeader::decode(&wire)?);
    };
    // Widths and representations were checked against the compiled schema
    // above. Copy fixed fields directly instead of expanding them into JSON
    // arrays and deserializing them back into the generated fixed arrays.
    Ok(mmx::BlockHeader {
        version: header.uint("version")? as u32,
        support_flags: header.uint("support_flags")? as u32,
        hash: bytes::<32>(header.field("hash")?)?.into(),
        prev: bytes::<32>(header.field("prev")?)?.into(),
        height: header.uint("height")? as u32,
        vdf_height: header.uint("vdf_height")? as u32,
        nonce: header.uint("nonce")?,
        time_stamp: signed(&header, "time_stamp")?,
        time_diff: header.uint("time_diff")?,
        space_diff: header.uint("space_diff")?,
        weight: bytes::<16>(header.field("weight")?)?.into(),
        total_weight: bytes::<16>(header.field("total_weight")?)?.into(),
        vdf_count: header.uint("vdf_count")? as u32,
        vdf_iters: header.uint("vdf_iters")?,
        vdf_output: bytes::<32>(header.field("vdf_output")?)?.into(),
        vdf_reward_addr: header
            .seq("vdf_reward_addr")?
            .iter()
            .map(|value| bytes::<32>(value).map(Into::into))
            .collect::<Result<_>>()?,
        vdf_reward_payout: optional_bytes::<32>(&header, "vdf_reward_payout")?.map(Into::into),
        proof: proofs,
        proof_hash: bytes::<32>(header.field("proof_hash")?)?.into(),
        proof_chain: bytes::<32>(header.field("proof_chain")?)?.into(),
        challenge: bytes::<32>(header.field("challenge")?)?.into(),
        is_space_fork: header.field("is_space_fork")?.boolean()?,
        space_fork_len: header.uint("space_fork_len")? as u32,
        space_fork_proofs: header.uint("space_fork_proofs")? as u32,
        reward_amount: header.uint("reward_amount")?,
        reward_addr: optional_bytes::<32>(&header, "reward_addr")?.map(Into::into),
        reward_contract: optional_bytes::<32>(&header, "reward_contract")?.map(Into::into),
        reward_account: optional_bytes::<32>(&header, "reward_account")?.map(Into::into),
        reward_vote: signed(&header, "reward_vote")? as i8,
        reward_vote_sum: signed(&header, "reward_vote_sum")? as i32,
        reward_vote_count: header.uint("reward_vote_count")? as u32,
        base_reward: header.uint("base_reward")?,
        project_addr: bytes::<32>(header.field("project_addr")?)?.into(),
        static_cost: header.uint("static_cost")?,
        total_cost: header.uint("total_cost")?,
        tx_count: header.uint("tx_count")? as u32,
        tx_fees: header.uint("tx_fees")?,
        txfee_buffer: header.uint("txfee_buffer")?,
        tx_hash: bytes::<32>(header.field("tx_hash")?)?.into(),
        farmer_sig: optional_bytes::<64>(&header, "farmer_sig")?.map(Into::into),
        content_hash: bytes::<32>(header.field("content_hash")?)?.into(),
    })
}

fn optional_bytes<const N: usize>(record: &Record, name: &str) -> Result<Option<[u8; N]>> {
    record.field(name)?.optional()?.map(bytes).transpose()
}

fn typed_object(record: &Record, max_container: usize) -> Option<vnx::serde_json::Value> {
    use vnx::serde_json::{Map, Value as Json};
    if !matches!(
        record.name.as_str(),
        "mmx.ProofOfSpace" | "mmx.ProofOfSpaceOG" | "mmx.ProofOfSpaceNFT"
    ) {
        return None;
    }
    let schema = hash::registry().by_name(&record.name).ok()?;
    let mut object = Map::new();
    object.insert("__type".into(), Json::String(record.name.clone()));
    for field in &schema.fields {
        object.insert(
            field.name.clone(),
            typed_value(record.fields.get(&field.name)?, max_container)?,
        );
    }
    Some(Json::Object(object))
}

fn typed_value(value: &Value, max_container: usize) -> Option<vnx::serde_json::Value> {
    use vnx::serde_json::Value as Json;
    Some(match value {
        Value::Null | Value::Optional(None) => Json::Null,
        Value::Bool(value) => Json::Bool(*value),
        Value::Uint(value) => Json::from(*value),
        Value::Int(value) => Json::from(*value),
        Value::Bytes(values) if values.len() <= max_container => {
            Json::Array(values.iter().map(|v| Json::from(*v)).collect())
        }
        Value::Sequence(values) if values.len() <= max_container => Json::Array(
            values
                .iter()
                .map(|value| typed_value(value, max_container))
                .collect::<Option<_>>()?,
        ),
        Value::Optional(Some(value)) => typed_value(value, max_container)?,
        Value::Record(record) => typed_object(record, max_container)?,
        _ => return None,
    })
}
pub fn hash_serialize(record: &Record, params: &mmx::ChainParams) -> Result<Vec<u8>> {
    hash::header(record, params.hardfork2_height)
}
pub fn calc_hash(record: &Record, params: &mmx::ChainParams) -> Result<Hash> {
    Ok(Sha256::digest(hash_serialize(record, params)?).into())
}
pub fn calc_content_hash(record: &Record) -> Result<Hash> {
    supported(record)?;
    let mut hash = Sha256::new();
    hash.update(bytes::<32>(record.field("hash")?)?);
    if let Some(signature) = record.field("farmer_sig")?.optional()? {
        hash.update(bytes::<64>(signature)?);
    }
    Ok(hash.finalize().into())
}
/// Native static validity, without signature verification or proof-of-space work.
pub fn is_valid(record: &Record, params: &mmx::ChainParams) -> Result<bool> {
    supported(record)?;
    let proofs = record.seq("proof")?;
    if record.uint("height")? != 0
        && (proofs.is_empty()
            || record.field("farmer_sig")?.optional()?.is_none()
            || record.uint("vdf_count")? < 1)
    {
        return Ok(false);
    }
    for value in proofs {
        let Some(value) = value.optional()? else {
            return Ok(false);
        };
        if !proof::is_valid(value.record()?)? {
            return Ok(false);
        }
    }
    Ok(record.uint("version")? == 0
        && (record.uint("reward_amount")? == 0
            || record.field("reward_addr")?.optional()?.is_some())
        && record.uint("vdf_count")? == record.seq("vdf_reward_addr")?.len() as u64
        && record.field("hash")?.bytes()? == calc_hash(record, params)?
        && record.field("content_hash")?.bytes()? == calc_content_hash(record)?)
}
/// Native BlockHeader::validate authenticates only the first proof's farmer key.
/// Call static validity separately; this method verifies the supplied hash as-is.
pub fn authenticate(record: &Record) -> std::result::Result<(), HeaderError> {
    supported(record)?;
    let signature = record
        .field("farmer_sig")?
        .optional()?
        .ok_or(HeaderError::Invalid("missing farmer signature"))?;
    let proof = record
        .seq("proof")?
        .first()
        .ok_or(HeaderError::Invalid("block has no proof"))?
        .record()?;
    if !mmx_crypto::verify(
        &bytes(record.field("hash")?)?,
        &bytes(proof.field("farmer_key")?)?,
        &bytes(signature)?,
    )? {
        return Err(HeaderError::Invalid("invalid farmer signature"));
    }
    Ok(())
}
/// Scalar checks performed by Node::validate before transaction execution. The
/// parent must already be authenticated and correspond to the current snapshot.
pub fn validate_context(
    record: &Record,
    previous: &mmx::BlockHeader,
    params: &mmx::ChainParams,
) -> std::result::Result<(), HeaderError> {
    supported(record)?;
    let invalid = HeaderError::Invalid;
    if bytes::<32>(record.field("prev")?)? != previous.hash.0 {
        return Err(invalid("parent snapshot mismatch"));
    }
    if record.uint("version")? != 0 {
        return Err(invalid("invalid version"));
    }
    if record.uint("height")? != previous.height.wrapping_add(1) as u64 {
        return Err(invalid("invalid height"));
    }
    if record.uint("space_fork_len")? > params.challenge_interval.wrapping_mul(100) as u64 {
        return Err(invalid("space fork too long"));
    }
    let timestamp = signed(record, "time_stamp")?;
    if timestamp < 0 {
        return Err(invalid("negative time stamp"));
    }
    let delta = timestamp.wrapping_sub(previous.time_stamp);
    let interval = (record.uint("vdf_count")? as i64).wrapping_mul(params.block_interval_ms);
    if delta > interval.wrapping_mul(2) {
        return Err(invalid("time stamp delta too high"));
    }
    if delta < interval / 2 {
        return Err(invalid("time stamp delta too low"));
    }
    if record.uint("time_diff")? < params.time_diff_divider || record.uint("space_diff")? == 0 {
        return Err(invalid("invalid difficulty"));
    }
    Ok(())
}
