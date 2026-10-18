use mmx_db::{
    journal::{Action, Change, Journal, ValueRef},
    Error,
};
use std::fs;

fn action(generation: u64, expected: u32, cut: u32, next: u32, floor: u32) -> Action {
    Action {
        generation,
        expected,
        cut,
        next,
        floor,
    }
}

#[test]
fn atomic_append_returns_value_offsets_without_readback_and_recovery_preserves_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("0.log");
    let mut log = Journal::open(&path, 17).unwrap();
    let reader = log.reader();
    assert!(Journal::open(&path, 17).is_err());
    let values = log
        .append(
            action(1, 0, 0, 4, 0),
            &[
                Change {
                    version: 0,
                    table: "a",
                    key: b"one",
                    value: Some(b"original"),
                },
                Change {
                    version: 2,
                    table: "a",
                    key: b"empty",
                    value: Some(b""),
                },
                Change {
                    version: 3,
                    table: "b",
                    key: b"nil",
                    value: None,
                },
            ],
        )
        .unwrap();
    assert_eq!(reader.value(values[0].value.unwrap()).unwrap(), b"original");
    assert_eq!(reader.value(values[1].value.unwrap()).unwrap(), b"");
    assert_eq!(values[2].value, None);
    let reference = values[0].value.unwrap();
    assert_eq!(ValueRef::decode(&reference.encode()).unwrap(), reference);
    let boundary = log.end();
    let replacement = log
        .append(
            action(2, 4, 1, 3, 0),
            &[Change {
                version: 1,
                table: "a",
                key: b"one",
                value: Some(b"replacement"),
            }],
        )
        .unwrap();
    let purge = action(3, 3, 3, 3, 2);
    log.append(purge, &[]).unwrap();
    let end = log.end();
    // References survive later appends; their reader never shares a seek cursor.
    assert_eq!(reader.value(reference).unwrap(), b"original");
    assert_eq!(
        reader.value(replacement[0].value.unwrap()).unwrap(),
        b"replacement"
    );
    assert!(reader
        .value(ValueRef {
            file_id: 18,
            ..reference
        })
        .is_err());
    assert!(ValueRef::decode(&[0; 23]).is_err());
    drop(reader);
    drop(log);
    let mut log = Journal::open(&path, 17).unwrap();
    assert!(matches!(
        log.append(action(4, 3, 3, 4, 2), &[]),
        Err(Error::Poisoned)
    ));
    let recovered = log.recover_from(8).unwrap();
    assert_eq!(recovered.len(), 3);
    assert_eq!(recovered[0].entries, values);
    assert_eq!(recovered[1].entries, replacement);
    assert_eq!(recovered[2].action, purge);
    assert_eq!(log.end(), end);
    drop(log);
    let mut log = Journal::open(&path, 17).unwrap();
    let recovered = log.recover_from(boundary).unwrap();
    assert_eq!(recovered.len(), 2);
    assert_eq!(recovered[0].action.generation, 2);
    let original = fs::read(&path).unwrap();
    // Neither a generation skip nor rollback below the retained floor writes.
    assert!(log.append(action(5, 3, 3, 4, 2), &[]).is_err());
    assert!(log.append(action(4, 3, 1, 1, 2), &[]).is_err());
    assert!(log.append(action(4, 3, 3, 4, 1), &[]).is_err());
    assert!(log
        .append(
            action(4, 3, 3, 4, 2),
            &[Change {
                version: 4,
                table: "a",
                key: b"bad",
                value: None
            }]
        )
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn physically_torn_last_transaction_is_removed_before_a_subsequent_append() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("0.log");
    let mut log = Journal::open(&path, 1).unwrap();
    log.append(action(1, 0, 0, 1, 0), &[]).unwrap();
    let boundary = log.end();
    log.append(
        action(2, 1, 1, 2, 0),
        &[Change {
            version: 1,
            table: "a",
            key: b"key",
            value: Some(b"payload"),
        }],
    )
    .unwrap();
    drop(log);
    let complete = fs::read(&path).unwrap();
    for missing in [1, 4, 11, 12, 20, complete.len() - boundary as usize - 1] {
        fs::write(&path, &complete[..complete.len() - missing]).unwrap();
        let mut log = Journal::open(&path, 1).unwrap();
        let recovered = log.recover_from(8).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(log.end(), boundary);
        log.append(action(2, 1, 1, 3, 0), &[]).unwrap();
        drop(log);
        let mut log = Journal::open(&path, 1).unwrap();
        let recovered = log.recover_from(8).unwrap();
        assert_eq!(recovered.last().unwrap().action.next, 3);
    }
}

#[test]
fn complete_frame_corruption_is_rejected_and_does_not_get_truncated_as_a_torn_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("0.log");
    let mut log = Journal::open(&path, 1).unwrap();
    let entries = log
        .append(
            action(1, 0, 0, 1, 0),
            &[Change {
                version: 0,
                table: "a",
                key: b"k",
                value: Some(b"payload"),
            }],
        )
        .unwrap();
    drop(log);
    let complete = fs::read(&path).unwrap();
    for offset in [
        8,
        16,
        24,
        entries[0].value.unwrap().offset as usize,
        complete.len() - 12,
        complete.len() - 1,
    ] {
        let mut bytes = complete.clone();
        bytes[offset] ^= 1;
        fs::write(&path, &bytes).unwrap();
        let mut log = Journal::open(&path, 1).unwrap();
        assert!(log.recover_from(8).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(matches!(log.recover_from(8), Err(Error::Poisoned)));
    }
    fs::write(&path, &complete).unwrap();
    let mut log = Journal::open(&path, 1).unwrap();
    log.recover_from(8).unwrap();
    let value = entries[0].value.unwrap();
    let mut damaged = complete.clone();
    damaged[value.offset as usize] ^= 1;
    fs::write(&path, &damaged).unwrap();
    assert!(matches!(
        log.reader().value(value),
        Err(Error::Corrupt("journal value checksum"))
    ));
}

#[test]
fn aggregate_replacement_is_streamed_beyond_the_old_64_mib_journal_limit() {
    // A single bounded value is reused as input, not 65 MiB of owned RAM copies.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("0.log");
    let payload = vec![42; 1024 * 1024];
    let keys = (0..65u32).map(u32::to_be_bytes).collect::<Vec<_>>();
    let changes = keys
        .iter()
        .map(|key| Change {
            version: 0,
            table: "large",
            key,
            value: Some(&payload),
        })
        .collect::<Vec<_>>();
    let mut log = Journal::open(&path, 1).unwrap();
    let entries = log.append(action(1, 0, 0, 1, 0), &changes).unwrap();
    assert!(log.end() > 64 * 1024 * 1024);
    let end = log.end();
    drop(log);
    let mut log = Journal::open(&path, 1).unwrap();
    let recovered = log.recover_from(8).unwrap();
    assert_eq!(recovered[0].entries, entries);
    assert_eq!(log.end(), end);
    assert_eq!(
        log.reader().value(entries[64].value.unwrap()).unwrap(),
        payload
    );
}
