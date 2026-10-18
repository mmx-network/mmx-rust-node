use crate::{error, plot::Prover, Result};
use mmx_consensus::{pos, proof, response, Record, Value};
use mmx_types::mmx;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

pub struct Harvester {
    pub name: String,
    pub directories: BTreeSet<PathBuf>,
    pub blacklist: BTreeSet<PathBuf>,
    pub max_recursion: usize,
    pub max_f1: usize,
    pub recursive: bool,
    pub farmer_keys: BTreeSet<[u8; 33]>,
    plots: BTreeMap<[u8; 32], Prover>,
    pub failures: Vec<(PathBuf, String)>,
    id: [u8; 32],
}
impl Harvester {
    pub fn new(name: String, farmer_keys: impl IntoIterator<Item = [u8; 33]>) -> Result<Self> {
        use rand::RngCore;
        if name.len() >= 1024 {
            return Err(error("harvester name must be shorter than 1024 bytes"));
        }
        let mut id = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut id);
        Ok(Self {
            name,
            directories: BTreeSet::new(),
            blacklist: BTreeSet::new(),
            max_recursion: 4,
            max_f1: 65536,
            recursive: true,
            farmer_keys: farmer_keys.into_iter().collect(),
            plots: BTreeMap::new(),
            failures: Vec::new(),
            id,
        })
    }
    pub fn plots(&self) -> &BTreeMap<[u8; 32], Prover> {
        &self.plots
    }
    fn files(&self, path: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
        if depth > self.max_recursion
            || self.blacklist.contains(path)
            || fs::symlink_metadata(path)?.file_type().is_symlink()
        {
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() && self.recursive {
                self.files(&path, depth + 1, out)?;
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "plot") {
                if out.len() >= 1_000_000 {
                    return Err(error("plot directory discovery limit exceeded"));
                }
                out.push(path);
            }
        }
        Ok(())
    }
    pub fn reload(&mut self) {
        let mut files = Vec::new();
        let mut failures = Vec::new();
        for path in &self.directories {
            if let Err(e) = self.files(path, 0, &mut files) {
                failures.push((path.clone(), e.to_string()));
            }
        }
        files.sort();
        files.dedup();
        let old: BTreeMap<_, _> = std::mem::take(&mut self.plots)
            .into_values()
            .map(|p| (p.path().to_path_buf(), p))
            .collect();
        let mut plots = BTreeMap::new();
        for path in files {
            let candidate = match old.get(&path).filter(|p| p.unchanged()) {
                Some(p) => Ok(p.clone()),
                None => Prover::open(&path),
            };
            match candidate {
                Ok(plot) => {
                    if !self.farmer_keys.contains(&plot.header.farmer_key.0) {
                        failures.push((path, "unknown farmer key".into()));
                        continue;
                    }
                    if let std::collections::btree_map::Entry::Vacant(entry) =
                        plots.entry(plot.header.plot_id.0)
                    {
                        entry.insert(plot);
                    } else {
                        failures.push((path, "duplicate plot id".into()));
                    }
                }
                Err(e) => failures.push((path, e.to_string())),
            }
        }
        self.plots = plots;
        self.failures = failures;
    }
    pub fn farm_info(&self) -> Result<mmx::FarmInfo> {
        let mut info = mmx::FarmInfo {
            harvester: Some(self.name.clone()),
            harvester_id: Some(self.id.into()),
            plot_dirs: self
                .directories
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            ..Default::default()
        };
        for plot in self.plots.values() {
            *info.plot_count.entry(plot.header.ksize as u8).or_default() += 1;
            info.total_bytes = info
                .total_bytes
                .checked_add(plot.length())
                .ok_or_else(|| error("farm size overflow"))?;
            // C++ effective size: (2*k+1) * 2^(k-1), irrespective of physical compression.
            let effective = (2 * plot.header.ksize as u64 + 1)
                .checked_shl((plot.header.ksize - 1) as u32)
                .ok_or_else(|| error("farm size overflow"))?;
            let effective = (u128::from(effective) * 2464 / 1000)
                .try_into()
                .map_err(|_| error("farm size overflow"))?;
            info.total_bytes_effective = info
                .total_bytes_effective
                .checked_add(effective)
                .ok_or_else(|| error("farm size overflow"))?;
        }
        Ok(info)
    }
    pub fn lookup(
        &self,
        request: &mmx::Challenge,
        params: &mmx::ChainParams,
    ) -> Result<Vec<Record>> {
        if request.difficulty == 0 || params.plot_filter > 30 {
            return Err(error("invalid challenge difficulty or filter"));
        }
        self.lookup_for(request, params, std::time::Duration::from_secs(10))
    }
    pub fn lookup_for(
        &self,
        request: &mmx::Challenge,
        params: &mmx::ChainParams,
        window: std::time::Duration,
    ) -> Result<Vec<Record>> {
        if request.difficulty == 0 || params.plot_filter > 30 {
            return Err(error("invalid challenge difficulty or filter"));
        }
        let hard_fork = request.vdf_height >= params.hardfork1_height;
        let started = Instant::now();
        let mut out = Vec::new();
        for plot in self.plots.values() {
            if started.elapsed() >= window {
                break;
            }
            let h = &plot.header;
            if h.ksize < params.min_ksize as i32
                || h.ksize > params.max_ksize as i32
                || !proof::check_plot_filter(&request.challenge.0, &h.plot_id.0, params.plot_filter)
            {
                continue;
            }
            let one = (|| -> Result<Vec<Record>> {
                let mut found = Vec::new();
                let local = proof::plot_challenge(&request.challenge.0, &h.plot_id.0);
                for mut quality in plot.qualities(&local, params.plot_filter, self.max_f1)? {
                    if started.elapsed() >= window {
                        break;
                    }
                    if hard_fork
                        && !pos::check_post_filter(&local, &quality.meta, params.post_filter)
                    {
                        continue;
                    }
                    if hard_fork && quality.proof.is_empty() {
                        quality = plot.full_proof(quality.index, self.max_f1)?;
                    }
                    let mut p = Record::from_generated(&mmx::ProofOfSpaceOG {
                        ksize: h.ksize as u8,
                        seed: h.seed.clone(),
                        plot_id: h.plot_id.clone(),
                        farmer_key: h.farmer_key.clone(),
                        challenge: request.challenge.clone(),
                        difficulty: request.difficulty,
                        proof_xs: quality.proof.clone(),
                        ..Default::default()
                    })?;
                    if let Some(contract) = &h.contract {
                        p.name = "mmx.ProofOfSpaceNFT".into();
                        p.fields
                            .insert("contract".into(), Value::Bytes(contract.0.to_vec()));
                    }
                    let target = if hard_fork {
                        let mut local_proof = p.clone();
                        local_proof
                            .fields
                            .insert("challenge".into(), Value::Bytes(local.to_vec()));
                        proof::calc_proof_hash(&local_proof)?
                    } else {
                        pos::calc_quality(&local, &quality.meta)
                    };
                    if !proof::check_threshold(
                        params,
                        h.ksize as u8,
                        &target,
                        request.difficulty,
                        hard_fork,
                    )? {
                        continue;
                    }
                    if quality.proof.is_empty() {
                        quality = plot.full_proof(quality.index, self.max_f1)?;
                        p.fields.insert(
                            "proof_xs".into(),
                            Value::Sequence(
                                quality
                                    .proof
                                    .into_iter()
                                    .map(|x| Value::Uint(x.into()))
                                    .collect(),
                            ),
                        );
                    }
                    p.fields.insert(
                        "score".into(),
                        Value::Uint(proof::calc_proof_hash(&p)?[0].into()),
                    );
                    let mut r = Record::from_generated(&mmx::ProofResponse {
                        vdf_height: request.vdf_height,
                        harvester: self.name.clone(),
                        lookup_time_ms: started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                        ..Default::default()
                    })?;
                    r.fields.insert("proof".into(), Value::Record(p));
                    r.fields
                        .insert("hash".into(), Value::Bytes(r.calc_hash(false, 0)?.to_vec()));
                    r.fields.insert(
                        "content_hash".into(),
                        Value::Bytes(response::content_hash(&r)?.to_vec()),
                    );
                    found.push(r);
                }
                Ok(found)
            })();
            match one {
                Ok(found) => out.extend(found),
                Err(e) => eprintln!("Plot {}: {e}", plot.path().display()),
            }
        }
        Ok(out)
    }
}

