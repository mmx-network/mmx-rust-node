//! Native MMX ECDSA verification shared by authorization and VM VERIFY.
//! Addresses are SHA-256 of the original 33 public-key bytes, including invalid
//! encodings. Verification preserves the native cache and parsing order.
use k256::ecdsa::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Copy, PartialEq, Eq)]
struct SignatureEntry {
    message: [u8; 32],
    key: [u8; 33],
    signature: [u8; 64],
}
static SIGNATURE_CACHE: LazyLock<Mutex<Vec<SignatureEntry>>> = LazyLock::new(|| {
    Mutex::new(vec![
        SignatureEntry {
            message: [0; 32],
            key: [0; 33],
            signature: [0; 64]
        };
        16384
    ])
});

fn signature_slot(signature: &[u8; 64]) -> usize {
    let mut hash = vnx::Crc64::new();
    hash.update(vnx::hash64(signature).to_le_bytes());
    // For fixed-length CRC inputs, replacing the native random salt by zero
    // XOR-permutes all slot indices. Collisions and cache behavior are identical,
    // including eviction of the initially zero entry; no RNG is required.
    hash.update([0; 8]);
    hash.as_u64() as usize % 16384
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VerifyError {
    #[error("secp256k1_ecdsa_signature_parse_compact() failed")]
    SignatureEncoding,
    #[error("secp256k1_ec_pubkey_parse() failed")]
    PublicKeyEncoding,
}

/// Hash the encoded key without parsing it, as native pubkey_t::get_addr does.
pub fn public_key_address(key: &[u8; 33]) -> [u8; 32] {
    Sha256::digest(key).into()
}

/// Return false for invalid signatures, including high-S and zero scalars.
/// Out-of-range scalars and malformed keys return distinct parse errors. The
/// shared cache is checked first, including its native initially-zero tuple.
pub fn verify(
    message: &[u8; 32],
    key: &[u8; 33],
    signature: &[u8; 64],
) -> Result<bool, VerifyError> {
    let entry = SignatureEntry {
        message: *message,
        key: *key,
        signature: *signature,
    };
    let slot = signature_slot(signature);
    if SIGNATURE_CACHE.lock().unwrap()[slot] == entry {
        return Ok(true);
    }
    const ORDER: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x41,
    ];
    // libsecp256k1 parses the signature before the public key. k256 rejects
    // zero scalars earlier, so parse the key before returning false for those.
    if signature[..32] >= ORDER[..] || signature[32..] >= ORDER[..] {
        return Err(VerifyError::SignatureEncoding);
    }
    let key = VerifyingKey::from_sec1_bytes(key).map_err(|_| VerifyError::PublicKeyEncoding)?;
    let valid = match Signature::from_slice(signature) {
        Ok(signature) => key.verify_prehash(message, &signature).is_ok(),
        Err(_) => false,
    };
    if valid {
        SIGNATURE_CACHE.lock().unwrap()[slot] = entry;
    }
    Ok(valid)
}
