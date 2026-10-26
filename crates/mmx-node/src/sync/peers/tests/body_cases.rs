use super::*;
use std::sync::{
    atomic::AtomicUsize,
    mpsc::{channel, Receiver, Sender},
};

struct Source {
    fake: Fake,
    gate: Option<Receiver<()>>,
    poll_gate: Option<Receiver<()>>,
    entered: Sender<usize>,
    downloaded: Sender<usize>,
    outstanding: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    stream_first: bool,
    fail_after_first: bool,
}
impl PeerSource for Source {
    fn hashes(&mut self, start: u32, count: u32) -> Result<Vec<Option<BlockHash>>> {
        self.fake.hashes(start, count)
    }
    fn synced_height(&mut self) -> Result<Option<u32>> {
        self.fake.synced_height()
    }
    fn poll(&mut self) -> Result<()> {
        if let Some(gate) = self.poll_gate.take() {
            self.entered.send(0)?;
            gate.recv()?;
        }
        Ok(())
    }
    fn fetch(&mut self, hashes: &[Hash]) -> Result<Vec<Option<Record>>> {
        let mut values = vec![None; hashes.len()];
        self.fetch_each(hashes, &mut |index, value| {
            values[index] = value;
            Ok(())
        })?;
        Ok(values)
    }
    fn fetch_each(
        &mut self,
        hashes: &[Hash],
        emit: &mut dyn FnMut(usize, Option<Record>) -> Result<()>,
    ) -> Result<()> {
        let active = self.outstanding.fetch_add(hashes.len(), Ordering::SeqCst) + hashes.len();
        self.peak.fetch_max(active, Ordering::SeqCst);
        struct Pending(Arc<AtomicUsize>, usize);
        impl Drop for Pending {
            fn drop(&mut self) {
                self.0.fetch_sub(self.1, Ordering::SeqCst);
            }
        }
        let mut pending = Pending(self.outstanding.clone(), hashes.len());
        if !self.stream_first {
            if let Some(gate) = self.gate.take() {
                self.entered.send(hashes.len())?;
                gate.recv()?;
            }
        }
        let values = self.fake.fetch(hashes)?;
        for (index, value) in values.into_iter().enumerate() {
            self.outstanding.fetch_sub(1, Ordering::SeqCst);
            pending.1 -= 1;
            emit(index, value)?;
            if index == 0 && self.stream_first {
                if let Some(gate) = self.gate.take() {
                    self.entered.send(hashes.len())?;
                    gate.recv()?;
                }
                if self.fail_after_first {
                    return Err(error("provider failed after its first complete body"));
                }
            }
        }
        let _ = self.downloaded.send(hashes.len());
        Ok(())
    }
}
fn source(fake: Fake) -> (Source, Receiver<usize>, Receiver<usize>) {
    let (entered, began) = channel();
    let (downloaded, ready) = channel();
    (
        Source {
            fake,
            gate: None,
            poll_gate: None,
            entered,
            downloaded,
            outstanding: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
            stream_first: false,
            fail_after_first: false,
        },
        began,
        ready,
    )
}

