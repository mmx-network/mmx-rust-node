use mmx_consensus::{proof, Record};
use mmx_farming::{harvester::Harvester, plot::Prover};
use mmx_types::{mmx, VnxType};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::Path,
};
use vnx::serde_json::{self, Value};
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures.json")).unwrap()
}
fn unhex(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap()).unwrap()
}
fn bits(data: &mut [u8], offset: usize, count: usize, value: u64) {
    for i in 0..count {
        data[(offset + i) / 8] |= (((value >> i) & 1) as u8) << ((offset + i) % 8);
    }
}
fn make_plot(path: &Path, has_meta: bool, compressed: bool) -> mmx::PlotHeader {
    let f = fixture();
    let p = Record::decode(&unhex(&f["proof"]["wire"])).unwrap();
    let expected = &f["compute"]["entries"][0];
    assert_eq!(
        p.field("plot_id").unwrap().bytes().unwrap(),
        unhex(&f["compute"]["id"])
    );
    let xs: Vec<u32> = expected["proof"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap() as u32)
        .collect();
    let mut h = mmx::PlotHeader {
        ksize: 8,
        xbits: if compressed { 7 } else { 8 },
        has_meta,
        seed: p
            .field("seed")
            .unwrap()
            .bytes()
            .unwrap()
            .try_into()
            .map(vnx::Fixed)
            .unwrap(),
        plot_id: p
            .field("plot_id")
            .unwrap()
            .bytes()
            .unwrap()
            .try_into()
            .map(vnx::Fixed)
            .unwrap(),
        farmer_key: p
            .field("farmer_key")
            .unwrap()
            .bytes()
            .unwrap()
            .try_into()
            .map(vnx::Fixed)
            .unwrap(),
        park_size_x: 128,
        park_size_y: 1,
        park_size_pd: 128,
        park_size_meta: 1,
        park_bytes_x: 256,
        park_bytes_y: 4,
        park_bytes_pd: 160,
        park_bytes_meta: 12,
        entry_bits_x: 16,
        num_entries_y: 1,
        table_offset_y: 4096,
        table_offset_meta: 4100,
        table_offset_x: 4112,
        table_offset_pd: (0..7).map(|i| 4368 + i * 160).collect(),
        plot_size: 5488,
        ..Default::default()
    };
    if p.name == "mmx.ProofOfSpaceNFT" {
        h.contract = Some(vnx::Fixed(
            p.field("contract")
                .unwrap()
                .bytes()
                .unwrap()
                .try_into()
                .unwrap(),
        ));
    }
    let mut data = vec![0; h.plot_size as usize];
    let y = expected["y"].as_u64().unwrap();
    data[4096..4100].copy_from_slice(&(y as u32).to_le_bytes());
    let meta = unhex(&expected["meta"]);
    for (i, v) in meta.as_chunks::<4>().0.iter().enumerate() {
        bits(
            &mut data[4100..4112],
            i * 8,
            8,
            u32::from_le_bytes(*v).into(),
        );
    }
    for (i, pair) in xs.as_chunks::<2>().0.iter().enumerate() {
        let (mut a, mut b) = (
            pair[0] >> u32::from(compressed),
            pair[1] >> u32::from(compressed),
        );
        if a < b {
            std::mem::swap(&mut a, &mut b);
        }
        if compressed {
            a += 1;
            b += 1;
            if a == b {
                a += 1;
                b = 0;
            }
        }
        let index = u64::from(a) * u64::from(a - 1) / 2 + u64::from(b);
        bits(&mut data[4112..4368], i * 16, 16, index);
    }
    for base in &h.table_offset_pd {
        let park = &mut data[*base as usize..*base as usize + 160];
        for i in 0..128 {
            bits(park, i * 8, 8, 2 * i as u64);
            bits(park, 1024 + i * 2, 2, 1);
        }
    }
    let header = h.encode(&mmx_types::vnx_registry().unwrap()).unwrap();
    assert!(header.len() < 4096);
    data[..header.len()].copy_from_slice(&header);
    fs::write(path, data).unwrap();
    h
}
#[test]
fn native_plot_parks_recover_cpp_quality_and_canonical_proof_for_hdd_ssd_and_compression() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture();
    let accelerator = (std::env::var("MMX_OPENCL_TESTS").as_deref() == Ok("1")).then(|| {
        std::sync::Arc::new(
            mmx_consensus::accelerator::Accelerator::new(
                0,
                None,
                std::env::var("MMX_OPENCL_ALL_DEVICES").as_deref() != Ok("1"),
            )
            .unwrap(),
        )
    });
    for (has_meta, compressed) in [(true, false), (false, false), (true, true)] {
        let path = dir.path().join(format!("{has_meta}-{compressed}.plot"));
        let h = make_plot(&path, has_meta, compressed);
        let mut p = Prover::open(&path).unwrap();
        let mut challenge = [0; 32];
        challenge[..4].copy_from_slice(
            &(f["compute"]["entries"][0]["y"].as_u64().unwrap() as u32).to_le_bytes(),
        );
        let qualities = p.qualities(&challenge, 0, 65536).unwrap();
        assert_eq!(qualities.len(), 1);
        assert_eq!(
            qualities[0].meta.as_slice(),
            unhex(&f["compute"]["entries"][0]["meta"])
        );
        let full = p.full_proof(0, 65536).unwrap();
        if let Some(accelerator) = &accelerator {
            p.set_accelerator(Some(accelerator.clone()));
            let gpu = p.full_proof(0, 65536).unwrap();
            assert_eq!(gpu.proof, full.proof);
            assert_eq!(gpu.meta, full.meta);
            assert!(accelerator.enabled());
        }
        assert_eq!(full.meta, qualities[0].meta);
        assert_eq!(
            full.proof,
            f["compute"]["entries"][0]["proof"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap() as u32)
                .collect::<Vec<_>>()
        );
        assert!(p.full_proof(1, 65536).is_err());
        if compressed {
            assert!(p.full_proof(0, 1).is_err());
        }
        let mut challenge = [0; 32];
        challenge[0] = 255;
        assert!(p.qualities(&challenge, 0, 65536).unwrap().is_empty());
        // A changed plot identity must fail before any park lookup.
        let mut h = h;
        h.plot_id.0[0] ^= 1;
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(&h.encode(&mmx_types::vnx_registry().unwrap()).unwrap())
            .unwrap();
        assert!(Prover::open(&path).is_err());
    }
}
#[test]
fn stale_plot_size_metadata_and_trailing_padding_preserve_native_proofs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("size-metadata.plot");
    let mut header = make_plot(&path, true, false);
    let expected = Prover::open(&path).unwrap().full_proof(0, 65536).unwrap();
    let length = fs::metadata(&path).unwrap().len();
    for declared in [0, length - 4096, length + 4096] {
        header.plot_size = declared;
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(&header.encode(&mmx_types::vnx_registry().unwrap()).unwrap())
            .unwrap();
        let prover = Prover::open(&path).unwrap();
        assert_eq!(prover.length(), length);
        let proof = prover.full_proof(0, 65536).unwrap();
        assert_eq!(proof.meta, expected.meta);
        assert_eq!(proof.proof, expected.proof);
    }
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(length + 4096).unwrap();
    let prover = Prover::open(&path).unwrap();
    assert_eq!(prover.length(), length + 4096);
    assert_eq!(prover.full_proof(0, 65536).unwrap().proof, expected.proof);
    // Metadata never permits reading beyond the real file, even if it claims
    // a larger size. Cutting the last required park must still be rejected.
    file.set_len(length - 1).unwrap();
    assert!(Prover::open(&path).is_err());
}
#[test]
fn discovery_deduplicates_ids_rejects_unknown_keys_and_does_not_follow_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.plot");
    let h = make_plot(&path, true, false);
    fs::copy(&path, dir.path().join("two.plot")).unwrap();
    fs::write(dir.path().join("broken.plot"), b"bad").unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path(), sub.join("loop")).unwrap();
    let mut harvester = Harvester::new("test".into(), [h.farmer_key.0]).unwrap();
    harvester.directories.insert(dir.path().to_path_buf());
    harvester.reload();
    assert_eq!(harvester.plots().len(), 1);
    assert_eq!(harvester.failures.len(), 2);
    let info = harvester.farm_info().unwrap();
    assert_eq!(info.total_bytes, 5488);
    assert_eq!(info.plot_count[&8], 1);
    assert_eq!(info.total_bytes_effective, 17 * 128 * 2464 / 1000);
    harvester.farmer_keys.clear();
    harvester.reload();
    assert!(harvester.plots().is_empty());
    // Truncation and oversized dimensions fail without large allocations.
    let mut h = h;
    h.park_bytes_x = i32::MAX;
    let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&h.encode(&mmx_types::vnx_registry().unwrap()).unwrap())
        .unwrap();
    assert!(Prover::open(&path).is_err());
}
#[test]
fn harvested_response_is_accepted_by_the_cpp_derived_consensus_verifier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.plot");
    let h = make_plot(&path, true, false);
    let f = fixture();
    let mut harvester = Harvester::new("test".into(), [h.farmer_key.0]).unwrap();
    harvester.directories.insert(dir.path().into());
    harvester.reload();
    let params = mmx::ChainParams::decode(&unhex(&f["proof"]["params"])).unwrap();
    let challenge = mmx::Challenge {
        vdf_height: f["proof"]["height"].as_u64().unwrap() as u32,
        challenge: vnx::Fixed(unhex(&f["proof"]["challenge"]).try_into().unwrap()),
        difficulty: 1,
    };
    let responses = harvester.lookup(&challenge, &params).unwrap();
    assert_eq!(responses.len(), 1);
    let p = responses[0].field("proof").unwrap().record().unwrap();
    proof::verify(p, &params, &challenge.challenge.0, 1, challenge.vdf_height).unwrap();
    for workers in [1, 4] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap();
        let result = pool
            .install(|| harvester.lookup(&challenge, &params))
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].field("proof").unwrap().record().unwrap(), p);
        assert!(mmx_consensus::response::is_valid(&result[0]).unwrap());
        assert!(pool
            .install(|| harvester.lookup_for(&challenge, &params, std::time::Duration::ZERO))
            .unwrap()
            .is_empty());
    }
}

