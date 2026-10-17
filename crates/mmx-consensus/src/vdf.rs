//! Native timelord proof admission, portable CPU work verification and
//! Node::find_vdf_points output linkage. Hashes and signatures are independent
//! stages; only `verify` can construct a `VerifiedPoint`.
use crate::{
    hash, input,
    proof::bytes,
    transitions::history::{self, HeaderSource},
    Hash, Record, Result,
};
use mmx_types::{mmx, VnxType};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum VdfError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Input(#[from] crate::Error),
    #[error(transparent)]
    Crypto(#[from] mmx_crypto::VerifyError),
    #[error(transparent)]
    History(#[from] crate::proof::ProofError),
}
pub type VdfResult<T> = std::result::Result<T, VdfError>;
fn invalid(message: impl Into<String>) -> VdfError {
    VdfError::Invalid(message.into())
}
fn supported(record: &Record) -> Result<()> {
    if record.name != "mmx.ProofOfTime" {
        return Err(input("expected proof of time"));
    }
    Ok(())
}
/// Native WriteBytes preimage for a ProofOfTime or VDF_Point (version zero).
pub fn hash_serialize(record: &Record) -> Result<Vec<u8>> {
    hash::vdf(record)
}
/// ProofOfTime content hash: SHA256(hash || timelord_key || timelord_sig).
pub fn calc_content_hash(record: &Record) -> Result<Hash> {
    supported(record)?;
    let mut hash = Sha256::new();
    hash.update(bytes::<32>(record, "hash")?);
    hash.update(bytes::<33>(record, "timelord_key")?);
    hash.update(bytes::<64>(record, "timelord_sig")?);
    Ok(hash.finalize().into())
}
pub fn is_valid(record: &Record) -> Result<bool> {
    if record.name == "mmx.VDF_Point" {
        // Native point validity authenticates metadata only, not embedded work.
        return Ok(bytes::<32>(record, "content_hash")? == record.calc_hash(false, 0)?);
    }
    supported(record)?;
    Ok(record.uint("version")? == 0
        && record.uint("vdf_height")? != 0
        && record.uint("num_iters")? != 0
        && !record.seq("segments")?.is_empty()
        && bytes::<32>(record, "hash")? == record.calc_hash(false, 0)?
        && bytes::<32>(record, "content_hash")? == calc_content_hash(record)?)
}
pub fn output(record: &Record) -> Result<Hash> {
    supported(record)?;
    record.seq("segments")?.last().map_or(Ok([0; 32]), |v| {
        v.bytes()?.try_into().map_err(|_| input("VDF segment size"))
    })
}
pub fn end_iters(record: &Record) -> Result<u64> {
    supported(record)?;
    Ok(record
        .uint("start")?
        .wrapping_add(record.uint("num_iters")?))
}
/// Verify the supplied hash as-is, matching ProofOfTime::validate. Static
/// validity is a separate prerequisite and uses the shared native crypto cache.
pub fn authenticate(record: &Record) -> VdfResult<()> {
    supported(record)?;
    if !mmx_crypto::verify(
        &bytes(record, "hash")?,
        &bytes(record, "timelord_key")?,
        &bytes(record, "timelord_sig")?,
    )? {
        return Err(invalid("invalid timelord signature"));
    }
    Ok(())
}
/// Node::verify_vdf admission, before the asynchronous native CPU task. The
/// source must contain authenticated history for this chain's parameters.
pub fn validate<S: HeaderSource>(
    record: &Record,
    params: &mmx::ChainParams,
    source: &S,
) -> VdfResult<()> {
    supported(record)?;
    if !is_valid(record)? {
        return Err(invalid("static validation failed"));
    }
    authenticate(record)?;
    let infused = source
        .get_header(&bytes(record, "prev")?)?
        .ok_or_else(|| invalid("could not find infused block"))?;
    let num_iters = history::block_iters(source, params, &infused)?;
    let supplied = record.uint("num_iters")?;
    if supplied != num_iters {
        return Err(invalid(format!("invalid num_iters: {supplied}")));
    }
    let size = record.uint("segment_size")?;
    if size != params.vdf_segment_size as u64 {
        return Err(invalid(format!("invalid segment size: {size}")));
    }
    let count = record.seq("segments")?.len() as u64;
    if count.wrapping_mul(size) != num_iters {
        return Err(invalid(format!("invalid segment count: {count}")));
    }
    Ok(())
}
/// Portable equivalent of Node::verify_vdf_cpu. Each segment starts from the
/// supplied preceding digest; the first mixes input, infusion and reward. When
/// several segments fail, native parallel execution may report a different
/// failing index. This implementation reports the first in sequence.
pub fn verify_cpu(record: &Record) -> VdfResult<()> {
    supported(record)?;
    let segments = record.seq("segments")?;
    if segments.is_empty() {
        return Err(invalid("VDF without segments"));
    }
    let size: u32 = record
        .uint("segment_size")?
        .try_into()
        .map_err(|_| input("VDF segment size range"))?;
    let mut initial = Sha256::new();
    initial.update(bytes::<32>(record, "input")?);
    initial.update(bytes::<32>(record, "prev")?);
    let mut reward = Sha256::new();
    reward.update(initial.finalize());
    reward.update(bytes::<32>(record, "reward_addr")?);
    let mut previous: Hash = reward.finalize().into();
    for (i, segment) in segments.iter().enumerate() {
        let supplied: Hash = segment
            .bytes()?
            .try_into()
            .map_err(|_| input("VDF segment size"))?;
        let mut value = previous;
        for _ in 0..size {
            value = Sha256::digest(value).into();
        }
        if value != supplied {
            return Err(invalid(format!("invalid output on segment {i}")));
        }
        previous = supplied;
    }
    Ok(())
}

/// Immutable evidence of successful signature, history, shape and CPU checks.
/// A decoded VDF_Point alone cannot establish this evidence. Rebuild this type
/// through `verify` when loading untrusted persisted/network records.
#[derive(Clone, Debug)]
pub struct VerifiedPoint {
    point: mmx::VDF_Point,
    proof: mmx::ProofOfTime,
}
impl VerifiedPoint {
    /// Native point metadata, with recv_time zero and no embedded proof. The
    /// signed proof is retained separately by `proof()`; neither is hashed here.
    pub fn point(&self) -> &mmx::VDF_Point {
        &self.point
    }
    pub fn proof(&self) -> &mmx::ProofOfTime {
        &self.proof
    }
}
pub fn verify<S: HeaderSource>(
    record: &Record,
    params: &mmx::ChainParams,
    source: &S,
) -> VdfResult<VerifiedPoint> {
    validate(record, params, source)?;
    verify_cpu(record)?;
    let proof = mmx::ProofOfTime::decode(&record.encode()?).map_err(crate::Error::from)?;
    let mut point = mmx::VDF_Point {
        vdf_height: proof.vdf_height,
        start: proof.start,
        num_iters: proof.num_iters,
        input: proof.input.clone(),
        output: output(record)?.into(),
        prev: proof.prev.clone(),
        reward_addr: proof.reward_addr.clone(),
        ..Default::default()
    };
    point.content_hash = Record::from_generated(&point)?.calc_hash(false, 0)?.into();
    Ok(VerifiedPoint { point, proof })
}

/// Match Node::find_vdf_points using points verified against this chain. Input
/// order is insertion order within equal-output candidates; the first matching
/// infusion/iteration pair wins, without backtracking. The result is chronological.
/// Header authentication/transitions are separate checks. Native linkage does
/// not compare signed point start/height metadata or block reward addresses.
pub fn link_block<'a, S: HeaderSource>(
    record: &Record,
    params: &mmx::ChainParams,
    source: &S,
    points: &'a [VerifiedPoint],
) -> VdfResult<Vec<&'a VerifiedPoint>> {
    if !matches!(record.name.as_str(), "mmx.Block" | "mmx.BlockHeader") {
        return Err(input("expected block header").into());
    }
    let count = record.uint("vdf_count")?;
    if count == 0 || count > params.max_vdf_count as u64 {
        return Err(invalid("invalid vdf_count"));
    }
    let parent = source
        .get_header(&bytes(record, "prev")?)?
        .ok_or_else(|| invalid("cannot find VDF parent"))?;
    let mut output: Hash = bytes(record, "vdf_output")?;
    let mut out = Vec::new();
    for offset in (0..count as u32).rev() {
        let infusion = history::infusion(source, params, &parent, offset)?;
        let point = points
            .iter()
            .find(|v| {
                v.point.output.0 == output
                    && v.point.prev.0 == infusion.hash
                    && v.point.num_iters == infusion.num_iters
            })
            .ok_or_else(|| invalid("cannot find VDF points"))?;
        output = point.point.input.0;
        out.push(point);
    }
    if output != parent.vdf_output.0 {
        return Err(invalid("VDF input does not match parent output"));
    }
    out.reverse();
    Ok(out)
}
