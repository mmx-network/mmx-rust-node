//! Native timelord proof admission, CPU/OpenCL work verification and
//! Node::find_vdf_points output linkage. Hashes and signatures are independent
//! stages; verified work evidence and current history admission are both required
//! to construct a `VerifiedPoint`.
use crate::{
    hash, input,
    proof::bytes,
    transitions::history::{self, HeaderSource},
    Hash, Record, Result,
};
use mmx_types::{mmx, VnxType};
use rayon::prelude::*;
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
/// Lane-batched equivalent of Node::verify_vdf_cpu. Each segment starts from the
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
    let initial: Hash = reward.finalize().into();
    // Native CPU verification caps chunks at sixteen independent segments.
    // Keep SIMD groups intact, but expose enough chunks to fill the shared pool
    // for short proofs too, instead of serializing sixteen lanes on one worker.
    let group = match mmx_crypto::recursive::backend() {
        mmx_crypto::recursive::Backend::ShaNi | mmx_crypto::recursive::Backend::ArmSha2 => 2,
        mmx_crypto::recursive::Backend::Avx2 => 8,
        mmx_crypto::recursive::Backend::Scalar => 1,
    };
    let workers = rayon::current_num_threads().max(1);
    let waves = segments.len().div_ceil(workers.saturating_mul(16));
    let batch = segments
        .len()
        .div_ceil(waves.saturating_mul(workers))
        .div_ceil(group)
        * group;
    let failed = std::sync::atomic::AtomicUsize::new(usize::MAX);
    // The supplied preceding digest remains the input even when another segment
    // is wrong. Indexed collection retains deterministic first-failure reporting.
    let results: Vec<VdfResult<()>> = segments
        .par_chunks(batch)
        .enumerate()
        .map(|(chunk, supplied)| {
            use std::sync::atomic::Ordering;
            // Native skips pending chunks after a bad segment. Keep lower
            // chunks eligible so first-failure reporting stays deterministic.
            if chunk > failed.load(Ordering::Relaxed) {
                return Ok(());
            }
            let result: VdfResult<()> = (|| {
                let mut inputs = [[0; 32]; 16];
                let mut outputs = [[0; 32]; 16];
                for (lane, segment) in supplied.iter().enumerate() {
                    let index = chunk * batch + lane;
                    outputs[lane] = segment
                        .bytes()?
                        .try_into()
                        .map_err(|_| input("VDF segment size"))?;
                    inputs[lane] = if index == 0 {
                        initial
                    } else {
                        segments[index - 1]
                            .bytes()?
                            .try_into()
                            .map_err(|_| input("VDF segment size"))?
                    };
                }
                mmx_crypto::recursive::hash_many(&mut inputs[..supplied.len()], size);
                for lane in 0..supplied.len() {
                    if inputs[lane] != outputs[lane] {
                        return Err(invalid(format!(
                            "invalid output on segment {}",
                            chunk * batch + lane
                        )));
                    }
                }
                Ok(())
            })();
            if result.is_err() {
                failed.fetch_min(chunk, Ordering::Relaxed);
            }
            result
        })
        .collect();
    for result in results {
        result?;
    }
    Ok(())
}

/// Owns the exact work-checked proof. No caller can substitute unchecked fields
/// or treat this as evidence that the proof belongs to the current fork.
pub struct WorkVerifiedProof(Record);
/// Compatibility name for callers using the portable CPU constructor.
pub type CpuVerifiedProof = WorkVerifiedProof;
impl WorkVerifiedProof {
    pub fn verify(record: Record) -> VdfResult<Self> {
        verify_cpu(&record)?;
        Ok(Self(record))
    }
    /// GPU checks the same independently supplied segment inputs as the CPU.
    /// A driver error falls back to CPU; an incorrect segment is rejected.
    pub fn verify_with(
        record: Record,
        accelerator: &crate::accelerator::Accelerator,
    ) -> VdfResult<Self> {
        verify_accelerated(&record, accelerator)?;
        Ok(Self(record))
    }
    pub fn admit<S: HeaderSource>(
        &self,
        params: &mmx::ChainParams,
        source: &S,
    ) -> VdfResult<VerifiedPoint> {
        validate(&self.0, params, source)?;
        verified_point(&self.0)
    }
}