#[cfg(unix)]
fn stalled_farm() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Harvester,
    mmx::ChainParams,
    mmx::Challenge,
) {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let fast = dir.path().join("fast.plot");
    let slow = dir.path().join("slow.plot");
    let header = make_plot(&fast, true, false);
    let mut slow_header = make_plot(&slow, true, false);
    // Put the stalled plot first in the ID map, so the old sequential loop
    // cannot deliver the valid proof until that plot has been released.
    let f = fixture();
    let params = mmx::ChainParams::decode(&unhex(&f["proof"]["params"])).unwrap();
    let challenge = mmx::Challenge {
        vdf_height: f["proof"]["height"].as_u64().unwrap() as u32,
        challenge: vnx::Fixed(unhex(&f["proof"]["challenge"]).try_into().unwrap()),
        difficulty: 1,
    };
    let mut identity = Record::decode(&unhex(&f["proof"]["wire"])).unwrap();
    for seed in 0u64.. {
        slow_header.seed.0[..8].copy_from_slice(&seed.to_le_bytes());
        identity.fields.insert(
            "seed".into(),
            mmx_consensus::Value::Bytes(slow_header.seed.0.to_vec()),
        );
        slow_header.plot_id = proof::calc_plot_id(&identity).unwrap().into();
        if slow_header.plot_id.0 < header.plot_id.0
            && proof::check_plot_filter(
                &challenge.challenge.0,
                &slow_header.plot_id.0,
                params.plot_filter,
            )
        {
            break;
        }
    }
    fs::OpenOptions::new()
        .write(true)
        .open(&slow)
        .unwrap()
        .write_all(
            &slow_header
                .encode(&mmx_types::vnx_registry().unwrap())
                .unwrap(),
        )
        .unwrap();
    let mut harvester = Harvester::new("parallel fixture".into(), [header.farmer_key.0]).unwrap();
    harvester.directories.insert(dir.path().to_path_buf());
    harvester.reload();
    assert_eq!(harvester.plots().len(), 2);
    fs::remove_file(&slow).unwrap();
    let path = std::ffi::CString::new(slow.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    (dir, slow, harvester, params, challenge)
}

#[cfg(unix)]
#[test]
fn parallel_lookup_publishes_native_proof_before_a_stalled_plot_finishes() {
    use std::{
        os::unix::fs::OpenOptionsExt,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        },
        thread,
        time::{Duration, Instant},
    };
    let (_dir, slow, harvester, params, challenge) = stalled_farm();
    let (tx, rx) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    let expected_challenge = challenge.clone();
    let expected_params = params.clone();
    let worker = thread::spawn(move || {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            harvester.lookup_each(&challenge, &params, Duration::from_secs(30), |proof| {
                assert_eq!(rayon::current_num_threads(), 2);
                assert!(rayon::current_thread_index().is_some());
                tx.send(proof).unwrap();
            })
        })
        .unwrap();
        finished.store(true, Ordering::Release);
    });
    let response = rx.recv_timeout(Duration::from_secs(5));
    let published_before_finish = !done.load(Ordering::Acquire);
    // Release the stalled open even when testing a sequential regression. A
    // FIFO fails positional reads, exercising isolation of a failing plot too.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&slow)
        {
            Ok(file) => {
                drop(file);
                break;
            }
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(e) => panic!("cannot release stalled plot: {e}"),
        }
    }
    worker.join().unwrap();
    let response = response.expect("a stalled plot held up another plot's proof");
    assert!(published_before_finish);
    assert!(mmx_consensus::response::is_valid(&response).unwrap());
    proof::verify(
        response.field("proof").unwrap().record().unwrap(),
        &expected_params,
        &expected_challenge.challenge.0,
        1,
        expected_challenge.vdf_height,
    )
    .unwrap();
    assert!(rx.try_recv().is_err());
}

