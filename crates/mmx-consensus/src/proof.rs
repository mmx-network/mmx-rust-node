//! Proof record hashing, plot identity and native CPU proof-of-space validation.
use crate::{input, Error, Hash, Record, Result};
use mmx_types::mmx;
use primitive_types::U256;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Input(#[from] Error),
    #[error(transparent)]
    Work(#[from] crate::pos::Error),
}
pub type ProofResult<T> = std::result::Result<T, ProofError>;
pub(crate) fn invalid(message: &str) -> ProofError {
    ProofError::Invalid(message.into())
}
pub(crate) fn bytes<const N: usize>(record: &Record, field: &str) -> Result<[u8; N]> {
    record
        .field(field)?
        .bytes()?
        .try_into()
        .map_err(|_| input("proof byte length"))
}
pub fn plot_challenge(challenge: &Hash, plot_id: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update(b"plot_challenge");
    h.update(plot_id);
    h.update(challenge);
    h.finalize().into()
}
pub fn check_plot_filter(challenge: &Hash, plot_id: &Hash, bits: u32) -> bool {
    let mut h = Sha256::new();
    h.update(b"plot_filter");
    h.update(plot_id);
    h.update(challenge);
    crate::pos::leading_zero_bits(&h.finalize().into(), bits)
}
pub fn check_threshold(
    params: &mmx::ChainParams,
    ksize: u8,
    quality: &Hash,
    space_diff: u64,
    hard_fork: bool,
) -> ProofResult<bool> {
    if space_diff == 0 {
        return Ok(false);
    }
    if ksize == 0 || params.post_filter > 256 || params.space_diff_constant == 0 {
        return Err(invalid("invalid threshold parameters"));
    }
    let denominator = U256::from(space_diff as u128 * params.space_diff_constant as u128);
    let threshold = ((U256::one() << 255) / denominator)
        .overflowing_mul(U256::from(2 * ksize as u64 + 1))
        .0;
    let mut value = U256::from_little_endian(quality);
    if hard_fork {
        value = if params.post_filter == 256 {
            U256::zero()
        } else {
            value >> params.post_filter
        };
    }
    Ok((value >> (ksize - 1)) < threshold)
}
/// Node::verify_proof(ProofOfSpace), preserving validation and failure order.
/// The expected challenge/difficulty must come from authenticated chain history.
pub fn verify(
    record: &Record,
    params: &mmx::ChainParams,
    challenge: &Hash,
    space_diff: u64,
    vdf_height: u32,
) -> ProofResult<Hash> {
    if space_diff == 0 {
        return Err(invalid("difficulty zero"));
    }
    if !is_valid(record)? {
        return Err(invalid("invalid proof"));
    }
    if bytes::<32>(record, "challenge")? != *challenge {
        return Err(invalid("invalid challenge"));
    }
    if record.uint("difficulty")? != space_diff {
        return Err(invalid("invalid difficulty"));
    }
    validate_plot_id(record)?;
    let id = bytes(record, "plot_id")?;
    if !check_plot_filter(challenge, &id, params.plot_filter) {
        return Err(invalid("plot filter failed"));
    }
    let ksize = record.uint("ksize")?;
    if ksize < params.min_ksize as u64 {
        return Err(invalid("ksize too low"));
    }
    if ksize > params.max_ksize as u64 {
        return Err(invalid("ksize too high"));
    }
    let hard_fork = vdf_height >= params.hardfork1_height;
    let xs = record
        .seq("proof_xs")?
        .iter()
        .map(|v| {
            v.uint()
                .and_then(|n| n.try_into().map_err(|_| input("proof coordinate range")))
        })
        .collect::<Result<Vec<_>>>()?;
    let quality = crate::pos::verify(
        &xs,
        &plot_challenge(challenge, &id),
        &id,
        params.plot_filter,
        params.post_filter,
        ksize as u8,
        hard_fork,
    )?;
    if !check_threshold(params, ksize as u8, &quality, space_diff, hard_fork)? {
        return Err(invalid("not good enough"));
    }
    if record.uint("score")? != calc_proof_hash(record)?[0] as u64 {
        return Err(invalid("proof score mismatch"));
    }
    Ok(quality)
}

fn supported(record: &Record) -> Result<()> {
    if matches!(
        record.name.as_str(),
        "mmx.ProofOfSpaceOG" | "mmx.ProofOfSpaceNFT"
    ) {
        Ok(())
    } else {
        Err(Error::Unsupported(record.name.clone()))
    }
}
pub fn is_valid(record: &Record) -> Result<bool> {
    if record.name == "mmx.ProofOfSpace" {
        return Ok(true);
    }
    supported(record)?;
    Ok(record.uint("ksize")? > 0 && record.seq("proof_xs")?.len() <= 256)
}
/// SHA256(challenge || SHA256(little-endian uint32 proof coordinates)).
pub fn calc_proof_hash(record: &Record) -> Result<Hash> {
    supported(record)?;
    let mut coordinates = Sha256::new();
    for x in record.seq("proof_xs")? {
        let x: u32 = x
            .uint()?
            .try_into()
            .map_err(|_| input("proof coordinate range"))?;
        coordinates.update(x.to_le_bytes());
    }
    let mut hash = Sha256::new();
    hash.update(record.field("challenge")?.bytes()?);
    hash.update(coordinates.finalize());
    Ok(hash.finalize().into())
}
pub fn calc_plot_id(record: &Record) -> Result<Hash> {
    supported(record)?;
    let mut hash = Sha256::new();
    let prefix: &[u8] = if record.name == "mmx.ProofOfSpaceNFT" {
        b"MMX/PLOTID/NFT"
    } else {
        b"MMX/PLOTID/OG"
    };
    hash.update(prefix);
    let ksize: u8 = record
        .uint("ksize")?
        .try_into()
        .map_err(|_| input("plot ksize range"))?;
    hash.update([ksize]);
    hash.update(record.field("seed")?.bytes()?);
    hash.update(record.field("farmer_key")?.bytes()?);
    if record.name == "mmx.ProofOfSpaceNFT" {
        hash.update(record.field("contract")?.bytes()?);
    }
    Ok(hash.finalize().into())
}
pub fn validate_plot_id(record: &Record) -> Result<()> {
    if record.field("plot_id")?.bytes()? != calc_plot_id(record)? {
        return Err(input("invalid plot id"));
    }
    Ok(())
}
