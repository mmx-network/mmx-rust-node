//! Proof record hashing and plot identity checks. These do not verify proof-of-
//! space quality, filters, difficulty or VDF work.
use crate::{input, Error, Hash, Record, Result};
use sha2::{Digest, Sha256};

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
