//! CPU-only header checks over owned, history-resolved contexts. Database and
//! voting access remain on the caller; workers never publish ledger state.
use super::{invalid, ChainResult};
use mmx_consensus::{
    proof::ProofResult,
    transitions::{self, ProofContext, VerifiedHeader},
    Hash, Record,
};
use mmx_types::mmx;
use std::{collections::BTreeMap, thread};

pub(super) struct Header<'a> {
    pub hash: Hash,
    pub record: &'a Record,
    pub context: ProofContext,
}
pub(super) fn verify_headers(
    jobs: &[Header<'_>],
    params: &mmx::ChainParams,
) -> ChainResult<BTreeMap<Hash, ProofResult<VerifiedHeader>>> {
    let workers = thread::available_parallelism().map_or(1, |n| n.get());
    verify_with_workers(jobs, params, workers)
}
fn verify_with_workers(
    jobs: &[Header<'_>],
    params: &mmx::ChainParams,
    workers: usize,
) -> ChainResult<BTreeMap<Hash, ProofResult<VerifiedHeader>>> {
    let check = |job: &Header<'_>| {
        (
            job.hash,
            transitions::verify_evidence(job.record, params, &job.context),
        )
    };
    let workers = workers.clamp(1, 8).min(jobs.len());
    if workers <= 1 {
        return Ok(jobs.iter().map(check).collect());
    }
    thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(workers))
            .map(|chunk| scope.spawn(move || chunk.iter().map(check).collect::<Vec<_>>()))
            .collect();
        let mut results = BTreeMap::new();
        let mut panicked = false;
        for handle in handles {
            match handle.join() {
                Ok(batch) => results.extend(batch),
                Err(_) => panicked = true,
            }
        }
        if panicked {
            return Err(invalid("header proof worker panicked"));
        }
        Ok(results)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{genesis::Assets, ChainState};
    use mmx_consensus::Value;
    use mmx_types::VnxType;
    use vnx::serde_json::{self, Value as Json};

    #[test]
    fn parallel_work_preserves_each_valid_invalid_and_context_bound_result() {
        let rows: Vec<Json> = include_str!("../../tests/fixtures/chain.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let root = rows.iter().find(|r| r["name"] == "synthetic").unwrap();
        let decode = |v: &Json| {
            Record::decode(&vnx::util::from_hex_string(v.as_str().unwrap()).unwrap()).unwrap()
        };
        let params = mmx::ChainParams::decode(
            &vnx::util::from_hex_string(root["params"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        let assets = Assets {
            binaries: root["binaries"]
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
        let mut chain = ChainState::memory(params.clone());
        chain.initialize(&assets).unwrap();
        let row = rows.iter().find(|r| r["kind"] == "chain").unwrap();
        let original = decode(&row["steps"][0]["block"]);
        let context = ProofContext::resolve(&original, &params, &chain).unwrap();
        let records: Vec<_> = (0..19)
            .map(|i| {
                let mut record = original.clone();
                if i % 4 == 1 {
                    if let Value::Sequence(proofs) = record.fields.get_mut("proof").unwrap() {
                        if let Value::Record(proof) = &mut proofs[0] {
                            proof
                                .fields
                                .insert("difficulty".into(), Value::Uint(context.space_diff + 1));
                        }
                    }
                } else if i % 4 == 2 {
                    record.fields.insert("vdf_iters".into(), Value::Uint(0));
                }
                record
            })
            .collect();
        let jobs: Vec<_> = records
            .iter()
            .enumerate()
            .map(|(i, record)| {
                let mut context = context.clone();
                if i % 4 == 3 {
                    context.challenge[0] ^= 1;
                }
                Header {
                    hash: [i as u8; 32],
                    record,
                    context,
                }
            })
            .collect();
        let serial = verify_with_workers(&jobs, &params, 1).unwrap();
        let parallel = verify_with_workers(&jobs, &params, 8).unwrap();
        assert!(verify_headers(&[], &params).unwrap().is_empty());
        assert_eq!(parallel.len(), jobs.len());
        for (i, job) in jobs.iter().enumerate() {
            let actual = &parallel[&job.hash];
            let expected = &serial[&job.hash];
            assert_eq!(actual.is_ok(), i % 4 == 0);
            match (actual, expected) {
                (Ok(a), Ok(b)) => {
                    assert!(a.matches(job.record, &params, &job.context));
                    assert!(b.matches(job.record, &params, &job.context));
                    let mut other = job.context.clone();
                    other.parent.hash.0[0] ^= 1;
                    assert!(!a.matches(job.record, &params, &other));
                }
                (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string()),
                _ => panic!("parallel proof outcome differs from serial"),
            }
        }
    }
}
