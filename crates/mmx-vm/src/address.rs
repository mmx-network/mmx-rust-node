//! Consensus address presentation, ported from addr_t.cpp and libbech32.
//! The native decoder accepts arbitrary HRPs and nonzero padding bits.
use crate::{Address, CodecError, U256};

const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const BECH32M: u32 = 0x2bc830a3;

fn polymod(values: impl IntoIterator<Item = u8>) -> u32 {
    let mut check = 1u32;
    for value in values {
        let top = check >> 25;
        check = ((check & 0x1ffffff) << 5) ^ u32::from(value);
        for (i, generator) in [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3]
            .iter()
            .enumerate()
        {
            if (top >> i) & 1 != 0 {
                check ^= generator;
            }
        }
    }
    check
}

fn expand(hrp: &[u8]) -> Vec<u8> {
    hrp.iter()
        .map(|b| b >> 5)
        .chain([0])
        .chain(hrp.iter().map(|b| b & 31))
        .collect()
}

pub fn encode(address: &Address) -> String {
    let mut bits = U256::from_little_endian(address);
    let mut words = [0u8; 52];
    words[51] = (bits.low_u32() as u8 & 1) << 4;
    bits >>= 1;
    for word in words[..51].iter_mut().rev() {
        *word = bits.low_u32() as u8 & 31;
        bits >>= 5;
    }
    let check = polymod(expand(b"mmx").into_iter().chain(words).chain([0; 6])) ^ BECH32M;
    let mut out = String::from("mmx1");
    for word in words {
        out.push(CHARSET[word as usize] as char);
    }
    for i in (0..6).rev() {
        out.push(CHARSET[((check >> (5 * i)) & 31) as usize] as char);
    }
    out
}

pub fn decode(text: &[u8]) -> Result<Address, CodecError> {
    if text == b"MMX" {
        return Ok([0; 32]);
    }
    let invalid = || CodecError::Invalid("MMX address");
    if !(8..=90).contains(&text.len())
        || text.iter().any(|b| !(33..=126).contains(b))
        || (text.iter().any(u8::is_ascii_lowercase) && text.iter().any(u8::is_ascii_uppercase))
    {
        return Err(invalid());
    }
    let text = text.to_ascii_lowercase();
    let pos = text.iter().rposition(|b| *b == b'1').ok_or_else(invalid)?;
    if pos == 0 || pos > 83 || text.len() - pos - 1 < 6 {
        return Err(invalid());
    }
    let words: Vec<u8> = text[pos + 1..]
        .iter()
        .map(|b| {
            CHARSET
                .iter()
                .position(|c| c == b)
                .map(|n| n as u8)
                .ok_or_else(invalid)
        })
        .collect::<Result<_, _>>()?;
    if words.len() != 58
        || polymod(
            expand(&text[..pos])
                .into_iter()
                .chain(words.iter().copied()),
        ) != BECH32M
    {
        return Err(invalid());
    }
    let mut bits = U256::zero();
    for word in &words[..51] {
        bits = (bits << 5) | U256::from(*word);
    }
    bits = (bits << 1) | U256::from(words[51] >> 4);
    let mut out = [0; 32];
    bits.to_little_endian(&mut out);
    Ok(out)
}
