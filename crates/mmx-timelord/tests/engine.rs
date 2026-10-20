use mmx_consensus::{vdf, Record};
use mmx_timelord::{
    engine::{Config, Engine},
    identity::Identity,
    Affinity, TimeLord,
};
use mmx_types::{mmx, VnxType};
use std::time::{Duration, Instant};
fn request(
    height: u32,
    start: u64,
    end: u64,
    infuse: u8,
    input: Option<u8>,
) -> mmx::IntervalRequest {
    let hash = |n| {
        let mut h = [0; 32];
        h[0] = n;
        h.into()
    };
    mmx::IntervalRequest {
        vdf_height: height,
        start,
        end,
        infuse: hash(infuse),
        input: input.map(hash),
    }
}
fn step(engine: &mut Engine, count: usize) {
    for _ in 0..count {
        let job = engine.job().unwrap();
        let output = job.compute();
        engine.complete(&job, output, Duration::from_micros(1));
    }
}
#[test]
fn original_cpp_timelord_requests_retries_forecasts_forks_stop_and_signatures() {
    let mut count = 0;
    for line in include_str!("fixtures/timelord.jsonl").lines() {
        let row: vnx::serde_json::Value = vnx::serde_json::from_str(line).unwrap();
        let decode =
            |v: &vnx::serde_json::Value| vnx::util::from_hex_string(v.as_str().unwrap()).unwrap();
        let key = decode(&row["key_file"]);
        assert_eq!(&key[..10], &[17, 0, 3, 0, 11, 0, 32, 0, 1, 0]);
        let signer = Identity::from_bytes(key[10..].try_into().unwrap()).unwrap();
        let mut config = Config::new(2);
        config.max_history = 32;
        config.reward_addr[0] = 7;
        let mut engine = Engine::new(config).unwrap();
        for event in row["events"].as_array().unwrap() {
            if event["submit"].is_string() {
                engine
                    .submit(mmx::IntervalRequest::decode(&decode(&event["submit"])).unwrap())
                    .unwrap();
            }
            if let Some(steps) = event["advance"].as_u64() {
                step(&mut engine, steps as usize);
            }
            if event["refresh"] == true {
                engine.refresh_publication();
            }
            if event["stop"] == true {
                engine.stop();
            }
            let completed = engine.take_completed(1000);
            let expected = event["proofs"].as_array().unwrap();
            assert_eq!(completed.len(), expected.len(), "{} / {event}", row["name"]);
            for (completed, expected) in completed.into_iter().zip(expected) {
                let proof = signer.sign(completed.proof).unwrap();
                let record = Record::from_generated(&proof).unwrap();
                let reference = Record::decode(&decode(expected)).unwrap();
                assert_eq!(record, reference, "{}", row["name"]);
                assert!(vdf::is_valid(&record).unwrap());
                vdf::authenticate(&record).unwrap();
                vdf::verify_cpu(&record).unwrap();
            }
        }
        count += 1;
    }
    assert_eq!(count, 5);
}
#[test]
fn fork_and_stop_fence_inflight_completions_and_expired_history_never_invents_input() {
    let mut config = Config::new(2);
    config.max_history = 4;
    let mut engine = Engine::new(config).unwrap();
    engine.submit(request(1, 0, 4, 10, Some(1))).unwrap();
    let old = engine.job().unwrap();
    engine.submit(request(8, 100, 104, 30, Some(99))).unwrap();
    assert!(!engine.complete(&old, old.compute(), Duration::from_micros(1)));
    assert_eq!(engine.status().iterations, 100);
    step(&mut engine, 2);
    let completed = engine.take_completed(1);
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].proof.start, 100);
    engine.submit(request(9, 104, 108, 31, None)).unwrap();
    let stopped = engine.job().unwrap();
    engine.stop();
    assert!(!engine.complete(&stopped, stopped.compute(), Duration::from_micros(1)));
    assert!(engine.job().is_none());
    assert_eq!(engine.status().history, 0);
    assert_eq!(engine.status().pending, 0);
    engine.submit(request(1, 0, 4, 10, Some(1))).unwrap();
    step(&mut engine, 2);
    engine.take_completed(1);
    for i in 0..20 {
        engine
            .submit(request(
                2 + i,
                4 + u64::from(i) * 2,
                6 + u64::from(i) * 2,
                20,
                None,
            ))
            .unwrap();
    }
    step(&mut engine, 20);
    assert_eq!(engine.status().history, 4);
    let before = engine.status();
    assert!(engine.submit(request(1, 0, 4, 11, None)).is_err());
    assert_eq!(engine.status().iterations, before.iterations);
    assert_eq!(engine.status().generation, before.generation);
}
#[test]
fn supplied_network_input_wins_over_missing_or_conflicting_old_history_on_restart() {
    let mut engine = Engine::new(Config::new(2)).unwrap();
    engine.submit(request(8, 100, 104, 30, Some(99))).unwrap();
    step(&mut engine, 2);
    engine.take_completed(1);
    // C++ history[start] inserts zero and then ignores the supplied input while
    // is_reset is true. Keep the actual network anchor and produce usable work.
    engine.submit(request(6, 80, 84, 40, Some(42))).unwrap();
    step(&mut engine, 2);
    let proof = engine.take_completed(1).pop().unwrap().proof;
    assert_eq!(proof.input.0[0], 42);
    assert_eq!(proof.start, 80);
    engine.submit(request(6, 80, 84, 41, Some(43))).unwrap();
    step(&mut engine, 2);
    assert_eq!(engine.take_completed(1).pop().unwrap().proof.input.0[0], 43);
}
#[test]
fn malformed_intervals_and_forecast_floods_do_not_start_hashing_or_grow_without_bound() {
    let mut engine = Engine::new(Config::new(2)).unwrap();
    for req in [
        request(0, 0, 2, 1, Some(1)),
        request(1, 2, 0, 1, Some(1)),
        request(1, 0, 0, 1, Some(1)),
        request(1, 0, 3, 1, Some(1)),
        request(1, 0, 2_000_000, 1, Some(1)),
    ] {
        assert!(engine.submit(req).is_err());
        assert!(engine.job().is_none());
        assert_eq!(engine.status().pending, 0);
    }
    for i in 0..1000 {
        engine
            .submit(request(
                i + 1,
                u64::from(i) * 2,
                u64::from(i) * 2 + 2,
                1,
                None,
            ))
            .unwrap();
    }
    assert!(engine.submit(request(1001, 2000, 2002, 1, None)).is_err());
    assert_eq!(engine.status().pending, 1000);
    assert_eq!(engine.status().infusions, 1000);
    assert!(engine.job().is_none());
    // An authoritative network anchor must recover even after the forecast cap.
    engine
        .submit(request(1001, 10_000, 10_002, 2, Some(9)))
        .unwrap();
    step(&mut engine, 1);
    assert_eq!(engine.take_completed(1).len(), 1);
}
#[test]
fn restart_discards_forecasts_on_an_obsolete_segment_lattice() {
    let mut engine = Engine::new(Config::new(4)).unwrap();
    engine.submit(request(1, 0, 8, 1, Some(1))).unwrap();
    engine.submit(request(8, 200, 208, 2, None)).unwrap();
    engine.submit(request(3, 101, 109, 3, Some(9))).unwrap();
    assert_eq!(engine.status().pending, 1);
    step(&mut engine, 2);
    assert_eq!(engine.take_completed(4).len(), 1);
    assert!(engine.job().is_none());
}
#[test]
fn changed_forecast_rewinds_past_removed_infusions_and_never_signs_overlapping_chains() {
    let mut engine = Engine::new(Config::new(2)).unwrap();
    engine.submit(request(1, 0, 4, 10, Some(1))).unwrap();
    engine.submit(request(2, 4, 6, 20, None)).unwrap();
    step(&mut engine, 3);
    engine.take_completed(1);
    // Merge the intervals after an old middle infusion was already computed.
    engine.submit(request(1, 0, 8, 10, Some(1))).unwrap();
    step(&mut engine, 4);
    let completed = engine.take_completed(4);
    assert_eq!(completed.len(), 1);
    let mut key = [0; 32];
    key[31] = 1;
    let signer = Identity::from_bytes(&key).unwrap();
    vdf::verify_cpu(
        &Record::from_generated(
            &signer
                .sign(completed.into_iter().next().unwrap().proof)
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    engine.stop();
    engine.submit(request(1, 0, 8, 10, Some(1))).unwrap();
    engine.submit(request(2, 4, 6, 20, None)).unwrap();
    step(&mut engine, 4);
    // Only the replacement 4..6 interval remains signable.
    let completed = engine.take_completed(4);
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].proof.start, 4);
    vdf::verify_cpu(
        &Record::from_generated(
            &signer
                .sign(completed.into_iter().next().unwrap().proof)
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
}
#[test]
fn stable_native_identity_is_private_and_corruption_is_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("timelord_sk.dat");
    let first = Identity::load_or_create(&path).unwrap().public_key();
    let data = std::fs::read(&path).unwrap();
    assert_eq!(data.len(), 42);
    assert_eq!(Identity::load_or_create(&path).unwrap().public_key(), first);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(Identity::load_or_create(&path).unwrap().public_key(), first);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o400
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(&path, b"corrupt").unwrap();
    assert!(Identity::load_or_create(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
}
#[cfg(target_os = "linux")]
#[test]
fn hashing_affinity_does_not_change_the_callers_allowed_cpu_set() {
    let allowed = || {
        std::fs::read_to_string("/proc/thread-self/status")
            .unwrap()
            .lines()
            .find(|line| line.starts_with("Cpus_allowed_list:"))
            .unwrap()
            .to_string()
    };
    let before = allowed();
    let first = before
        .split_whitespace()
        .nth(1)
        .unwrap()
        .split(['-', ','])
        .next()
        .unwrap()
        .parse()
        .unwrap();
    for affinity in [Affinity::Auto, Affinity::Core(first), Affinity::Any] {
        let mut worker = TimeLord::spawn(Config::new(2), affinity).unwrap();
        assert_eq!(allowed(), before);
        worker.shutdown().unwrap();
        assert_eq!(allowed(), before);
    }
}
#[test]
fn dedicated_worker_emits_work_and_shutdown_cancels_a_long_segment_promptly() {
    let mut config = Config::new(32);
    config.max_history = 100;
    let mut worker = TimeLord::spawn(config, Affinity::Auto).unwrap();
    let control = worker.control();
    control.submit(request(1, 0, 64, 2, Some(1))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let ready = control
            .wait_completed(Duration::from_millis(50), 1)
            .unwrap();
        if !ready.is_empty() {
            let mut key = [0; 32];
            key[31] = 1;
            let proof = Identity::from_bytes(&key)
                .unwrap()
                .sign(ready.into_iter().next().unwrap().proof)
                .unwrap();
            vdf::verify_cpu(&Record::from_generated(&proof).unwrap()).unwrap();
            break;
        }
        assert!(Instant::now() < deadline);
    }
    assert!(control.status().unwrap().waiting);
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(control.status().unwrap().iterations, 64);
    control.submit(request(2, 64, 128, 3, None)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if !control
            .wait_completed(Duration::from_millis(50), 1)
            .unwrap()
            .is_empty()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "forecast did not wake the paused worker"
        );
    }
    assert_eq!(control.status().unwrap().iterations, 128);
    control.stop().unwrap();
    worker.shutdown().unwrap();
    assert!(control.submit(request(1, 0, 64, 2, Some(1))).is_err());
    let mut worker = TimeLord::spawn(Config::new(u32::MAX), Affinity::Any).unwrap();
    let control = worker.control();
    control
        .submit(request(1, 0, u64::from(u32::MAX), 2, Some(1)))
        .unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let started = Instant::now();
    worker.shutdown().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(TimeLord::spawn(Config::new(2), Affinity::Core(usize::MAX)).is_err());
}
