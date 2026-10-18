use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use mmx_consensus::{header, proof, Record, Value as N};
use mmx_execution::chain::{forks::ForkChain, genesis::Assets};
use mmx_types::{mmx, VnxType};
use vnx::serde_json::{self, Value};
fn decode(value: &Value) -> Record {
    Record::decode(&vnx::util::from_hex_string(value.as_str().unwrap()).unwrap()).unwrap()
}
#[test]
fn disconnected_height_gap_never_enters_live_or_historical_proof_ranking() {
    let rows: Vec<Value> = include_str!("fixtures/chain.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let g = rows.iter().find(|row| row["name"] == "synthetic").unwrap();
    let params = mmx::ChainParams::decode(
        &vnx::util::from_hex_string(g["params"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let assets = Assets {
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
    };
    let first = &rows.iter().find(|row| row["kind"] == "chain").unwrap()["steps"][0];
    let original = decode(&first["block"]);
    let vdf = decode(&first["vdf"]);
    let mut gap = original.clone();
    gap.fields.insert("height".into(), N::Uint(2));
    let id = header::calc_hash(&gap, &params).unwrap();
    let mut key = [0; 32];
    key[31] = 1;
    let sig: Signature = SigningKey::from_slice(&key)
        .unwrap()
        .sign_prehash(&id)
        .unwrap();
    gap.fields.insert("hash".into(), N::Bytes(id.to_vec()));
    gap.fields.insert(
        "farmer_sig".into(),
        N::Optional(Some(Box::new(N::Bytes(sig.to_bytes().to_vec())))),
    );
    gap.fields.insert(
        "content_hash".into(),
        N::Bytes(header::calc_content_hash(&gap).unwrap().to_vec()),
    );
    let challenge: [u8; 32] = gap.field("proof").unwrap().seq().unwrap()[0]
        .record()
        .unwrap()
        .field("challenge")
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    for live in [false, true] {
        let mut chain = ForkChain::memory(params.clone());
        chain.initialize(&assets).unwrap();
        let root = chain.selected().hash_at(0).unwrap().unwrap();
        chain.admit(&gap, std::slice::from_ref(&vdf)).unwrap();
        let result = if live {
            chain.update_at(root, None, 0)
        } else {
            chain.update_sync_at(root, None, 0)
        }
        .unwrap();
        assert!(result.best.is_none());
        assert!(result.waiting.contains(&id));
        assert_eq!(chain.best_proof(&challenge), None);
        chain.admit(&original, std::slice::from_ref(&vdf)).unwrap();
        if live {
            chain.update_at(root, None, 0)
        } else {
            chain.update_sync_at(root, None, 0)
        }
        .unwrap();
        assert_eq!(chain.selected().tip().unwrap().unwrap().height, 1);
        assert_eq!(
            chain.best_proof(&challenge),
            Some(
                proof::calc_proof_hash(
                    original.field("proof").unwrap().seq().unwrap()[0]
                        .record()
                        .unwrap()
                )
                .unwrap()
            )
        );
    }
}