pub fn verify_accelerated(
    record: &Record,
    accelerator: &crate::accelerator::Accelerator,
) -> VdfResult<()> {
    supported(record)?;
    let segments = record.seq("segments")?;
    if segments.is_empty() {
        return Err(invalid("VDF without segments"));
    }
    let size: u32 = record
        .uint("segment_size")?
        .try_into()
        .map_err(|_| input("VDF segment size range"))?;
    let outputs: Vec<Hash> = segments
        .iter()
        .map(|v| v.bytes()?.try_into().map_err(|_| input("VDF segment size")))
        .collect::<Result<_>>()?;
    let mut first = Sha256::new();
    first.update(bytes::<32>(record, "input")?);
    first.update(bytes::<32>(record, "prev")?);
    let mut reward = Sha256::new();
    reward.update(first.finalize());
    reward.update(bytes::<32>(record, "reward_addr")?);
    let initial: Hash = reward.finalize().into();
    let inputs: Vec<_> = std::iter::once(initial)
        .chain(outputs.iter().copied().take(outputs.len() - 1))
        .collect();
    let Some(actual) = accelerator.repeated_sha256(&inputs, size) else {
        return verify_cpu(record);
    };
    for (i, (actual, expected)) in actual.iter().zip(&outputs).enumerate() {
        if actual != expected {
            return Err(invalid(format!("invalid output on segment {i}")));
        }
    }
    Ok(())
}

/// Immutable evidence of successful signature, history, shape and work checks.
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
    verified_point(record)
}
fn verified_point(record: &Record) -> VdfResult<VerifiedPoint> {
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

/// Native find_next_vdf_points. Points must be work verified; signed metadata
/// alone cannot extend a peak. Input order breaks equal receive-time ties.
pub fn next_points<'a, S: HeaderSource>(
    source: &S,
    params: &mmx::ChainParams,
    peak: &mmx::BlockHeader,
    points: &'a [VerifiedPoint],
) -> VdfResult<Vec<&'a VerifiedPoint>> {
    use std::collections::BTreeMap;
    let mut paths = BTreeMap::from([(peak.vdf_output.0, None)]);
    let mut peaks = Vec::new();
    let mut iters = peak.vdf_iters;
    for offset in 0..params.max_vdf_count {
        let infusion = history::infusion(source, params, peak, offset)?;
        iters = iters.wrapping_add(infusion.num_iters);
        let mut next = Vec::new();
        for (i, p) in points.iter().enumerate() {
            let p = p.point();
            if p.start.wrapping_add(p.num_iters) == iters
                && paths.contains_key(&p.input.0)
                && p.prev.0 == infusion.hash
                && p.num_iters == infusion.num_iters
            {
                paths.insert(p.output.0, Some(i));
                next.push(i);
            }
        }
        if next.is_empty() {
            break;
        }
        peaks = next;
    }
    let Some(mut current) = peaks
        .into_iter()
        .min_by_key(|&i| (points[i].point().recv_time, i))
        .map(Some)
    else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    while let Some(i) = current {
        if out.len() >= points.len() {
            return Err(invalid("cyclic VDF points"));
        }
        out.push(&points[i]);
        current = *paths
            .get(&points[i].point().input.0)
            .ok_or_else(|| invalid("missing VDF point input"))?;
    }
    out.reverse();
    Ok(out)
}

#[cfg(test)]
mod next_tests {
    use super::*;
    use vnx::serde_json::{self, Value};
    fn hash(n: u64) -> Hash {
        let mut h = [0; 32];
        h[..8].copy_from_slice(&n.to_le_bytes());
        h
    }
    #[test]
    fn native_next_vdf_paths_infusions_iteration_indexes_and_arrival_order() {
        let mut count = 0;
        for text in include_str!("../tests/fixtures/next_vdf.jsonl").lines() {
            let r: Value = serde_json::from_str(text).unwrap();
            let params = mmx::ChainParams {
                commit_delay: 0,
                infuse_delay: 0,
                max_vdf_count: r["maximum"].as_u64().unwrap() as u32,
                time_diff_divider: 1,
                time_diff_constant: 1,
                ..Default::default()
            };
            let peak = mmx::BlockHeader {
                hash: hash(41).into(),
                time_diff: r["num"].as_u64().unwrap(),
                vdf_iters: r["iters"].as_str().unwrap().parse().unwrap(),
                ..Default::default()
            };
            // Synthetic already-verified points exercise the native selection policy;
            // public callers can only create this capability via work verification.
            let points: Vec<_> = r["points"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| VerifiedPoint {
                    point: mmx::VDF_Point {
                        start: p["start"].as_str().unwrap().parse().unwrap(),
                        num_iters: p["num"].as_u64().unwrap(),
                        input: hash(p["input"].as_u64().unwrap()).into(),
                        output: hash(p["output"].as_u64().unwrap()).into(),
                        prev: hash(p["prev"].as_u64().unwrap()).into(),
                        recv_time: p["recv_time"].as_i64().unwrap(),
                        ..Default::default()
                    },
                    proof: Default::default(),
                })
                .collect();
            let source = std::collections::BTreeMap::<Hash, mmx::BlockHeader>::new();
            let actual: Vec<_> = next_points(&source, &params, &peak, &points)
                .unwrap()
                .into_iter()
                .map(|p| vnx::util::to_hex_string(&p.point().output.0, false, false).to_uppercase())
                .collect();
            let expected: Vec<_> = r["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_uppercase())
                .collect();
            assert_eq!(actual, expected, "{}", r["name"]);
            count += 1;
        }
        assert_eq!(count, 240);
    }
}
