//! Native ValidatorVote signatures and Node::get_validators history traversal.
use crate::{input, proof::bytes, transitions::history::HeaderSource, Hash, Record};
use mmx_types::mmx;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub type FarmerKey = [u8; 33];
#[derive(Debug, thiserror::Error)]
pub enum VoteError {
    #[error("invalid validator vote")]
    Invalid,
    #[error(transparent)]
    Input(#[from] crate::Error),
    #[error(transparent)]
    Crypto(#[from] mmx_crypto::VerifyError),
    #[error(transparent)]
    History(#[from] crate::proof::ProofError),
}
pub type VoteResult<T> = Result<T, VoteError>;
fn supported(record: &Record) -> crate::Result<()> {
    if record.name != "mmx.ValidatorVote" {
        return Err(input("expected validator vote"));
    }
    Ok(())
}
/// Hash raw block/key bytes after the literal domain, without presentation or
/// VNX/hash-field framing, matching Farmer::sign_vote and ValidatorVote::is_valid.
pub fn signing_hash(block: &Hash, farmer: &FarmerKey) -> Hash {
    let mut h = Sha256::new();
    h.update(b"MMX/validator/vote/");
    h.update(block);
    h.update(farmer);
    h.finalize().into()
}
pub fn content_hash(record: &Record) -> crate::Result<Hash> {
    supported(record)?;
    let mut h = Sha256::new();
    h.update(bytes::<32>(record, "hash")?);
    h.update(bytes::<33>(record, "farmer_key")?);
    h.update(bytes::<64>(record, "farmer_sig")?);
    Ok(h.finalize().into())
}
/// Content hash comparison short-circuits signature/key parsing, as in C++.
pub fn is_valid(record: &Record) -> VoteResult<bool> {
    supported(record)?;
    if bytes::<32>(record, "content_hash")? != content_hash(record)? {
        return Ok(false);
    }
    Ok(mmx_crypto::verify(
        &signing_hash(&bytes(record, "hash")?, &bytes(record, "farmer_key")?),
        &bytes(record, "farmer_key")?,
        &bytes(record, "farmer_sig")?,
    )?)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedVote {
    block: Hash,
    farmer: FarmerKey,
    id: Hash,
    record: Record,
}
impl VerifiedVote {
    pub fn block_hash(&self) -> &Hash {
        &self.block
    }
    pub fn farmer_key(&self) -> &FarmerKey {
        &self.farmer
    }
    pub fn content_hash(&self) -> &Hash {
        &self.id
    }
    pub fn record(&self) -> &Record {
        &self.record
    }
}
pub fn verify(record: &Record) -> VoteResult<VerifiedVote> {
    if !is_valid(record)? {
        return Err(VoteError::Invalid);
    }
    Ok(VerifiedVote {
        block: bytes(record, "hash")?,
        farmer: bytes(record, "farmer_key")?,
        id: bytes(record, "content_hash")?,
        record: record.clone(),
    })
}

/// The initial commit-delay walk is NOT clamped to genesis. A missing header
/// terminates lookup; history read errors propagate. The scan counts headers,
/// not distinct keys, and takes the first proof's key only.
pub fn validators<S: HeaderSource>(
    source: &S,
    params: &mmx::ChainParams,
    block: &mmx::BlockHeader,
    max_history: u32,
) -> VoteResult<BTreeSet<FarmerKey>> {
    let mut current = Some(block.clone());
    for _ in 0..params.commit_delay {
        let Some(header) = current else { break };
        current = source.get_header(&header.prev.0)?;
    }
    let mut keys = BTreeSet::new();
    for _ in 0..max_history {
        if keys.len() >= params.max_validators as usize {
            break;
        }
        let Some(header) = current else { break };
        if let Some(proof) = header.proof.first() {
            let key = vnx::serde_json::from_value::<mmx::pubkey_t>(
                proof
                    .get("farmer_key")
                    .cloned()
                    .ok_or_else(|| input("missing validator farmer key"))?,
            )
            .map_err(vnx::Error::from)
            .map_err(crate::Error::from)?;
            keys.insert(key.0);
        }
        current = source.get_header(&header.prev.0)?;
    }
    Ok(keys)
}
