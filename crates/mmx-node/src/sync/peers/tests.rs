use super::*;
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use mmx_execution::chain::{forks::ForkChain, genesis::Assets, network::Network};
use mmx_types::{mmx, VnxType};
use vnx::serde_json::{self, Value as Json};

#[derive(Clone, Debug)]
enum Event {
    Hash(u8, u32, u32),
    Fetch(u8, Hash, Instant),
}
#[derive(Clone)]
struct Fake {
    id: u8,
    blocks: BTreeMap<u32, Record>,
    claims: BTreeMap<u32, BlockHash>,
    events: Arc<Mutex<Vec<Event>>>,
    fail_hash: bool,
    fail_first: Arc<AtomicBool>,
    hash_delay: Duration,
}
impl Fake {
    fn new(id: u8, blocks: &[Record], events: &Arc<Mutex<Vec<Event>>>) -> Self {
        Self {
            id,
            blocks: blocks
                .iter()
                .map(|b| (number(b, "height").unwrap(), b.clone()))
                .collect(),
            claims: BTreeMap::new(),
            events: events.clone(),
            fail_hash: false,
            fail_first: Arc::new(AtomicBool::new(false)),
            hash_delay: Duration::ZERO,
        }
    }
}
impl PeerSource for Fake {
    fn hashes(&mut self, start: u32, count: u32) -> Result<Vec<Option<BlockHash>>> {
        self.events
            .lock()
            .unwrap()
            .push(Event::Hash(self.id, start, count));
        if self.fail_hash {
            return Err(error("simulated hash timeout"));
        }
        thread::sleep(self.hash_delay);
        (start..start + count)
            .map(|h| {
                Ok(self.claims.get(&h).copied().or_else(|| {
                    self.blocks.get(&h).map(|b| BlockHash {
                        block: hash(b, "hash").unwrap(),
                        content: hash(b, "content_hash").unwrap(),
                    })
                }))
            })
            .collect()
    }
    fn fetch(&mut self, hashes: &[Hash]) -> Result<Vec<Option<Record>>> {
        assert!(hashes.len() <= 4);
        hashes
            .iter()
            .map(|id| {
                self.events
                    .lock()
                    .unwrap()
                    .push(Event::Fetch(self.id, *id, Instant::now()));
                if self.fail_first.swap(false, Ordering::Relaxed) {
                    return Ok(None);
                }
                Ok(self
                    .blocks
                    .values()
                    .find(|b| hash(b, "hash").unwrap() == *id)
                    .cloned())
            })
            .collect()
    }
    fn synced_height(&mut self) -> Result<Option<u32>> {
        Ok(Some(self.blocks.keys().next_back().copied().unwrap_or(0)))
    }
}
fn decode(value: &Json) -> Record {
    Record::decode(&vnx::util::from_hex_string(value.as_str().unwrap()).unwrap()).unwrap()
}
fn fixture() -> (Network, Arc<Mutex<Node>>, Vec<Record>) {
    let rows: Vec<Json> = include_str!("../../../../mmx-execution/tests/fixtures/chain.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let g = rows.iter().find(|r| r["name"] == "synthetic").unwrap();
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
    let mut chain = ForkChain::memory(network.params.clone());
    chain.initialize(&network.assets).unwrap();
    let node = Arc::new(Mutex::new(
        Node::from_chain(chain, SigningKey::from_slice(&[7; 32]).unwrap()).unwrap(),
    ));
    let blocks = std::iter::once(decode(&g["block"]))
        .chain(
            rows.iter().find(|r| r["kind"] == "chain").unwrap()["steps"]
                .as_array()
                .unwrap()[..2]
                .iter()
                .map(|s| decode(&s["block"])),
        )
        .collect();
    (network, node, blocks)
}
fn peers(blocks: &[Record], events: &Arc<Mutex<Vec<Event>>>) -> PeerPool<Fake> {
    PeerPool::new(
        (1..=3)
            .map(|id| ([id; 32], Fake::new(id, blocks, events)))
            .collect(),
    )
}
fn counts(events: &Arc<Mutex<Vec<Event>>>) -> BTreeMap<Hash, usize> {
    let mut out = BTreeMap::new();
    for event in &*events.lock().unwrap() {
        if let Event::Fetch(_, hash, _) = event {
            *out.entry(*hash).or_default() += 1;
        }
    }
    out
}
#[test]
fn requires_three_distinct_hash_responses_before_any_body_download() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let mut duplicate_ids = PeerPool::new(
        (1..=3)
            .map(|id| ([1; 32], Fake::new(id, &blocks, &events)))
            .collect(),
    );
    assert_eq!(duplicate_ids.len(), 1);
    assert!(duplicate_ids
        .window(1, 1, &node, &AtomicBool::new(false))
        .is_err());
    assert!(node.lock().unwrap().sync_pending.is_empty());
    assert!(events.lock().unwrap().is_empty());
    let mut pool = peers(&blocks, &events);
    pool.peers[0].source.fail_hash = true;
    assert!(pool.window(1, 1, &node, &AtomicBool::new(false)).is_err());
    assert!(counts(&events).is_empty());
    assert_eq!(pool.len(), 2);
    assert!(node.lock().unwrap().sync_pending.is_empty());
    let pool = PeerPool::new(
        (1..=20)
            .map(|id| ([id; 32], Fake::new(id, &blocks, &events)))
            .collect(),
    );
    assert_eq!(pool.len(), MAX_PEERS);
    assert_eq!(
        pool.ids().into_iter().collect::<BTreeSet<_>>().len(),
        MAX_PEERS
    );
}
#[test]
fn matching_claims_download_once_and_restart_reuses_exact_archived_content() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let mut pool = peers(&blocks, &events);
    let stop = AtomicBool::new(false);
    let window = pool.window(1, 2, &node, &stop).unwrap();
    assert_eq!((window.body_requests, window.body_retries), (2, 0));
    assert_eq!(
        window.heights,
        vec![vec![blocks[1].clone()], vec![blocks[2].clone()]]
    );
    assert_eq!(
        counts(&events).values().copied().collect::<Vec<_>>(),
        [1, 1]
    );
    let voters: BTreeSet<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::Hash(id, 1, 2) => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(voters.len(), 3);
    publish(window, &node, &mut Progress::default()).unwrap();
    let mut fresh = peers(&blocks, &events);
    let reused = fresh.window(1, 2, &node, &stop).unwrap();
    assert_eq!((reused.body_requests, reused.body_retries), (0, 0));
    assert_eq!(
        counts(&events).values().copied().collect::<Vec<_>>(),
        [1, 1]
    );
    stop.store(true, Ordering::Relaxed);
    assert!(fresh.window(1, 2, &node, &stop).is_err());
}

