use crate::{error, Result};
use mmx_consensus::{header, proof, response, vote, Record, Value};
use mmx_types::mmx;
use mmx_wallet::{Address, FarmerKey};
use rand::RngCore;
use std::{
    collections::BTreeMap,
    io::{self, Write},
    time::{Duration, Instant},
};

pub const MAX_REPORT_BYTES: usize = 256 * 1024;
const MAX_REPORT_MEMORY: usize = 32 * 1024 * 1024;

struct ReportSize(usize);
impl Write for ReportSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_REPORT_BYTES)
            .ok_or_else(|| io::Error::other("farm report exceeds 256 KiB"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn report_memory(info: &mmx::FarmInfo) -> Result<usize> {
    if info.harvester.as_ref().is_some_and(|n| n.len() >= 1024)
        || info.plot_dirs.len() > 1024
        || info.plot_dirs.iter().any(|p| p.len() > 4096)
        || info.plot_count.len() > 64
        || info.harvester_bytes.len() > 1024
        || info.pool_info.len() > 1024
        || info.pool_stats.len() > 1024
    {
        return Err(error("farm report field limit exceeded"));
    }
    let mut size = ReportSize(0);
    vnx::serde_json::to_writer(&mut size, info)?;
    // Charge string/vector/map overhead conservatively as well as encoded
    // bytes. Tiny entries can occupy much more heap than their JSON spelling.
    Ok(size.0 * 16 + std::mem::size_of::<mmx::FarmInfo>() + 128)
}

/// Holds only farmer identities. Spending keys remain in the wallet process.
pub struct Farmer {
    keys: BTreeMap<[u8; 33], FarmerKey>,
    pub reward_address: Option<Address>,
    reports: BTreeMap<[u8; 32], (Instant, mmx::FarmInfo, usize)>,
    report_bytes: usize,
    pub harvester_timeout: Duration,
    pub partial_difficulties: BTreeMap<Address, u64>,
}
impl Farmer {
    pub fn new(keys: impl IntoIterator<Item = FarmerKey>, reward_address: Option<Address>) -> Self {
        Self {
            keys: keys.into_iter().map(|k| (k.public_key(), k)).collect(),
            reward_address: reward_address.filter(|a| *a != Address::default()),
            reports: BTreeMap::new(),
            report_bytes: 0,
            harvester_timeout: Duration::from_secs(60),
            partial_difficulties: BTreeMap::new(),
        }
    }
    pub fn public_keys(&self) -> Vec<[u8; 33]> {
        self.keys.keys().copied().collect()
    }
    pub fn add_key(&mut self, key: FarmerKey) {
        self.keys.insert(key.public_key(), key);
    }
    fn sign(&self, public: &[u8; 33], hash: &[u8; 32]) -> Result<[u8; 64]> {
        Ok(self
            .keys
            .get(public)
            .ok_or_else(|| error("unknown farmer key"))?
            .sign(hash)?)
    }
    pub fn sign_response(&self, record: &Record) -> Result<Record> {
        if !response::is_valid(record)? {
            return Err(error("invalid proof response"));
        }
        let p = record.field("proof")?.record()?;
        proof::validate_plot_id(p)?;
        let key = p.field("farmer_key")?.bytes()?.try_into()?;
        let mut out = record.clone();
        let signature = self.sign(key, record.field("hash")?.bytes()?.try_into()?)?;
        out.fields
            .insert("farmer_sig".into(), Value::Bytes(signature.to_vec()));
        out.fields.insert(
            "content_hash".into(),
            Value::Bytes(response::content_hash(&out)?.to_vec()),
        );
        Ok(out)
    }
    pub fn sign_block(&self, block: &Record, params: &mmx::ChainParams) -> Result<Record> {
        if !matches!(block.name.as_str(), "mmx.BlockHeader" | "mmx.Block") {
            return Err(error("expected block header"));
        }
        let first = block
            .field("proof")?
            .seq()?
            .first()
            .ok_or_else(|| error("missing proof"))?
            .record()?;
        let key = first.field("farmer_key")?.bytes()?.try_into()?;
        let mut out = block.clone();
        out.fields
            .insert("nonce".into(), Value::Uint(rand::rngs::OsRng.next_u64()));
        if out.field("reward_addr")?.optional()?.is_none() {
            out.fields
                .insert("reward_addr".into(), optional_address(self.reward_address));
        }
        if out.field("reward_contract")?.optional()?.is_some() {
            out.fields.insert(
                "reward_account".into(),
                optional_address(self.reward_address),
            );
        }
        let hash = header::calc_hash(&out, params)?;
        out.fields
            .insert("hash".into(), Value::Bytes(hash.to_vec()));
        out.fields.insert(
            "farmer_sig".into(),
            Value::Optional(Some(Box::new(Value::Bytes(
                self.sign(key, &hash)?.to_vec(),
            )))),
        );
        out.fields.insert(
            "content_hash".into(),
            Value::Bytes(header::calc_content_hash(&out)?.to_vec()),
        );
        Ok(out)
    }
    pub fn sign_vote(&self, record: &Record) -> Result<[u8; 64]> {
        if record.name != "mmx.ValidatorVote" {
            return Err(error("expected validator vote"));
        }
        let key = record.field("farmer_key")?.bytes()?.try_into()?;
        self.sign(
            key,
            &vote::signing_hash(record.field("hash")?.bytes()?.try_into()?, key),
        )
    }
    pub fn report(&mut self, info: mmx::FarmInfo) -> Result<()> {
        use sha2::{Digest, Sha256};
        let size = report_memory(&info)?;
        let id = match (&info.harvester_id, &info.harvester) {
            (Some(id), _) => id.0,
            (_, Some(name)) => Sha256::digest(name.as_bytes()).into(),
            _ => return Err(error("harvester report has no identity")),
        };
        self.reports.retain(|_, (time, _, bytes)| {
            if time.elapsed() <= self.harvester_timeout {
                return true;
            }
            self.report_bytes -= *bytes;
            false
        });
        if self.reports.len() >= 4096 && !self.reports.contains_key(&id) {
            return Err(error("too many harvesters"));
        }
        let previous = self.reports.get(&id).map_or(0, |(_, _, bytes)| *bytes);
        let bytes = self.report_bytes - previous + size;
        if bytes > MAX_REPORT_MEMORY {
            return Err(error("farm report memory budget exceeded"));
        }
        self.reports.insert(id, (Instant::now(), info, size));
        self.report_bytes = bytes;
        Ok(())
    }
    pub fn farm_info(&self) -> Result<mmx::FarmInfo> {
        let mut out = mmx::FarmInfo {
            reward_addr: self.reward_address.map(|a| a.0.into()),
            ..Default::default()
        };
        for (_, r, _) in self
            .reports
            .values()
            .filter(|(t, _, _)| t.elapsed() <= self.harvester_timeout)
        {
            out.total_bytes = out
                .total_bytes
                .checked_add(r.total_bytes)
                .ok_or_else(|| error("farm size overflow"))?;
            out.total_bytes_effective = out
                .total_bytes_effective
                .checked_add(r.total_bytes_effective)
                .ok_or_else(|| error("effective farm size overflow"))?;
            out.total_balance = out
                .total_balance
                .checked_add(r.total_balance)
                .ok_or_else(|| error("virtual balance overflow"))?;
            for (k, n) in &r.plot_count {
                let v = out.plot_count.entry(*k).or_default();
                *v = v
                    .checked_add(*n)
                    .ok_or_else(|| error("plot count overflow"))?;
            }
            let name = r.harvester.clone().unwrap_or_default();
            if r.harvester.is_some() {
                let v = out.harvester_bytes.entry(name.clone()).or_default();
                v.0 =
                    v.0.checked_add(r.total_bytes)
                        .ok_or_else(|| error("farm size overflow"))?;
                v.1 =
                    v.1.checked_add(r.total_bytes_effective)
                        .ok_or_else(|| error("farm size overflow"))?;
            }
            out.plot_dirs
                .extend(r.plot_dirs.iter().map(|p| format!("{name}:{p}")));
            for (address, info) in &r.pool_info {
                let count = out.pool_info.get(address).map_or(0, |p| p.plot_count);
                let mut info = info.clone();
                info.plot_count = info
                    .plot_count
                    .checked_add(count)
                    .ok_or_else(|| error("pool plot count overflow"))?;
                out.pool_info.insert(address.clone(), info);
            }
        }
        for (address, info) in &mut out.pool_info {
            info.partial_diff = *self
                .partial_difficulties
                .get(&Address(address.0))
                .unwrap_or(&0);
        }
        Ok(out)
    }
}
fn optional_address(address: Option<Address>) -> Value {
    Value::Optional(address.map(|a| Box::new(Value::Bytes(a.0.to_vec()))))
}

#[cfg(test)]
mod report_tests {
    use super::*;
    fn report(id: u8) -> mmx::FarmInfo {
        mmx::FarmInfo {
            harvester_id: Some([id; 32].into()),
            harvester: Some("test".into()),
            plot_dirs: vec!["x".repeat(4096); 32],
            ..Default::default()
        }
    }
    #[test]
    fn reports_bound_fields_retained_bytes_replacements_and_expiry() {
        let mut farmer = Farmer::new([], None);
        let mut bad = report(0);
        bad.plot_dirs = vec!["x".repeat(128 * 1024)];
        assert!(farmer.report(bad).is_err());
        assert!(farmer.reports.is_empty());
        for id in 0..=255u8 {
            if farmer.report(report(id)).is_err() {
                break;
            }
        }
        assert!(farmer.reports.len() < 256 && !farmer.reports.is_empty());
        assert!(farmer.report_bytes <= MAX_REPORT_MEMORY);
        let size = farmer.report_bytes;
        farmer.report(report(0)).unwrap();
        assert_eq!(farmer.report_bytes, size);
        let count = farmer.reports.len();
        let mut oversized = report(0);
        oversized.plot_dirs = vec!["x".repeat(4096); 128];
        assert!(farmer.report(oversized).is_err());
        assert_eq!(farmer.reports.len(), count);
        assert_eq!(farmer.report_bytes, size);
        for (time, _, _) in farmer.reports.values_mut() {
            *time = Instant::now() - Duration::from_secs(120);
        }
        farmer.report(report(255)).unwrap();
        assert_eq!(farmer.reports.len(), 1);
        assert_eq!(farmer.report_bytes, report_memory(&report(255)).unwrap());
    }
}
