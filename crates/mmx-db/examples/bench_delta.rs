//! Small replay I/O comparison; uses about 4 MiB of logical history.
use mmx_db::{Store, Write};
use std::{collections::BTreeMap, fs, time::Instant};
fn counters() -> BTreeMap<String, u64> {
    fs::read_to_string("/proc/self/io")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(": ")?;
            Some((key.into(), value.parse().ok()?))
        })
        .collect()
}
fn report(
    phase: &str,
    start: Instant,
    before: &BTreeMap<String, u64>,
    after: &BTreeMap<String, u64>,
) {
    let delta = |key: &str| {
        after
            .get(key)
            .unwrap_or(&0)
            .saturating_sub(*before.get(key).unwrap_or(&0))
    };
    println!("{{\"phase\":\"{}\",\"seconds\":{},\"write_syscall_bytes\":{},\"disk_write_bytes\":{},\"read_syscall_bytes\":{}}}",phase,start.elapsed().as_secs_f64(),delta("wchar"),delta("write_bytes"),delta("rchar"));
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let mut db = Store::open_native(directory.path())?;
    db.commit_batch(1, [Write::put("base", b"key", b"retained")])?;
    let before = counters();
    let start = Instant::now();
    let mut branch = db.fork(1)?;
    for height in 1..=16u32 {
        let writes = (0..64u32).map(|key| {
            let mut value = vec![height as u8; 4096];
            value[..4].copy_from_slice(&key.to_le_bytes());
            Write::put("tail", key.to_be_bytes(), value)
        });
        branch.commit_batch(height + 1, writes)?;
    }
    let after = counters();
    report("stage", start, &before, &after);
    println!(
        "{{\"staging_files\":{}}}",
        fs::read_dir(directory.path().join("staging")).map_or(0, |entries| entries.count())
    );
    let before = counters();
    let start = Instant::now();
    db.replace_suffix(1, &branch)?;
    db.checkpoint()?;
    let after = counters();
    report("publish", start, &before, &after);
    drop(branch);
    drop(db);
    let mut db = Store::open_native(directory.path())?;
    let mut digest = crc32fast::Hasher::new();
    for height in 1..=16u32 {
        for key in 0..64u32 {
            let value = db.get("tail", &key.to_be_bytes(), Some(height))?.unwrap();
            assert_eq!(&value[..4], &key.to_le_bytes());
            assert!(value[4..].iter().all(|byte| *byte == height as u8));
            digest.update(&value);
        }
    }
    db.revert(1)?;
    assert!(db.scan("tail", &[], None)?.is_empty());
    assert_eq!(db.get("base", b"key", None)?, Some(b"retained".to_vec()));
    db.revert(0)?;
    drop(db);
    assert_eq!(Store::open_native(directory.path())?.version(), 0);
    println!(
        "{{\"verified\":true,\"history_crc32\":{},\"network_downloads\":0}}",
        digest.finalize()
    );
    Ok(())
}
