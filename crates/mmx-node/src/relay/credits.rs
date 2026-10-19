//! Router's earned early-relay permissions, separate from consensus evidence.
use crate::{error, node::number, Result};
use mmx_consensus::Record;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
type Key = [u8; 33];
const MAX_KEYS: usize = 100_000;
#[derive(Default)]
pub(super) struct Credits {
    pub vdf_height: u32,
    pub peak_height: u32,
    farmers: BTreeMap<u32, BTreeSet<Key>>,
    farmer_count: usize,
    timelords: BTreeMap<Key, bool>,
    timelord_order: VecDeque<Key>,
}

fn key(record: &Record, field: &str) -> Result<Key> {
    record
        .field(field)?
        .bytes()?
        .try_into()
        .map_err(|_| error("invalid relay key length"))
}
impl Credits {
    pub fn verified(&mut self, record: &Record) -> Result<()> {
        match record.name.as_str() {
            "mmx.Block" => {
                // Native prunes through the PREVIOUS verified VDF height. A
                // lesser block cannot erase a better proof at the new height.
                while self
                    .farmers
                    .first_key_value()
                    .is_some_and(|(h, _)| *h <= self.vdf_height)
                {
                    self.farmer_count -= self.farmers.pop_first().unwrap().1.len();
                }
                self.vdf_height = number(record, "vdf_height")?;
                self.peak_height = number(record, "height")?;
                // Blocks deliberately earn no farmer or timelord permissions.
            }
            "mmx.ProofResponse" => {
                let height = number(record, "vdf_height")?;
                let farmer = key(record.field("proof")?.record()?, "farmer_key")?;
                if self.farmer_count < MAX_KEYS
                    && self.farmers.entry(height).or_default().insert(farmer)
                {
                    self.farmer_count += 1;
                }
            }
            "mmx.ProofOfTime" => {
                let timelord = key(record, "timelord_key")?;
                if self.timelords.insert(timelord, true).is_none() {
                    self.timelord_order.push_back(timelord);
                    if self.timelord_order.len() > MAX_KEYS {
                        self.timelords
                            .remove(&self.timelord_order.pop_front().unwrap());
                    }
                }
                self.vdf_height = self.vdf_height.max(number(record, "vdf_height")?);
            }
            _ => (),
        }
        Ok(())
    }
    pub fn consume(&mut self, record: &Record, max_segments: usize) -> Result<bool> {
        match record.name.as_str() {
            "mmx.Block" => {
                let Some(proof) = record.field("proof")?.seq()?.first() else {
                    return Ok(false);
                };
                let farmer = key(proof.record()?, "farmer_key")?;
                let height = number(record, "vdf_height")?;
                Ok(self.farmers.get_mut(&height).is_some_and(|keys| {
                    let found = keys.remove(&farmer);
                    self.farmer_count -= usize::from(found);
                    found
                }))
            }
            "mmx.ProofOfTime" if record.field("segments")?.seq()?.len() < max_segments => Ok(self
                .timelords
                .get_mut(&key(record, "timelord_key")?)
                .is_some_and(std::mem::take)),
            _ => Ok(false),
        }
    }
    pub fn too_old(&self, record: &Record, delay: u32) -> Result<bool> {
        Ok(match record.name.as_str() {
            "mmx.Block" => {
                u64::from(number(record, "height")?) + u64::from(delay)
                    < u64::from(self.peak_height)
            }
            "mmx.ProofOfTime" => {
                u64::from(number(record, "vdf_height")?) + u64::from(delay)
                    < u64::from(self.vdf_height)
            }
            _ => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{bytes, record};
    use mmx_consensus::Value;
    fn proof(key: u8) -> Record {
        record(
            "mmx.ProofOfSpaceOG",
            [("farmer_key".into(), bytes([key; 33]))],
        )
    }
    fn response(height: u32, key: u8) -> Record {
        record(
            "mmx.ProofResponse",
            [
                ("vdf_height".into(), Value::Uint(height.into())),
                ("proof".into(), Value::Record(proof(key))),
            ],
        )
    }
    fn block(height: u32, key: u8) -> Record {
        record(
            "mmx.Block",
            [
                ("vdf_height".into(), Value::Uint(height.into())),
                ("height".into(), Value::Uint(height.into())),
                (
                    "proof".into(),
                    Value::Sequence(vec![Value::Record(proof(key))]),
                ),
            ],
        )
    }
    fn vdf(height: u32, key: u8, segments: usize) -> Record {
        record(
            "mmx.ProofOfTime",
            [
                ("vdf_height".into(), Value::Uint(height.into())),
                ("timelord_key".into(), bytes([key; 33])),
                (
                    "segments".into(),
                    Value::Sequence(vec![bytes([0; 32]); segments]),
                ),
            ],
        )
    }
    #[test]
    fn farmer_permissions_bind_key_and_height_and_blocks_never_earn_credit() {
        let mut credits = Credits::default();
        credits.verified(&block(9, 1)).unwrap();
        assert!(!credits.consume(&block(9, 1), 65536).unwrap());
        credits.verified(&response(10, 1)).unwrap();
        assert!(!credits.consume(&block(11, 1), 65536).unwrap());
        assert!(!credits.consume(&block(10, 2), 65536).unwrap());
        assert!(credits.consume(&block(10, 1), 65536).unwrap());
        assert!(!credits.consume(&block(10, 1), 65536).unwrap());
        credits.verified(&response(10, 1)).unwrap();
        credits.verified(&response(11, 2)).unwrap();
        credits.verified(&vdf(10, 7, 1)).unwrap();
        credits.verified(&block(11, 1)).unwrap();
        assert!(!credits.consume(&block(10, 1), 65536).unwrap());
        assert!(credits.consume(&block(11, 2), 65536).unwrap());
        assert_eq!(credits.farmer_count, 0);
    }
    #[test]
    fn timelord_permissions_require_work_and_preserve_segment_and_replay_boundaries() {
        let mut credits = Credits::default();
        credits.verified(&vdf(20, 7, 1)).unwrap();
        assert!(!credits.consume(&vdf(21, 8, 1), 2).unwrap());
        assert!(!credits.consume(&vdf(21, 7, 2), 2).unwrap());
        assert!(!credits.consume(&vdf(21, 7, 1), 0).unwrap());
        assert!(credits.consume(&vdf(21, 7, 1), 2).unwrap());
        assert!(!credits.consume(&vdf(22, 7, 1), 2).unwrap());
        credits.verified(&vdf(21, 7, 1)).unwrap();
        assert!(credits.consume(&vdf(22, 7, 1), 2).unwrap());
        assert!(!credits.too_old(&vdf(20, 7, 1), 1).unwrap());
        assert!(credits.too_old(&vdf(19, 7, 1), 1).unwrap());
        credits.verified(&block(20, 1)).unwrap();
        assert!(!credits.too_old(&block(19, 1), 1).unwrap());
        assert!(credits.too_old(&block(18, 1), 1).unwrap());
        assert!(!credits.too_old(&block(u32::MAX, 1), 1).unwrap());
    }
}
