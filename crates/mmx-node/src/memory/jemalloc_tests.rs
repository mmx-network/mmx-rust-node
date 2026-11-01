use super::{heap, Heap};
use std::{
    hint::black_box,
    process::Command,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

const MIB: usize = 1024 * 1024;

fn rss() -> Result<usize, String> {
    std::fs::read_to_string("/proc/self/status")
        .map_err(|error| error.to_string())?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| "missing RSS".to_owned())?
        .parse::<usize>()
        .map(|kib| kib * 1024)
        .map_err(|error| error.to_string())
}

#[test]
fn repeated_bursts_are_reclaimed_with_workers_alive() {
    if std::env::var_os("MMX_JEMALLOC_TEST").is_none() {
        // A fresh process isolates the baseline and allocator configuration
        // from other tests. Exercise the actual executable's global allocator.
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "memory::jemalloc_tests::repeated_bursts_are_reclaimed_with_workers_alive",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MMX_JEMALLOC_TEST", "1")
            .env_remove("_RJEM_MALLOC_CONF")
            .env_remove("MALLOC_CONF")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    assert!(tikv_jemalloc_ctl::background_thread::read().unwrap());
    let baseline = heap().unwrap();
    let baseline_rss = rss().unwrap();
    let gate = Arc::new(Barrier::new(9));
    let observations = thread::scope(|scope| {
        for _ in 0..8 {
            let gate = gate.clone();
            scope.spawn(move || {
                let mut pins = Vec::new();
                for round in 0..3 {
                    let mut buffers = Vec::new();
                    for _ in 0..16 {
                        buffers.push(vec![0xa5u8; 512 * 1024]);
                        if round == 0 {
                            pins.push(vec![0x5au8; 4096]);
                        }
                    }
                    black_box(&buffers);
                    gate.wait();
                    gate.wait();
                    drop(buffers);
                    gate.wait();
                    gate.wait();
                }
                black_box(pins);
            });
        }
        let mut observations = Vec::new();
        for round in 0..3 {
            gate.wait();
            let peak = heap().map_err(|error| error.to_string());
            gate.wait();
            gate.wait();
            // Do not force purge or flush worker caches. Wait for normal
            // background decay while all workers and their small pins live.
            let released = wait_for_decay(baseline, baseline_rss);
            // Always release workers before inspecting Results or asserting,
            // so a diagnostic failure cannot deadlock the scoped join.
            gate.wait();
            observations.push((round, peak, released));
        }
        observations
    });
    for (round, peak, released) in observations {
        let peak = peak.unwrap();
        let (released, released_rss) = released.unwrap();
        eprintln!("jemalloc round={round} baseline={baseline:?} peak={peak:?} released={released:?} baseline_rss={baseline_rss} released_rss={released_rss}");
        assert!(peak.allocated >= baseline.allocated + 60 * MIB);
        assert!(released.allocated <= baseline.allocated + 8 * MIB);
        assert!(released.resident <= baseline.resident + 32 * MIB);
        assert!(released_rss <= baseline_rss + 32 * MIB);
    }
}

fn wait_for_decay(baseline: Heap, baseline_rss: usize) -> Result<(Heap, usize), String> {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let current = heap().map_err(|error| error.to_string())?;
        let current_rss = rss()?;
        if (current.allocated <= baseline.allocated + 8 * MIB
            && current.resident <= baseline.resident + 32 * MIB
            && current_rss <= baseline_rss + 32 * MIB)
            || Instant::now() >= deadline
        {
            return Ok((current, current_rss));
        }
        thread::sleep(Duration::from_millis(200));
    }
}