#[test]
fn common_content_uses_the_fastest_claimant_without_reducing_hash_observers() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let mut pool = peers(&blocks, &events);
    for peer in &mut pool.peers {
        if peer.source.id != 1 {
            peer.source.hash_delay = Duration::from_millis(50);
        }
    }
    let window = pool.window(1, 2, &node, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        window.heights,
        vec![vec![blocks[1].clone()], vec![blocks[2].clone()]]
    );
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::Hash(_, 1, 2)))
            .count(),
        3
    );
    let providers: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Fetch(peer, _, _) => Some(*peer),
            _ => None,
        })
        .collect();
    assert_eq!(providers, [1, 1]);
}
fn resign(block: &mut Record, params: &mmx::ChainParams) {
    let id = mmx_consensus::header::calc_hash(block, params).unwrap();
    block
        .fields
        .insert("hash".into(), Value::Bytes(id.to_vec()));
    let mut secret = [0; 32];
    secret[31] = 1;
    let sig: Signature = SigningKey::from_slice(&secret)
        .unwrap()
        .sign_prehash(&id)
        .unwrap();
    block.fields.insert(
        "farmer_sig".into(),
        Value::Optional(Some(Box::new(Value::Bytes(sig.to_bytes().to_vec())))),
    );
    let content = mmx_consensus::header::calc_content_hash(block).unwrap();
    block
        .fields
        .insert("content_hash".into(), Value::Bytes(content.to_vec()));
    mmx_execution::block::prevalidate(block, params).unwrap();
}
#[test]
fn two_lying_peers_cannot_hide_the_third_peers_valid_chain() {
    let (network, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let mut sources = vec![([1; 32], Fake::new(1, &blocks, &events))];
    // Both liars have authentic, self-consistent block commitments and
    // signatures, but wrong rewards. Consensus must reject their ledger
    // transitions while preserving the third peer's correct candidates.
    for liar in 2..=3 {
        let mut bad = blocks.clone();
        for height in 1..bad.len() {
            let prev = hash(&bad[height - 1], "hash").unwrap();
            bad[height]
                .fields
                .insert("prev".into(), Value::Bytes(prev.to_vec()));
            let reward = bad[height].field("reward_amount").unwrap().uint().unwrap();
            bad[height].fields.insert(
                "reward_amount".into(),
                Value::Uint(reward + u64::from(liar)),
            );
            resign(&mut bad[height], &network.params);
        }
        sources.push(([liar; 32], Fake::new(liar, &bad, &events)));
    }
    let mut pool = PeerPool::new(sources);
    let progress = synchronize_peers(&mut pool, &node, &AtomicBool::new(false), |_| {}).unwrap();
    let local = node.lock().unwrap();
    assert_eq!(progress.selected, 2);
    assert!(local.is_synced());
    assert_eq!(
        local.chain.selected().tip().unwrap().unwrap().hash.0,
        hash(&blocks[2], "hash").unwrap()
    );
    let downloaded = counts(&events);
    assert_eq!(
        downloaded.len(),
        6,
        "all three distinct candidates at both heights"
    );
    assert!(
        downloaded.values().all(|&n| n == 1),
        "no duplicate downloads, including final tip probe"
    );
}
#[test]
fn a_second_provider_is_used_only_after_the_original_lease_times_out() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let fail_first = Arc::new(AtomicBool::new(true));
    let mut pool = peers(&blocks, &events);
    for peer in &mut pool.peers {
        peer.source.fail_first = fail_first.clone();
    }
    pool.timeout = Duration::from_millis(50);
    let window = pool.window(1, 1, &node, &AtomicBool::new(false)).unwrap();
    assert_eq!((window.body_requests, window.body_retries), (2, 1));
    assert_eq!(window.heights[0], vec![blocks[1].clone()]);
    let times: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            Event::Fetch(_, _, at) => Some(*at),
            _ => None,
        })
        .collect();
    assert_eq!(times.len(), 2);
    assert!(times[1].duration_since(times[0]) >= Duration::from_millis(45));
}
#[test]
fn forged_block_ids_cannot_poison_an_honest_content_hash_claim() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let mut pool = peers(&blocks, &events);
    pool.timeout = Duration::from_millis(5);
    for peer in &mut pool.peers[..2] {
        peer.source.claims.insert(
            1,
            BlockHash {
                block: [98; 32],
                content: hash(&blocks[1], "content_hash").unwrap(),
            },
        );
    }
    let window = pool.window(1, 1, &node, &AtomicBool::new(false)).unwrap();
    assert_eq!(window.heights[0], vec![blocks[1].clone()]);
    assert_eq!(counts(&events)[&hash(&blocks[1], "hash").unwrap()], 1);
}
#[test]
fn replaying_a_content_hash_at_another_height_does_not_download_it_again() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let mut pool = peers(&blocks, &events);
    for peer in &mut pool.peers[..2] {
        peer.source.claims.insert(
            2,
            BlockHash {
                block: hash(&blocks[1], "hash").unwrap(),
                content: hash(&blocks[1], "content_hash").unwrap(),
            },
        );
    }
    let window = pool.window(1, 2, &node, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        window.heights,
        vec![vec![blocks[1].clone()], vec![blocks[2].clone()]]
    );
    assert_eq!(
        counts(&events).values().copied().collect::<Vec<_>>(),
        [1, 1]
    );
}
#[test]
fn future_candidates_are_filtered_before_deciding_the_sync_peak() {
    let (network, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let mut extended = blocks.clone();
    let future =
        vnx::util::get_wall_time_millis() + network.params.block_interval_ms * 101 + 60_000;
    extended[2]
        .fields
        .insert("time_stamp".into(), Value::Int(future));
    resign(&mut extended[2], &network.params);
    let mut pool = PeerPool::new(vec![
        ([1; 32], Fake::new(1, &blocks[..2], &events)),
        ([2; 32], Fake::new(2, &extended, &events)),
        ([3; 32], Fake::new(3, &extended, &events)),
    ]);
    let progress = synchronize_peers(&mut pool, &node, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(progress.selected, 1);
    assert!(node.lock().unwrap().is_synced());
    assert_eq!(counts(&events)[&hash(&extended[2], "hash").unwrap()], 1);
}
#[test]
fn missing_ancestors_are_fetched_backwards_without_lowering_the_existing_root() {
    let (network, node, blocks) = fixture();
    {
        let mut local = node.lock().unwrap();
        local.chain.admit_blocks(&blocks[1..]).unwrap();
        local.sync_pos = 3;
        local.update().unwrap();
        local.root = 2;
    }
    let rows: Vec<Json> = include_str!("../../../../mmx-execution/tests/fixtures/chain.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let deep = rows.iter().find(|r| r["kind"] == "deep_fork").unwrap();
    let mut alternate = vec![
        blocks[0].clone(),
        decode(&deep["steps"][0]["block"]),
        decode(&deep["steps"][1]["block"]),
    ];
    // A signed orphan above our root triggers backwards discovery. Its
    // invalid transition must not grant permission to rebase fork policy.
    let mut child = alternate[2].clone();
    child.fields.insert("height".into(), Value::Uint(3));
    child.fields.insert(
        "prev".into(),
        Value::Bytes(hash(&alternate[2], "hash").unwrap().to_vec()),
    );
    resign(&mut child, &network.params);
    alternate.push(child);
    let events = Arc::new(Mutex::new(vec![]));
    let mut pool = peers(&alternate, &events);
    synchronize_peers(&mut pool, &node, &AtomicBool::new(false), |_| {}).unwrap();
    let local = node.lock().unwrap();
    assert_eq!(local.root, 2);
    assert_eq!(
        local.chain.selected().tip().unwrap().unwrap().hash.0,
        hash(&blocks[2], "hash").unwrap()
    );
    assert!(!local.is_synced());
    assert!(local.sync_pending.is_empty());
    for parent in &alternate[1..3] {
        assert!(local
            .chain
            .block(&hash(parent, "hash").unwrap())
            .unwrap()
            .is_some());
    }
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, Event::Hash(_, 1, 2))));
    assert!(counts(&events).values().all(|&n| n == 1));
}