#[test]
fn healthy_pipelines_continue_past_a_stalled_provider_with_at_most_32_outstanding_requests() {
    let (network, node, mut blocks) = fixture();
    for height in 3..=65 {
        let mut block = blocks[2].clone();
        block.fields.insert("height".into(), Value::Uint(height));
        resign(&mut block, &network.params);
        blocks.push(block);
    }
    let mut alternative = blocks[1].clone();
    let reward = alternative.field("reward_amount").unwrap().uint().unwrap();
    alternative
        .fields
        .insert("reward_amount".into(), Value::Uint(reward + 1));
    resign(&mut alternative, &network.params);
    let events = Arc::new(Mutex::new(vec![]));
    let (fast, _entered_fast, ready_fast) = source(Fake::new(1, &blocks, &events));
    let outstanding = fast.outstanding.clone();
    let peak = fast.peak.clone();
    let mut slow_blocks = blocks.clone();
    slow_blocks[1] = alternative;
    let (mut slow, entered_slow, _ready_slow) = source(Fake::new(2, &slow_blocks, &events));
    let (mut other, _entered_other, _ready_other) = source(Fake::new(3, &blocks, &events));
    for value in [&mut slow, &mut other] {
        value.fake.hash_delay = Duration::from_millis(20);
        value.outstanding = outstanding.clone();
        value.peak = peak.clone();
    }
    let (release, resumed) = channel();
    slow.gate = Some(resumed);
    let mut pool = PeerPool::new(vec![([1; 32], fast), ([2; 32], slow), ([3; 32], other)]);
    pool.timeout = Duration::from_secs(5);
    thread::scope(|scope| {
        let worker = scope.spawn(|| pool.window(1, 65, &node, &AtomicBool::new(false)));
        // Drop this sender before the scope joins, including on assertion failure.
        let release = release;
        assert_eq!(
            entered_slow.recv_timeout(Duration::from_secs(2)).unwrap(),
            1
        );
        let mut downloaded = 0;
        while downloaded < 65 {
            downloaded += ready_fast.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert_eq!(
            downloaded, 65,
            "all healthy bodies progress before the slow owner returns"
        );
        assert!(
            !worker.is_finished(),
            "the still-observed competing content retains its lease"
        );
        assert!(peak.load(Ordering::SeqCst) <= MAX_BODY_INFLIGHT);
        release.send(()).unwrap();
        let window = worker.join().unwrap().unwrap();
        assert_eq!(window.heights.len(), 65);
        assert_eq!(window.heights[0].len(), 2);
        assert_eq!((window.body_requests, window.body_retries), (66, 0));
    });
    assert!(counts(&events).values().all(|&count| count == 1));
}

#[test]
fn a_partial_batch_survives_a_stalled_tail_and_late_owner_replacement() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let (mut first, entered, _ready_first) = source(Fake::new(1, &blocks, &events));
    let (release, resumed) = channel();
    first.gate = Some(resumed);
    first.stream_first = true;
    first.fail_after_first = true;
    let mut sources = vec![([1; 32], first)];
    for id in 2..=3 {
        let (mut value, _, _) = source(Fake::new(id, &blocks, &events));
        value.fake.hash_delay = Duration::from_millis(15);
        sources.push(([id; 32], value));
    }
    let mut pool = PeerPool::new(sources);
    pool.timeout = Duration::from_millis(80);
    thread::scope(|scope| {
        let (finished, result) = channel();
        let worker_node = node.clone();
        let worker = scope.spawn(move || {
            let _ = finished.send(pool.window(1, 2, &worker_node, &AtomicBool::new(false)));
            pool
        });
        let release = release;
        assert_eq!(entered.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        let window = result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let mut pool = worker.join().unwrap();
        assert_eq!(
            window.heights,
            [vec![blocks[1].clone()], vec![blocks[2].clone()]]
        );
        assert_eq!((window.body_requests, window.body_retries), (3, 1));
        assert_eq!(counts(&events)[&hash(&blocks[1], "hash").unwrap()], 1);
        assert_eq!(counts(&events)[&hash(&blocks[2], "hash").unwrap()], 2);
        let old = pool.peers.iter().find(|peer| peer.id == [1; 32]).unwrap();
        assert!(
            old.body.is_some(),
            "return the window before joining the late owner"
        );
        pool.remove_failed();
        let (replacement, _, _) = source(Fake::new(4, &blocks, &events));
        assert!(pool.install_authenticated([1; 32], replacement));
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while pool.peers.iter().any(|peer| peer.replacement.is_some()) {
            assert!(Instant::now() < until);
            pool.drain_control().unwrap();
            thread::yield_now();
        }
        let peer = pool.peers.iter().find(|peer| peer.id == [1; 32]).unwrap();
        assert_eq!(peer.source.as_ref().unwrap().fake.id, 4);
        assert!(peer.active);
        assert!(!peer.qualified, "old data must not qualify the replacement");
    });
}

#[test]
fn cancellation_preserves_ready_bodies_and_retains_every_blocked_owner_until_cleanup() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let (mut first, entered, _ready) = source(Fake::new(1, &blocks, &events));
    let (release, resumed) = channel();
    first.gate = Some(resumed);
    first.stream_first = true;
    let mut sources = vec![([1; 32], first)];
    for id in 2..=3 {
        let (mut value, _, _) = source(Fake::new(id, &blocks, &events));
        value.fake.hash_delay = Duration::from_millis(20);
        sources.push(([id; 32], value));
    }
    let mut pool = PeerPool::new(sources);
    let stop = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        let (finished, result) = channel();
        let worker_node = node.clone();
        let worker_stop = stop.clone();
        let worker = scope.spawn(move || {
            let _ = finished.send(pool.window(1, 2, &worker_node, &worker_stop));
            pool
        });
        let release = release;
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        stop.store(true, Ordering::Relaxed);
        assert!(result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .err()
            .unwrap()
            .to_string()
            .contains("cancelled"));
        let pool = worker.join().unwrap();
        assert!(node.lock().unwrap().sync_pending.is_empty());
        assert!(pool.completed.values().any(Option::is_some));
        assert!(pool.peers.iter().any(|peer| peer.body.is_some()));
        release.send(()).unwrap();
        drop(pool); // joins the retained owner after its delivery queue closes
    });
}

#[test]
fn a_stalled_poll_cannot_block_the_coordinator_or_other_peers_hash_and_body_work() {
    let (_, node, blocks) = fixture();
    let events = Arc::new(Mutex::new(vec![]));
    let (mut first, entered, _ready) = source(Fake::new(1, &blocks, &events));
    let (release, resumed) = channel();
    first.poll_gate = Some(resumed);
    let mut sources = vec![([1; 32], first)];
    for id in 2..=4 {
        let (value, _, _) = source(Fake::new(id, &blocks, &events));
        sources.push(([id; 32], value));
    }
    let mut pool = PeerPool::new(sources);
    thread::scope(|scope| {
        let (finished, result) = channel();
        let worker = scope.spawn(move || {
            let _ = finished.send(pool.poll());
            pool
        });
        let release = release;
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let mut pool = worker.join().unwrap();
        let window = pool.window(1, 2, &node, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            window.heights,
            [vec![blocks[1].clone()], vec![blocks[2].clone()]]
        );
        assert!(pool
            .peers
            .iter()
            .find(|peer| peer.id == [1; 32])
            .unwrap()
            .inflight
            .is_some());
        release.send(()).unwrap();
        drop(pool);
    });
}
