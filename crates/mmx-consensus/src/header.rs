//! BlockHeader hashes, static validity, farmer authentication and the contextual
//! checks in Node::validate. Space/VDF work, challenge/difficulty transitions,
//! weight verification and NFT reward-target execution remain separate stages.
use crate::{hash, input, proof, Error, Hash, Record, Result, Value};
use mmx_types::mmx;
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