#[test]
fn cpp_written_and_cpp_proven_plot_files_are_read_without_conversion() {
    let f = fixture();
    let dir = tempfile::tempdir().unwrap();
    for (mode, bytes) in [
        include_bytes!("native/native-0.plot").as_slice(),
        include_bytes!("native/native-1.plot").as_slice(),
        include_bytes!("native/native-2.plot").as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.path().join(format!("native-{mode}.plot"));
        fs::write(&path, bytes).unwrap();
        let p = Prover::open(&path).unwrap();
        let full = p.full_proof(0, 65536).unwrap();
        assert_eq!(
            full.meta.as_slice(),
            unhex(&f["compute"]["entries"][0]["meta"])
        );
        assert_eq!(
            full.proof,
            f["compute"]["entries"][0]["proof"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap() as u32)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn quality_ranges_include_duplicates_across_parks_and_the_short_final_park() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ranges.plot");
    let mut h = make_plot(&path, true, false);
    h.num_entries_y = 10;
    h.park_size_y = 3;
    h.park_bytes_y = 6;
    h.table_offset_y = 5500;
    h.table_offset_meta = 6000;
    h.park_size_meta = 4;
    h.park_bytes_meta = 48;
    h.plot_size = 7000;
    let mut bytes = fs::read(&path).unwrap();
    bytes.resize(7000, 0);
    let ys = [20u32, 21, 21, 21, 22, 23, 23, 23, 24, 25];
    for (park, entries) in ys.chunks(3).enumerate() {
        let data = &mut bytes[5500 + park * 6..5506 + park * 6];
        data.fill(0);
        data[..4].copy_from_slice(&entries[0].to_le_bytes());
        for i in 1..entries.len() {
            bits(
                data,
                32 + (i - 1) * 2,
                2,
                u64::from(entries[i] - entries[i - 1]),
            );
        }
    }
    for index in 0..ys.len() {
        let offset = 6000 + (index / 4) * 48;
        let data = &mut bytes[offset..offset + 48];
        for j in 0..12 {
            bits(data, ((index % 4) * 12 + j) * 8, 8, index as u64);
        }
    }
    let header = h.encode(&mmx_types::vnx_registry().unwrap()).unwrap();
    bytes[..header.len()].copy_from_slice(&header);
    fs::write(&path, bytes).unwrap();
    let p = Prover::open(&path).unwrap();
    for (begin, filter, indices) in [
        (21, 0, vec![1, 2, 3]),
        (23, 0, vec![5, 6, 7]),
        (22, 1, vec![4, 5, 6, 7]),
        (25, 0, vec![9]),
        (19, 0, vec![]),
        (255, 0, vec![]),
    ] {
        let mut challenge = [0; 32];
        challenge[0] = begin;
        let result = p.qualities(&challenge, filter, 65536).unwrap();
        assert_eq!(result.iter().map(|q| q.index).collect::<Vec<_>>(), indices);
        for q in result {
            assert_eq!(
                u32::from_le_bytes(q.meta[..4].try_into().unwrap()) as u64,
                q.index
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn overlapping_challenges_keep_reporting_and_deliver_new_work_while_one_plot_is_stalled() {
    use mmx_farming::harvester::LookupScheduler;
    use std::{
        os::unix::fs::OpenOptionsExt,
        sync::{mpsc, Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };
    let (_dir, slow, harvester, params, mut challenge) = stalled_farm();
    let harvester = Arc::new(Mutex::new(harvester));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let mut scheduler = LookupScheduler::new(2).unwrap();
    let (proofs, responses) = mpsc::channel();
    let (done, reports) = mpsc::channel();
    let emit = Arc::new(move |proof| {
        let _ = proofs.send(proof);
    });
    let report = Arc::new(move |info| {
        let _ = done.send(info);
    });
    let job = harvester
        .lock()
        .unwrap()
        .lookup_job(&challenge, &params, Duration::from_secs(30))
        .unwrap();
    scheduler.submit(job, emit.clone(), report.clone());
    pool.install(|| scheduler.poll()).unwrap();
    let first = responses.recv_timeout(Duration::from_secs(5));
    // A second admitted challenge has a different difficulty. Both use the
    // native valid proof witness; no production deduplication is bypassed.
    challenge.vdf_height += 1;
    challenge.difficulty = 2;
    let started = Instant::now();
    let (job, info) = {
        let h = harvester.lock().unwrap();
        (
            h.lookup_job(&challenge, &params, Duration::from_secs(30))
                .unwrap(),
            h.farm_info().unwrap(),
        )
    };
    let report_delay = started.elapsed();
    scheduler.submit(job, emit, report);
    let until = Instant::now() + Duration::from_secs(5);
    let mut second = None;
    while Instant::now() < until {
        pool.install(|| scheduler.poll()).unwrap();
        if let Ok(proof) = responses.try_recv() {
            second = Some(proof);
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let still_running = scheduler.active();
    let completed_before_release = reports.try_recv().is_ok();
    // Remove queued references to the FIFO before releasing the one blocked
    // read, so a regression can fail without hanging the test runner.
    scheduler.clear_pending();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&slow)
        {
            Ok(file) => {
                drop(file);
                break;
            }
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) && Instant::now() < until => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(e) => panic!("cannot release stalled plot: {e}"),
        }
    }
    let until = Instant::now() + Duration::from_secs(5);
    while scheduler.active() > 0 && Instant::now() < until {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(first.is_ok());
    assert_eq!(info.plot_count.values().sum::<u32>(), 2);
    assert!(report_delay < Duration::from_secs(1));
    assert_eq!(still_running, 1);
    assert!(!completed_before_release);
    assert_eq!(scheduler.active(), 0);
    let second = second.expect("new challenge proof waited for the old stalled plot");
    assert_eq!(
        second.field("vdf_height").unwrap().uint().unwrap(),
        u64::from(challenge.vdf_height)
    );
    assert!(mmx_consensus::response::is_valid(&second).unwrap());
    proof::verify(
        second.field("proof").unwrap().record().unwrap(),
        &params,
        &challenge.challenge.0,
        challenge.difficulty,
        challenge.vdf_height,
    )
    .unwrap();
}
