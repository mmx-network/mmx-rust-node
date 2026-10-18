//! Native little-endian park bits, ternary delta codes and triangular X pairs.
use crate::{error, Result};

pub fn read_bits(bytes: &[u8], offset: usize, count: usize) -> Result<u64> {
    let end = offset
        .checked_add(count)
        .ok_or_else(|| error("bit offset overflow"))?;
    if count > 64 || end > bytes.len().saturating_mul(8) {
        return Err(error("bit stream underflow"));
    }
    let mut out = 0;
    for i in 0..count {
        out |= u64::from((bytes[(offset + i) / 8] >> ((offset + i) % 8)) & 1) << i;
    }
    Ok(out)
}
pub fn decode(bytes: &[u8], count: usize, mut offset: usize) -> Result<Vec<u8>> {
    if count > bytes.len().saturating_mul(4) {
        return Err(error("symbol count exceeds bit stream"));
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let mut symbol = 48;
        for index in 0..16 {
            let digit = read_bits(bytes, offset, 2)? as u8;
            offset += 2;
            if digit != 3 {
                symbol = 3 * index + digit;
                break;
            }
        }
        out.push(symbol);
    }
    Ok(out)
}
fn triangular(x: u32) -> u64 {
    u64::from(x) * u64::from(x.saturating_sub(1)) / 2
}
pub fn line_point(index: u64, compressed: bool) -> Result<(u32, u32)> {
    let mut x = 0u32;
    for i in (0..32).rev() {
        let next = x | (1 << i);
        if triangular(next) <= index {
            x = next;
        }
    }
    let y = index
        .checked_sub(triangular(x))
        .ok_or_else(|| error("invalid line point"))?;
    let mut y: u32 = y.try_into().map_err(|_| error("invalid line point"))?;
    if y >= x {
        return Err(error("line point outside u32 square"));
    }
    if compressed {
        if y == 0 {
            x = x
                .checked_sub(1)
                .ok_or_else(|| error("invalid equal X pair"))?;
            y = x;
        }
        x = x
            .checked_sub(1)
            .ok_or_else(|| error("invalid compressed X pair"))?;
        y = y
            .checked_sub(1)
            .ok_or_else(|| error("invalid compressed X pair"))?;
    }
    Ok((x, y))
}