/// C++ harvesters prioritize the newest queued height and look up a challenge
/// only once. Bound the remembered identities so reconnects cannot grow RAM.
pub struct ChallengeQueue {
    pending: BTreeMap<u32, (Instant, mmx::Challenge)>,
    seen: BTreeSet<([u8; 32], u64)>,
    order: std::collections::VecDeque<([u8; 32], u64)>,
    timeout: std::time::Duration,
}
impl ChallengeQueue {
    pub fn new(timeout: std::time::Duration) -> Self {
        Self {
            pending: BTreeMap::new(),
            seen: BTreeSet::new(),
            order: std::collections::VecDeque::new(),
            timeout,
        }
    }
    pub fn push(&mut self, request: mmx::Challenge, age: std::time::Duration) -> bool {
        if age > self.timeout {
            return false;
        }
        let id = (request.challenge.0, request.difficulty);
        if !self.seen.insert(id) {
            return false;
        }
        self.order.push_back(id);
        if self.order.len() > 4096 {
            self.seen.remove(&self.order.pop_front().unwrap());
        }
        self.pending.insert(
            request.vdf_height,
            (
                Instant::now().checked_sub(age).unwrap_or_else(Instant::now),
                request,
            ),
        );
        while self.pending.len() > 128 {
            self.pending.pop_first();
        }
        true
    }
    pub fn pop(&mut self) -> Option<mmx::Challenge> {
        self.pending.retain(|_, (t, _)| t.elapsed() <= self.timeout);
        self.pending.pop_last().map(|(_, (_, r))| r)
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.seen.clear();
        self.order.clear();
    }
}
