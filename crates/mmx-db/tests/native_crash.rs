use mmx_db::{Store, Write};
use std::{
    io::{BufRead, BufReader, Write as IoWrite},
    process::{Command, Stdio},
};

#[test]
#[ignore = "invoked as an isolated child by the crash-recovery test"]
fn native_crash_child() {
    let Some(path) = std::env::var_os("MMX_NATIVE_CRASH_PATH") else {
        return;
    };
    let stage = std::env::var("MMX_NATIVE_CRASH_STAGE").unwrap();
    let mut db = Store::open_native(path).unwrap();
    db.commit_batch(1, [Write::put("base", b"key", b"retained")])
        .unwrap();
    db.checkpoint().unwrap();
    let mut branch = db.fork(1).unwrap();
    for height in 1..=12u32 {
        branch
            .commit_batch(
                height + 1,
                [Write::put(
                    "tail",
                    height.to_be_bytes(),
                    vec![height as u8; 65536],
                )],
            )
            .unwrap();
    }
    if stage != "staged" {
        db.replace_suffix(1, &branch).unwrap();
    }
    if stage == "checkpointed" {
        db.checkpoint().unwrap();
    }
    println!("READY");
    std::io::stdout().flush().unwrap();
    // The parent kills its own child while handles/snapshots remain live.
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
}

#[test]
fn process_death_before_or_after_publication_recovers_an_atomic_suffix_and_reopens_after_rollback()
{
    for stage in ["staged", "published", "checkpointed"] {
        let directory = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_crash_child", "--ignored", "--nocapture"])
            .env("MMX_NATIVE_CRASH_PATH", directory.path())
            .env("MMX_NATIVE_CRASH_STAGE", stage)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut ready = false;
        loop {
            let mut line = String::new();
            if output.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if line.trim() == "READY" {
                ready = true;
                break;
            }
        }
        assert!(ready, "child did not reach {stage}");
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        let mut db = Store::open_native(directory.path()).unwrap();
        assert_eq!(
            db.get("base", b"key", None).unwrap(),
            Some(b"retained".to_vec())
        );
        if stage == "staged" {
            assert_eq!(db.version(), 1);
            assert!(db.scan("tail", &[], None).unwrap().is_empty());
        } else {
            assert_eq!(db.version(), 13);
            for height in 1..=12u32 {
                assert!(db
                    .get("tail", &height.to_be_bytes(), Some(height - 1))
                    .unwrap()
                    .is_none());
                assert_eq!(
                    db.get("tail", &height.to_be_bytes(), Some(height)).unwrap(),
                    Some(vec![height as u8; 65536])
                );
            }
        }
        db.revert(0).unwrap();
        drop(db);
        let db = Store::open_native(directory.path()).unwrap();
        assert_eq!(db.version(), 0);
        assert!(db.scan("base", &[], None).unwrap().is_empty());
        assert!(db.scan("tail", &[], None).unwrap().is_empty());
    }
}