#[test]
fn empty_height_replies_do_not_replay_unadvertised_future_orphans_after_rewind_or_restart() {
    let (network, _, blocks) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let node = Arc::new(Mutex::new(Node::open(directory.path(), &network).unwrap()));
    let events = Arc::new(Mutex::new(vec![]));
    let stop = AtomicBool::new(false);
    synchronize_peers(&mut peers(&blocks, &events), &node, &stop, |_| {}).unwrap();
    node.lock().unwrap().rollback_to(0).unwrap();
    synchronize_peers(&mut peers(&blocks[..2], &events), &node, &stop, |_| {}).unwrap();
    assert!(node.lock().unwrap().is_synced());
    for _ in 0..3 {
        let mut local = node.lock().unwrap();
        local.tick().unwrap();
        assert_eq!(local.chain.selected().tip().unwrap().unwrap().height, 1);
    }
    drop(node);
    let node = Arc::new(Mutex::new(Node::open(directory.path(), &network).unwrap()));
    {
        let mut local = node.lock().unwrap();
        local.tick().unwrap();
        assert_eq!(local.chain.selected().tip().unwrap().unwrap().height, 1);
        assert!(local
            .chain
            .block(&hash(&blocks[2], "hash").unwrap())
            .unwrap()
            .is_some());
    }
    synchronize_peers(&mut peers(&blocks, &events), &node, &stop, |_| {}).unwrap();
    assert_eq!(
        node.lock()
            .unwrap()
            .chain
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .height,
        2
    );
    assert!(counts(&events).values().all(|&n| n == 1));
}

