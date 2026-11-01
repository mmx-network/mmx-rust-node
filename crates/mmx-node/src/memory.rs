//! Keep temporary sync/proof buffers from raising glibc's per-worker heap
//! high-water marks. Apply only in the executable, before starting any workers.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(super) fn configure() {
    // Preserve an operator's explicit allocator policy, including the newer
    // GLIBC_TUNABLES spelling. Other tunables do not override this default.
    let tunables = std::env::var_os("GLIBC_TUNABLES");
    if std::env::var_os("MALLOC_MMAP_THRESHOLD_").is_some()
        || tunables.is_some_and(|value| {
            value
                .as_encoded_bytes()
                .split(|byte| *byte == b':')
                .any(|setting| setting.starts_with(b"glibc.malloc.mmap_threshold="))
        })
    {
        return;
    }
    // Fix the initial 128 KiB threshold instead of letting glibc raise it as
    // large temporary allocations are freed. These buffers can then be unmapped
    // on drop, even with small, long-lived allocations in the same worker.
    // SAFETY: this changes only glibc's allocation policy, with a valid constant
    // and positive threshold. main calls it before creating worker threads.
    if unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, 128 * 1024) } == 0 {
        eprintln!("Could not configure glibc's temporary-buffer allocation threshold");
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(super) fn configure() {}

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    use std::{hint::black_box, process::Command, sync::Arc, sync::Barrier, thread};

    fn rss() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap()
            * 1024
    }

    #[test]
    fn temporary_worker_buffers_return_memory_and_explicit_thresholds_survive() {
        let Some(mode) = std::env::var_os("MMX_ALLOCATOR_TEST") else {
            // Each policy needs a fresh allocator. Never change the allocator
            // policy or environment underneath concurrently running tests.
            for mode in [
                "default",
                "tuned",
                "environment",
                "tunables",
                "other_tunable",
            ] {
                let mut child = Command::new(std::env::current_exe().unwrap());
                child
                    .args([
                        "--exact",
                        "memory::tests::temporary_worker_buffers_return_memory_and_explicit_thresholds_survive",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("MMX_ALLOCATOR_TEST", mode)
                    .env_remove("MALLOC_MMAP_THRESHOLD_")
                    .env_remove("GLIBC_TUNABLES");
                match mode {
                    "environment" => {
                        child.env("MALLOC_MMAP_THRESHOLD_", "8388608");
                    }
                    "tunables" => {
                        child.env("GLIBC_TUNABLES", "glibc.malloc.mmap_threshold=8388608");
                    }
                    "other_tunable" => {
                        child.env("GLIBC_TUNABLES", "glibc.malloc.perturb=0");
                    }
                    _ => {}
                }
                let output = child.output().unwrap();
                assert!(
                    output.status.success(),
                    "{mode}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                eprint!("{}", String::from_utf8_lossy(&output.stdout));
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
            return;
        };
        let explicit = mode == "environment" || mode == "tunables";
        if mode != "default" {
            super::configure();
        }
        // Freeing a large mmap normally raises glibc's dynamic threshold. The
        // node default must remain fixed even after such a workload spike.
        let warmup = vec![0x5au8; 16 * 1024 * 1024];
        black_box(&warmup);
        drop(warmup);
        // SAFETY: mallinfo2 only takes an allocator statistics snapshot.
        let before = unsafe { libc::mallinfo2() };
        let buffer = vec![0xa5u8; 2 * 1024 * 1024];
        black_box(&buffer);
        let during = unsafe { libc::mallinfo2() };
        if explicit {
            assert_eq!(
                during.hblkhd, before.hblkhd,
                "operator threshold was overridden"
            );
        } else if mode != "default" {
            assert!(
                during.hblkhd >= before.hblkhd + buffer.len(),
                "temporary buffer moved into a worker heap"
            );
        }
        drop(buffer);

        // Hold the workers and their small persistent allocations alive after
        // releasing a 64 MiB burst. Thread exit must not hide retained heaps.
        let baseline = rss();
        let gate = Arc::new(Barrier::new(9));
        thread::scope(|scope| {
            for _ in 0..8 {
                let gate = gate.clone();
                scope.spawn(move || {
                    let mut buffers = Vec::new();
                    let mut pins = Vec::new();
                    for _ in 0..16 {
                        buffers.push(vec![0xa5u8; 512 * 1024]);
                        pins.push(vec![0x5au8; 4096]);
                    }
                    black_box(&buffers);
                    gate.wait();
                    gate.wait();
                    drop(buffers);
                    gate.wait();
                    gate.wait();
                    black_box(pins);
                });
            }
            gate.wait();
            let peak = rss();
            gate.wait();
            gate.wait();
            let retained = rss();
            // Release workers before asserting, so a failure cannot deadlock
            // the scope while its workers wait for the parent.
            gate.wait();
            eprintln!("allocator={mode:?} baseline={baseline} peak={peak} retained={retained}");
            if !explicit && mode != "default" {
                assert!(
                    retained < baseline + 16 * 1024 * 1024,
                    "temporary worker buffers retained {retained} bytes after baseline {baseline}"
                );
            }
        });
    }
}
