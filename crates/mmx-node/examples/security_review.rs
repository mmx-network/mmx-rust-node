//! Bounded review probes using public C++ test fixtures and disposable state.
//! Prints observations; these are diagnostics, not assertions of desired policy.
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use mmx_consensus::{header, proof, response, Record, Value as N, Variant};
use mmx_execution::{
    block,
    chain::{forks::ForkChain, genesis::Assets, network::Network},
};
use mmx_node::{
    node::{hash, record},
    peer, Node, Result,
};
use mmx_types::{mmx, VnxType};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use vnx::serde_json::{self, Value};

fn decode(value: &Value) -> Record {
    Record::decode(&vnx::util::from_hex_string(value.as_str().unwrap()).unwrap()).unwrap()
}
fn fixture() -> (Network, Record, Record) {
    let rows: Vec<Value> = include_str!("../../mmx-execution/tests/fixtures/chain.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let g = rows.iter().find(|row| row["name"] == "synthetic").unwrap();
    let network = Network {
        params: mmx::ChainParams::decode(
            &vnx::util::from_hex_string(g["params"].as_str().unwrap()).unwrap(),
        )
        .unwrap(),
        assets: Assets {
            binaries: g["binaries"]
                .as_array()
                .unwrap()
                .iter()
                .map(decode)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
            testnet_rewards: None,
            project_relay: None,
        },
    };
    let first =
        decode(&rows.iter().find(|row| row["kind"] == "chain").unwrap()["steps"][0]["block"]);
    let vdf = decode(&rows.iter().find(|row| row["kind"] == "chain").unwrap()["steps"][0]["vdf"]);
    (network, first, vdf)
}
fn signer() -> SigningKey {
    let mut scalar = [0; 32];
    scalar[31] = 1;
    SigningKey::from_slice(&scalar).unwrap()
}
fn resign(block: &mut Record, params: &mmx::ChainParams) -> Result<()> {
    let id = header::calc_hash(block, params)?;
    let sig: Signature = signer().sign_prehash(&id)?;
    block.fields.insert("hash".into(), N::Bytes(id.to_vec()));
    block.fields.insert(
        "farmer_sig".into(),
        N::Optional(Some(Box::new(N::Bytes(sig.to_bytes().to_vec())))),
    );
    block.fields.insert(
        "content_hash".into(),
        N::Bytes(header::calc_content_hash(block)?.to_vec()),
    );
    Ok(())
}
fn main() -> Result<()> {
    // At most ten thousand null values (~1 MiB); never exercise the huge limit.
    for count in [1_u32, 10_000] {
        let bytes = [
            2_u16.to_le_bytes().as_slice(),
            &12_u16.to_le_bytes(),
            &0_u16.to_le_bytes(),
            &count.to_le_bytes(),
        ]
        .concat();
        let value = Variant::decode(&bytes)?;
        let expanded = value.value().seq()?.len() * std::mem::size_of::<N>();
        let mut data = Record::from_generated(&mmx::contract::Data::default())?;
        data.fields.insert("value".into(), N::Variant(value));
        let wire = data.encode()?;
        let decoded = peer::decode_frame(&peer::encode_frame(&data)?)?;
        println!("REVIEW decoder count={count} dynamic_bytes={} record_bytes={} expanded_value_bytes={expanded} decoded={}", bytes.len(), wire.len(), decoded == data);
    }
    let (network, first, vdf) = fixture();
    let mut disconnected = first.clone();
    disconnected.fields.insert("height".into(), N::Uint(2));
    resign(&mut disconnected, &network.params)?;
    block::prevalidate(&disconnected, &network.params)?;
    let challenge = hash(
        disconnected.field("proof")?.seq()?[0].record()?,
        "challenge",
    )?;
    let expected = proof::calc_proof_hash(disconnected.field("proof")?.seq()?[0].record()?)?;
    for live in [false, true] {
        let mut chain = ForkChain::memory(network.params.clone());
        chain.initialize(&network.assets)?;
        let root = chain.selected().hash_at(0)?.unwrap();
        chain.admit(&disconnected, std::slice::from_ref(&vdf))?;
        let now = vnx::util::get_wall_time_millis();
        let result = if live {
            chain.update_at(root, None, now)?
        } else {
            chain.update_sync_at(root, None, now)?
        };
        println!("REVIEW disconnected live={live} best_selected={} proof_rank_registered={} invalid={} selected_height={}", result.best.is_some(), chain.best_proof(&challenge) == Some(expected), result.invalid.len(), chain.selected().tip()?.unwrap().height);
    }

    let dir = tempfile::tempdir()?;
    let mut disk = ForkChain::open_native(dir.path(), network.params.clone())?;
    disk.initialize(&network.assets)?;
    let mut orphan = first.clone();
    orphan.fields.insert("prev".into(), N::Bytes(vec![222; 32]));
    orphan.fields.insert("tx_list".into(), N::Sequence(vec![]));
    for key in [
        "static_cost",
        "total_cost",
        "tx_fees",
        "tx_count",
        "reward_amount",
    ] {
        orphan.fields.insert(key.into(), N::Uint(0));
    }
    orphan.fields.insert(
        "tx_hash".into(),
        N::Bytes(block::transaction_root(&[]).to_vec()),
    );
    let mut p = orphan.field("proof")?.seq()?[0].record()?.clone();
    p.fields.insert("proof_xs".into(), N::Sequence(vec![]));
    orphan
        .fields
        .insert("proof".into(), N::Sequence(vec![N::Record(p)]));
    let mut retained = 0;
    for nonce in 1..=4 {
        orphan.fields.insert("nonce".into(), N::Uint(nonce));
        resign(&mut orphan, &network.params)?;
        disk.admit(&orphan, &[])?;
        retained += usize::from(disk.block(&hash(&orphan, "hash")?)?.is_some());
    }
    println!(
        "REVIEW unauthenticated_space_work_orphans retained={retained} selected_height={}",
        disk.selected().tip()?.unwrap().height
    );

    let mut chain = ForkChain::memory(network.params.clone());
    chain.initialize(&network.assets)?;
    let node = Arc::new(Mutex::new(Node::from_chain(chain, signer())?));
    let rows = include_str!("../../mmx-consensus/tests/fixtures/responses.jsonl");
    let original: Value = rows
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["name"] == "extensions1_0")
        .unwrap();
    let mut future = decode(&original["wire"]);
    future
        .fields
        .insert("vdf_height".into(), N::Uint(u32::MAX.into()));
    future
        .fields
        .insert("farmer_sig".into(), N::Bytes(vec![0; 64]));
    future.fields.insert(
        "hash".into(),
        N::Bytes(future.calc_hash(false, 0)?.to_vec()),
    );
    future.fields.insert(
        "content_hash".into(),
        N::Bytes(response::content_hash(&future)?.to_vec()),
    );
    for _ in 0..32 {
        node.lock().unwrap().receive(&future)?;
    }
    node.lock().unwrap().tick()?;
    println!(
        "REVIEW replayed_future_unsigned_responses queued={}",
        node.lock().unwrap().pending_proofs()
    );

    let mut server = peer::Server::bind("127.0.0.1:0", node.clone())?;
    let mut connection = peer::Connection::connect(&server.address().to_string())?;
    connection.send(&record(
        "mmx.Request",
        [
            ("id".into(), N::Uint(1)),
            (
                "method".into(),
                N::Record(record("mmx.Node.get_params", [])),
            ),
        ],
    ))?;
    let reply = connection.receive(Duration::from_secs(2))?.unwrap();
    println!(
        "REVIEW unrestricted_peer_rpc result={}",
        reply.field("result")?.record()?.name
    );
    drop(connection);
    server.shutdown();
    Ok(())
}
