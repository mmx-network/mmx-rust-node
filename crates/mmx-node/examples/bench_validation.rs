//! Profile static validation of cached bodies served by a running local node.
//! This does not run synchronization or alter the node's selected chain.
use mmx_consensus::{Record, StreamEncoder};
use mmx_execution::{block, chain::network::Network};
use mmx_node::{peer::Client, Node, Result};
use mmx_types::{mmx, VnxType};
use std::{
    collections::BTreeMap,
    hint::black_box,
    sync::{Arc, Mutex},
    time::Instant,
};

struct History(BTreeMap<mmx_consensus::Hash, Arc<mmx::BlockHeader>>);
impl mmx_consensus::transitions::history::HeaderSource for History {
    fn get_header(
        &self,
        hash: &mmx_consensus::Hash,
    ) -> mmx_consensus::proof::ProofResult<Option<mmx::BlockHeader>> {
        Ok(self.0.get(hash).map(|header| header.as_ref().clone()))
    }
    fn get_shared_header(
        &self,
        hash: &mmx_consensus::Hash,
    ) -> mmx_consensus::proof::ProofResult<Option<Arc<mmx::BlockHeader>>> {
        Ok(self.0.get(hash).cloned())
    }
}
fn header(record: &Record) -> Result<mmx::BlockHeader> {
    let header = Record {
        name: "mmx.BlockHeader".into(),
        fields: record
            .fields
            .iter()
            .filter(|(name, _)| name.as_str() != "tx_list")
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    };
    Ok(mmx::BlockHeader::decode(&header.encode()?)?)
}

fn measure(
    name: &str,
    records: &[Record],
    rounds: u32,
    mut action: impl FnMut(&Record) -> Result<()>,
) -> Result<()> {
    let began = Instant::now();
    for _ in 0..rounds {
        for record in records {
            action(black_box(record))?;
        }
    }
    let elapsed = began.elapsed().as_secs_f64();
    let calls = records.len() as u64 * u64::from(rounds);
    println!(
        "{}",
        vnx::serde_json::json!({
            "operation": name, "calls": calls, "seconds": elapsed,
            "microseconds_per_call": elapsed * 1_000_000.0 / calls as f64,
        })
    );
    Ok(())
}

fn main() -> Result<()> {
    let mut root = "../mmx-node".to_owned();
    let mut peer = "127.0.0.1:12337".to_owned();
    let mut count = 128u32;
    let mut rounds = 20u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("missing benchmark argument")?;
        match arg.as_str() {
            "--mmx-root" => root = value,
            "--peer" => peer = value,
            "--blocks" => count = value.parse()?,
            "--rounds" => rounds = value.parse()?,
            _ => return Err(format!("unknown benchmark argument {arg}").into()),
        }
    }
    if count == 0 || count > 128 || rounds == 0 {
        return Err("require 1..=128 blocks and positive rounds".into());
    }
    let network = Network::load(root, "mainnet")?;
    let directory = tempfile::tempdir()?;
    let local = Arc::new(Mutex::new(Node::open(directory.path(), &network)?));
    let mut client = Client::connect(&peer, local)?;
    let tip = client.height()?;
    if tip == 0 {
        return Err("local node has no non-genesis blocks to profile".into());
    }
    let start = tip.saturating_sub(count - 1).max(1);
    let history_start = start
        .saturating_sub(1000 + network.params.commit_delay)
        .max(1);
    let mut hashes = Vec::new();
    let mut height = history_start;
    while height <= tip {
        let count = (tip - height + 1).min(128);
        for pair in client.hash_pairs(height, count)? {
            hashes.push(pair.ok_or("local node omitted an archived hash")?.block);
        }
        height = height
            .checked_add(count)
            .ok_or("benchmark height overflow")?;
    }
    let mut records = Vec::new();
    for hashes in hashes.chunks(mmx_node::peer::MAX_BODY_REQUESTS) {
        for record in client.blocks_by_hash(hashes)? {
            records.push(record.ok_or("local node omitted an archived body")?);
        }
    }
    let sample = records.split_off((start - history_start) as usize);
    let ancestors = records;
    let records = sample;
    println!(
        "{}",
        vnx::serde_json::json!({"from_height": start, "to_height": tip, "blocks": records.len()})
    );
    measure("header_validity", &records, rounds, |record| {
        if !mmx_consensus::header::is_valid(record, &network.params)? {
            return Err("invalid cached header".into());
        }
        Ok(())
    })?;
    measure("farmer_authentication_cold", &records, 1, |record| {
        Ok(mmx_consensus::header::authenticate(record)?)
    })?;
    measure("farmer_authentication_warm", &records, rounds, |record| {
        Ok(mmx_consensus::header::authenticate(record)?)
    })?;
    measure("full_prevalidation", &records, rounds, |record| {
        Ok(block::prevalidate(record, &network.params)?)
    })?;
    measure("typed_header_conversion", &records, rounds, |record| {
        black_box(header(record)?);
        Ok(())
    })?;
    let mut history = History(BTreeMap::new());
    for record in ancestors.iter().chain(&records) {
        let header = header(record)?;
        history.0.insert(header.hash.0, Arc::new(header));
    }
    measure("validator_lookup", &records, rounds, |record| {
        let hash: mmx_consensus::Hash = record.field("hash")?.bytes()?.try_into()?;
        black_box(mmx_consensus::vote::validators(
            &history,
            &network.params,
            &history.0[&hash],
            1000,
        )?);
        Ok(())
    })?;
    if records.len() > 64 {
        let tail = &records[64..];
        measure("proof_context_resolution", tail, rounds, |record| {
            black_box(mmx_consensus::transitions::ProofContext::resolve(
                record,
                &network.params,
                &history,
            )?);
            Ok(())
        })?;
        measure("cpu_space_checks", tail, 1, |record| {
            let context = mmx_consensus::transitions::ProofContext::resolve(
                record,
                &network.params,
                &history,
            )?;
            black_box(mmx_consensus::transitions::verify_evidence(
                record,
                &network.params,
                &context,
            )?);
            Ok(())
        })?;
    }
    measure("standalone_wire_encoding", &records, rounds, |record| {
        black_box(record.encode()?);
        Ok(())
    })?;
    let mut encoder = StreamEncoder::default();
    measure("continuing_wire_encoding", &records, rounds, |record| {
        black_box(encoder.encode(record)?);
        Ok(())
    })?;
    Ok(())
}
