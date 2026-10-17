//! Versioned lossless storage records. Native peer records remain self-describing.
use super::{invalid, ChainResult};
use mmx_consensus::Record;

const MAGIC: &[u8; 8] = b"MMXR\x01\0\0\0";
pub fn encode(record: &Record) -> ChainResult<Vec<u8>> {
    let payload = record.encode_compact()?;
    let mut out = Vec::with_capacity(MAGIC.len() + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&payload);
    Ok(out)
}
pub fn decode(bytes: &[u8]) -> ChainResult<Record> {
    if bytes.starts_with(b"MMXR") {
        if !bytes.starts_with(MAGIC) {
            return Err(invalid("unknown archived record format"));
        }
        Ok(Record::decode_compact(&bytes[MAGIC.len()..])?)
    } else {
        // Earlier Rust databases retained full native wire declarations.
        Ok(Record::decode(bytes)?)
    }
}