#[test]
fn prefetch_starts_three_hash_queries_while_the_consumer_holds_the_ledger() {
    use std::sync::mpsc::{channel, Sender};
    struct Probe(Sender<()>);
    impl PeerSource for Probe {
        fn hashes(&mut self, _: u32, count: u32) -> Result<Vec<Option<BlockHash>>> {
            self.0.send(())?;
            Ok(vec![None; count as usize])
        }
        fn fetch(&mut self, _: &[Hash]) -> Result<Vec<Option<Record>>> {
            Err(error("empty hash replies must not download bodies"))
        }
        fn synced_height(&mut self) -> Result<Option<u32>> {
            Ok(Some(100))
        }
    }
    let (_, node, _) = fixture();
    let (started, received) = channel();
    let mut pool = PeerPool::new(
        (1..=3)
            .map(|id| ([id; 32], Probe(started.clone())))
            .collect(),
    );
    let stop = AtomicBool::new(false);
    prefetch::with_pool(&mut pool, &node, &stop, |producer| {
        let mut local = node.lock().unwrap();
        producer.prefetch(65, 4, &mut local)?;
        assert!(
            producer.prefetch(69, 4, &mut local).is_err(),
            "only one queued window"
        );
        assert_eq!(local.sync_pending, (65..69).collect());
        for _ in 0..3 {
            received.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        drop(local);
        let window = producer.window(65, 4)?;
        assert_eq!(window.heights.len(), 4);
        assert!(window.heights.iter().all(Vec::is_empty));
        assert_eq!(window.target, 100);
        Ok(())
    })
    .unwrap();
    assert!(node.lock().unwrap().sync_pending.is_empty());
    // Dropping an unconsumed queued window joins the network owner and clears
    // its pending-height guards before the same clients can be reused.
    let result: Result<()> = prefetch::with_pool(&mut pool, &node, &stop, |producer| {
        let mut local = node.lock().unwrap();
        producer.prefetch(69, 4, &mut local)?;
        for _ in 0..3 {
            received.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        drop(local);
        Err(error("consumer stopped early"))
    });
    assert!(result.is_err());
    assert!(node.lock().unwrap().sync_pending.is_empty());
}

#[test]
fn a_dropped_prefetched_reply_keeps_downloaded_content_for_the_next_sync_attempt() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let mut pool = peers(&blocks, &events);
    let stop = AtomicBool::new(false);
    let result: Result<()> = prefetch::with_pool(&mut pool, &node, &stop, |producer| {
        {
            let mut local = node.lock().unwrap();
            producer.prefetch(1, 2, &mut local)?;
        }
        producer.refresh()?; // Collect the window before the metadata reply.
        Err(error("consumer rejected a later operation"))
    });
    assert!(result.is_err());
    assert!(node.lock().unwrap().sync_pending.is_empty());
    assert_eq!(
        node.lock()
            .unwrap()
            .chain
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .height,
        0
    );
    prefetch::with_pool(&mut pool, &node, &stop, |producer| {
        let window = producer.window(1, 2)?;
        assert_eq!(
            window.heights,
            vec![vec![blocks[1].clone()], vec![blocks[2].clone()]]
        );
        Ok(())
    })
    .unwrap();
    assert_eq!(
        counts(&events).values().copied().collect::<Vec<_>>(),
        [1, 1]
    );
}

#[test]
fn unconsumed_prefetch_survives_restart_without_selecting_or_downloading_its_future_blocks() {
    let (network, _, blocks) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let node = Arc::new(Mutex::new(Node::open(directory.path(), &network).unwrap()));
    let events = Arc::new(Mutex::new(vec![]));
    let mut pool = peers(&blocks, &events);
    let stop = AtomicBool::new(false);
    let result: Result<()> = prefetch::with_pool(&mut pool, &node, &stop, |producer| {
        {
            let mut local = node.lock().unwrap();
            producer.prefetch(1, 2, &mut local)?;
        }
        producer.refresh()?;
        Err(error("stop before publication"))
    });
    assert!(result.is_err());
    assert_eq!(
        node.lock()
            .unwrap()
            .chain
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .height,
        0
    );
    assert!(node
        .lock()
        .unwrap()
        .chain
        .block(&hash(&blocks[2], "hash").unwrap())
        .unwrap()
        .is_some());
    drop(node);
    let node = Arc::new(Mutex::new(Node::open(directory.path(), &network).unwrap()));
    node.lock().unwrap().tick().unwrap();
    assert_eq!(
        node.lock()
            .unwrap()
            .chain
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .height,
        0
    );
    let progress = synchronize_peers(&mut peers(&blocks, &events), &node, &stop, |_| {}).unwrap();
    assert_eq!(progress.selected, 2);
    assert_eq!((progress.body_requests, progress.body_retries), (0, 0));
    assert!(counts(&events).values().all(|&n| n == 1));
}
