//! Native farmer ProofResponse envelopes and selected-history eligibility.
use crate::{
    input,
    proof::{self, bytes, VerifiedProof},
    transitions::history::{self, HeaderSource},
    Hash, Record,
};
use mmx_types::mmx;
use sha2::{Digest, Sha256};
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Input(#[from] crate::Error),
    #[error(transparent)]
    Crypto(#[from] mmx_crypto::VerifyError),
    #[error(transparent)]
    Proof(#[from] proof::ProofError),
}
pub type Result<T> = std::result::Result<T, Error>;
fn supported(record: &Record) -> crate::Result<()> {
    if record.name != "mmx.ProofResponse" {
        return Err(input("expected proof response"));
    }
    Ok(())
}
pub fn content_hash(record: &Record) -> crate::Result<Hash> {
    supported(record)?;
    let mut h = Sha256::new();
    h.update(bytes::<32>(record, "hash")?);
    h.update(bytes::<64>(record, "farmer_sig")?);
    Ok(h.finalize().into())
}
pub fn is_valid(record: &Record) -> crate::Result<bool> {
    supported(record)?;
    let Some(p) = record.field("proof")?.optional()? else {
        return Ok(false);
    };
    Ok(proof::is_valid(p.record()?)?
        && record.field("harvester")?.text()?.len() < 1024
        && bytes::<32>(record, "hash")? == record.calc_hash(false, 0)?
        && bytes::<32>(record, "content_hash")? == content_hash(record)?)
}
/// Native validate is independent of is_valid; callers must apply both gates.
pub fn authenticate(record: &Record) -> Result<()> {
    supported(record)?;
    let p = record
        .field("proof")?
        .optional()?
        .ok_or(Error::Invalid("missing proof"))?
        .record()?;
    if bytes::<32>(p, "plot_id")? != proof::calc_plot_id(p)? {
        return Err(Error::Invalid("invalid plot id"));
    }
    if !mmx_crypto::verify(
        &bytes(record, "hash")?,
        &bytes(p, "farmer_key")?,
        &bytes(record, "farmer_sig")?,
    )? {
        return Err(Error::Invalid("invalid farmer signature"));
    }
    Ok(())
}
pub fn challenge<S: HeaderSource>(
    source: &S,
    params: &mmx::ChainParams,
    peak: &mmx::BlockHeader,
    height: u32,
    max_history: u32,
) -> Result<(Hash, u64)> {
    if (height > peak.vdf_height && height - peak.vdf_height > params.max_vdf_count)
        || (height <= peak.vdf_height && peak.vdf_height - height > max_history)
    {
        return Err(Error::Invalid("cannot find challenge"));
    }
    let mut block = peak.clone();
    while block.vdf_height > height {
        block = source
            .get_header(&block.prev.0)?
            .ok_or(Error::Invalid("cannot find challenge"))?;
    }
    Ok(history::challenge(
        source,
        params,
        &block,
        height - block.vdf_height,
    )?)
}
pub fn verify<S: HeaderSource>(
    record: &Record,
    params: &mmx::ChainParams,
    source: &S,
    peak: &mmx::BlockHeader,
    root_vdf_height: u32,
    max_history: u32,
) -> Result<VerifiedProof> {
    if !is_valid(record)? {
        return Err(Error::Invalid("invalid response"));
    }
    authenticate(record)?;
    let height = record.uint("vdf_height")? as u32;
    if height <= root_vdf_height {
        return Err(Error::Invalid("proof too old"));
    }
    let (challenge, diff) = challenge(source, params, peak, height, max_history)?;
    Ok(proof::verify_evidence(
        record.field("proof")?.record()?,
        params,
        &challenge,
        diff,
        height,
    )?)
}
