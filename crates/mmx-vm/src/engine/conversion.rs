use super::*;

/// Native CONV flag bytes. Destination format occupies bits 8..16.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Conversion {
    Default = 0,
    Base2 = 1,
    Base8 = 2,
    Base10 = 3,
    Base16 = 4,
    Bool = 5,
    Uint = 6,
    String = 7,
    Binary = 8,
    Array = 9,
    Address = 10,
    BigEndian = 11,
    LittleEndian = 12,
}

fn base(flag: u8, default: u32) -> Result<u32> {
    Ok(match flag {
        0 => default,
        1 => 2,
        2 => 8,
        3 => 10,
        4 => 16,
        _ => return Err(Error::Conversion),
    })
}
fn big_endian(flag: u8) -> Result<bool> {
    match flag {
        0 | 11 => Ok(true),
        12 => Ok(false),
        _ => Err(Error::Conversion),
    }
}
fn digit(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u32),
        b'a'..=b'z' => Some((byte - b'a') as u32 + 10),
        b'A'..=b'Z' => Some((byte - b'A') as u32 + 10),
        _ => None,
    }
}
fn format_uint(mut value: U256, base: u32) -> Vec<u8> {
    if value.is_zero() {
        return vec![b'0'];
    }
    let mut out = Vec::new();
    while !value.is_zero() {
        out.push(b"0123456789abcdef"[(value % base).low_u32() as usize]);
        value /= base;
    }
    out.reverse();
    out
}

impl<S: Storage> Engine<'_, S> {
    pub(super) fn convert(&mut self, dst: u64, src: u64, dflags: u64, sflags: u64) -> Result<()> {
        let (kind, format, source) = (dflags as u8, (dflags >> 8) as u8, sflags as u8);
        let value = self.load(src)?;
        if kind == Conversion::Bool as u8 {
            return self.write_value(dst, Value::Bool(value.is_true()), false);
        }
        let mut assign = false;
        let result = match value {
            Value::Nil => match kind {
                6 => Value::from(0u64),
                10 => Value::Binary(vec![0; 32]),
                _ => return Err(Error::Conversion),
            },
            Value::Bool(b) if kind == 6 => Value::from(u64::from(b)),
            Value::Uint(n) => match kind {
                6 => Value::Uint(n),
                7 => {
                    let base = base(format, 10)?;
                    let cost = match n.bits() {
                        129.. => {
                            if base == 10 {
                                return Err(Error::Conversion);
                            }
                            250_000
                        }
                        65..=128 => 25_000,
                        33..=64 => 1000,
                        _ => 500,
                    };
                    self.charge(cost)?;
                    assign = true;
                    Value::String(format_uint(n, base))
                }
                8 => {
                    let mut bytes = vec![0; 32];
                    if big_endian(format)? {
                        n.to_big_endian(&mut bytes);
                    } else {
                        n.to_little_endian(&mut bytes);
                    }
                    assign = true;
                    Value::Binary(bytes)
                }
                _ => return Err(Error::Conversion),
            },
            Value::String(bytes) => match kind {
                6 => {
                    let default = if bytes.starts_with(b"0x") {
                        16
                    } else if bytes.starts_with(b"0b") {
                        2
                    } else {
                        10
                    };
                    let base = base(source, default)?;
                    let bytes = if bytes.starts_with(b"0x") || bytes.starts_with(b"0b") {
                        &bytes[2..]
                    } else {
                        &bytes[..]
                    };
                    if bytes.iter().any(|b| digit(*b).is_none_or(|d| d >= base)) {
                        return Err(Error::Conversion);
                    }
                    self.charge(bytes.len() as u64 * 200)?;
                    let n = bytes.iter().fold(U256::zero(), |n, b| {
                        n.overflowing_mul(U256::from(base))
                            .0
                            .overflowing_add(U256::from(digit(*b).unwrap()))
                            .0
                    });
                    Value::Uint(n)
                }
                7 => Value::String(bytes),
                8 => {
                    assign = true;
                    Value::Binary(match source {
                        0 => bytes,
                        4 => {
                            if bytes.len() % 2 != 0 {
                                return Err(Error::Conversion);
                            }
                            let bytes = bytes.strip_prefix(b"0x").unwrap_or(&bytes);
                            if bytes.iter().any(|b| digit(*b).is_none_or(|d| d >= 16)) {
                                return Err(Error::Conversion);
                            }
                            bytes
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|v| (digit(v[0]).unwrap() * 16 + digit(v[1]).unwrap()) as u8)
                                .collect()
                        }
                        _ => return Err(Error::Conversion),
                    })
                }
                10 => {
                    self.charge(1000)?;
                    assign = true;
                    Value::Binary(
                        crate::address::decode(&bytes)
                            .map_err(|_| Error::Conversion)?
                            .to_vec(),
                    )
                }
                _ => return Err(Error::Conversion),
            },
            Value::Binary(bytes) => match kind {
                6 => {
                    if bytes.len() != 32 {
                        return Err(Error::Conversion);
                    }
                    Value::Uint(if big_endian(source)? {
                        U256::from_big_endian(&bytes)
                    } else {
                        U256::from_little_endian(&bytes)
                    })
                }
                7 => {
                    assign = true;
                    Value::String(match format {
                        0 => bytes,
                        4 => bytes
                            .iter()
                            .flat_map(|b| {
                                [
                                    b"0123456789ABCDEF"[(b >> 4) as usize],
                                    b"0123456789ABCDEF"[(b & 15) as usize],
                                ]
                            })
                            .collect(),
                        10 => {
                            self.charge(1000)?;
                            crate::address::encode(
                                &bytes.try_into().map_err(|_| Error::Conversion)?,
                            )
                            .into_bytes()
                        }
                        _ => return Err(Error::Conversion),
                    })
                }
                8 => Value::Binary(bytes),
                10 if bytes.len() == 32 => Value::Binary(bytes),
                _ => return Err(Error::Conversion),
            },
            _ => return Err(Error::Conversion),
        };
        self.write_value(dst, result, assign)
    }
}
