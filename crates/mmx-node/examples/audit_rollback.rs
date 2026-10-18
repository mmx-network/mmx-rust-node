//! Offline large-state rewind/replay audit. Source files are opened read-only;
//! every mutation happens in an automatically removed, isolated database copy.
use mmx_consensus::{Hash, Record};
use mmx_execution::chain::{network::Network, ChainState};
use mmx_node::{Node, Result};
use rusqlite::{backup::Backup, Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use vnx::serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    rows: u64,
    hash: Hash,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    history: Stamp,
    visible: Stamp,
    ledger_history: Stamp,
    ledger_visible: Stamp,
}
#[derive(Default)]
struct Fingerprint {
    rows: u64,
    hash: Sha256,
}
impl Fingerprint {
    fn add(&mut self, hash: &Hash) {
        self.rows += 1;
        self.hash.update(hash);
    }
    fn finish(self) -> Stamp {
        Stamp {
            rows: self.rows,
            hash: self.hash.finalize().into(),
        }
    }
}
#[derive(Default)]
struct View {
    history: Fingerprint,
    visible: Fingerprint,
    ledger_history: Fingerprint,
    ledger_visible: Fingerprint,
    latest: Option<Hash>,
}
fn readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    Ok(conn)
}
fn backup(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination.parent().ok_or("missing backup parent")?)?;
    let from = readonly(source)?;
    // Pin a coherent source view even if the primary node continues writing.
    from.execute_batch("BEGIN DEFERRED")?;
    from.query_row("SELECT version FROM meta", [], |row| row.get::<_, u32>(0))?;
    let mut to = Connection::open(destination)?;
    to.busy_timeout(Duration::from_secs(30))?;
    Backup::new(&from, &mut to)?.run_to_completion(1024, Duration::from_millis(1), None)?;
    Ok(())
}
fn snapshot(source: &Path, destination: &Path) -> Result<()> {
    for name in ["selected", "archive"] {
        let started = Instant::now();
        backup(
            &source.join("chain").join(name).join("state.sqlite"),
            &destination.join("chain").join(name).join("state.sqlite"),
        )?;
        println!(
            "{}",
            json!({"phase":"snapshot","database":name,"seconds":started.elapsed().as_secs_f64()})
        );
    }
    Ok(())
}
fn ledger(table: &str) -> bool {
    // Stored record encodings and redundant legacy header rows may change
    // during replay. Ledger, height/transaction indexes and tip must not.
    !matches!(
        table,
        "chain_blocks" | "chain_headers" | "chain_transactions"
    )
}
fn flush(views: &mut BTreeMap<u32, View>, is_ledger: bool) {
    for view in views.values_mut() {
        if let Some(hash) = view.latest.take() {
            view.visible.add(&hash);
            if is_ledger {
                view.ledger_visible.add(&hash);
            }
        }
    }
}
fn states(conn: &Connection, heights: &[u32]) -> Result<BTreeMap<u32, State>> {
    let mut views: BTreeMap<_, _> = heights.iter().map(|h| (*h, View::default())).collect();
    let mut statement = conn.prepare("SELECT t,n,k,v,value FROM history ORDER BY t,n,k,v")?;
    let mut rows = statement.query([])?;
    let mut current = None;
    let mut is_ledger = false;
    while let Some(row) = rows.next()? {
        let table: String = row.get(0)?;
        let size: u64 = row.get(1)?;
        let key: Vec<u8> = row.get(2)?;
        let version: u32 = row.get(3)?;
        let value: Option<Vec<u8>> = row.get(4)?;
        let identity = (table, size, key);
        if current.as_ref() != Some(&identity) {
            flush(&mut views, is_ledger);
            is_ledger = ledger(&identity.0);
            current = Some(identity);
        }
        let (table, size, key) = current.as_ref().ok_or("missing history key")?;
        let mut hash = Sha256::new();
        hash.update((table.len() as u64).to_le_bytes());
        hash.update(table.as_bytes());
        hash.update(size.to_le_bytes());
        hash.update((key.len() as u64).to_le_bytes());
        hash.update(key);
        hash.update(
            value
                .as_ref()
                .map_or(u64::MAX, |v| v.len() as u64)
                .to_le_bytes(),
        );
        if let Some(value) = &value {
            hash.update(value);
        }
        let visible: Hash = hash.finalize().into();
        let historical: Hash =
            Sha256::digest([visible.as_slice(), &version.to_le_bytes()].concat()).into();
        for (_, view) in views.range_mut(version..) {
            view.history.add(&historical);
            if is_ledger {
                view.ledger_history.add(&historical);
            }
            view.latest = value.as_ref().map(|_| visible);
        }
    }
    flush(&mut views, is_ledger);
    Ok(views
        .into_iter()
        .map(|(height, v)| {
            (
                height,
                State {
                    history: v.history.finish(),
                    visible: v.visible.finish(),
                    ledger_history: v.ledger_history.finish(),
                    ledger_visible: v.ledger_visible.finish(),
                },
            )
        })
        .collect())
}
fn current(path: &Path, height: u32) -> Result<State> {
    let conn = readonly(path)?;
    let version: u32 = conn.query_row("SELECT version FROM meta", [], |row| row.get(0))?;
    if version != height + 1 {
        return Err("rewind left the wrong version".into());
    }
    let future: u64 = conn.query_row(
        "SELECT count(*) FROM history WHERE v>=?1",
        [version],
        |row| row.get(0),
    )?;
    if future != 0 {
        return Err("rewind retained future versions".into());
    }
    // Inspect all rows, not just v<=height, so undeleted future state cannot hide.
    Ok(states(&conn, &[u32::MAX])?
        .remove(&u32::MAX)
        .ok_or("missing current digest")?)
}
fn selected_hash(conn: &Connection, height: u32) -> Result<Hash> {
    let bytes: Vec<u8> = conn.query_row(
        "SELECT value FROM history WHERE t='chain_height' AND k=?1 ORDER BY v DESC LIMIT 1",
        [height.to_be_bytes()],
        |row| row.get(0),
    )?;
    Ok(bytes.try_into().map_err(|_| "invalid height hash")?)
}
fn block(reference: &Connection, archive: &Connection, height: u32) -> Result<Record> {
    let id = selected_hash(reference, height)?;
    let bytes: Vec<u8> = archive.query_row(
        "SELECT value FROM history WHERE t='fork_blocks' AND k=?1 ORDER BY v DESC LIMIT 1",
        [id],
        |row| row.get(0),
    )?;
    Ok(
        if let Some(payload) = bytes.strip_prefix(b"MMXR\x01\0\0\0") {
            Record::decode_compact(payload)?
        } else {
            Record::decode(&bytes)?
        },
    )
}
fn replay(
    data: &Path,
    reference: &Connection,
    network: &Network,
    from: u32,
    to: u32,
) -> Result<()> {
    let started = Instant::now();
    let mut chain = ChainState::open_indexed(data.join("chain/selected"), network.params.clone())?;
    let archive = readonly(&data.join("chain/archive/state.sqlite"))?;
    for height in from + 1..=to {
        // Offline audit of already archived canonical blocks. Full CPU space,
        // authentication, transitions, body and ledger checks run again; native
        // historical policy alone omits VDF linkage. No peer download occurs.
        chain.append_sync(&block(reference, &archive, height)?)?;
    }
    if chain.hash_at(to)? != Some(selected_hash(reference, to)?) {
        return Err("replay hash mismatch".into());
    }
    println!(
        "{}",
        json!({"phase":"replay","from":from,"to":to,"seconds":started.elapsed().as_secs_f64(),"network_downloads":0})
    );
    Ok(())
}
fn main() -> Result<()> {
    let mut source = None;
    let mut output = None;
    let mut root = PathBuf::from("../mmx-node");
    let mut name = "mainnet".to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("missing audit argument")?;
        match arg.as_str() {
            "--source-dir" => source = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--mmx-root" => root = value.into(),
            "--network" => name = value,
            _ => return Err(format!("unknown audit argument: {arg}").into()),
        }
    }
    let network = Network::load(root, &name)?;
    let output = output.ok_or("missing --output")?;
    std::fs::create_dir_all(&output)?;
    let temporary = tempfile::Builder::new()
        .prefix("rollback-audit-")
        .tempdir_in(output)?;
    println!("{}", json!({"phase":"start","directory":temporary.path()}));
    let reference = temporary.path().join("reference");
    let data = temporary.path().join("working");
    snapshot(&source.ok_or("missing --source-dir")?, &reference)?;
    snapshot(&reference, &data)?;
    for database in ["selected", "archive"] {
        let started = Instant::now();
        let mut store = mmx_db::Store::open_indexed(data.join("chain").join(database))?;
        let changed = store.optimize_layout()?;
        println!(
            "{}",
            json!({"phase":"layout","database":database,"rewritten":changed,"seconds":started.elapsed().as_secs_f64()})
        );
    }
    let selected = data.join("chain/selected/state.sqlite");
    let rowid_original = temporary.path().join("selected.rowid.sqlite");
    backup(&selected, &rowid_original)?;
    let expected = readonly(&reference.join("chain/selected/state.sqlite"))?;
    let tip: u32 = expected.query_row("SELECT version-1 FROM meta", [], |row| row.get(0))?;
    if tip == 0 {
        return Err("audit requires a non-genesis source prefix".into());
    }
    let first = tip.min(128);
    let mut heights = vec![0, 1.min(tip), first, tip.saturating_sub(1), tip];
    let bridges: Vec<_> = [
        network.params.hardfork1_height,
        network.params.hardfork2_height,
    ]
    .into_iter()
    .filter(|&h| h > 0 && h.checked_add(2).is_some_and(|end| end <= tip))
    .collect();
    for height in &bridges {
        heights.extend([height - 1, *height, height + 1, height + 2]);
    }
    heights.sort_unstable();
    heights.dedup();
    println!(
        "{}",
        json!({"phase":"reference","tip":tip,"heights":heights})
    );
    let reference_states = states(&expected, &heights)?;
    let archive = data.join("chain/archive/state.sqlite");
    let archive_before = states(&readonly(&archive)?, &[u32::MAX])?;
    let archive_epoch =
        readonly(&archive)?.query_row("SELECT version,generation FROM meta", [], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, u64>(1)?))
        })?;
    let mut node = Node::open(&data, &network)?;
    for &height in heights.iter().rev() {
        let started = Instant::now();
        println!("{}", json!({"phase":"rewind_start","height":height}));
        node.rollback_to(height)?;
        node.tick()?;
        node.tick()?;
        if node.chain().selected().hash_at(height)? != Some(selected_hash(&expected, height)?) {
            return Err("rewind changed the selected prefix".into());
        }
        if node
            .chain()
            .selected()
            .tip()?
            .ok_or("missing rewound tip")?
            .height
            != height
        {
            return Err("offline tick replayed retained future blocks".into());
        }
        let actual = current(&selected, height)?;
        if actual != reference_states[&height] {
            return Err(format!("rewind state/history mismatch at {height}: {actual:?}").into());
        }
        println!(
            "{}",
            json!({"phase":"rewind_verified","height":height,"history_rows":actual.history.rows,"visible_rows":actual.visible.rows,"seconds":started.elapsed().as_secs_f64()})
        );
    }
    drop(node);
    replay(&data, &expected, &network, 0, first)?;
    let actual = current(&selected, first)?;
    if actual.ledger_history != reference_states[&first].ledger_history
        || actual.ledger_visible != reference_states[&first].ledger_visible
    {
        return Err("genesis replay ledger/index mismatch".into());
    }
    let reopened = Node::open(&data, &network)?;
    if reopened.chain().selected().hash_at(first)? != Some(selected_hash(&expected, first)?) {
        return Err("reopened replay hash mismatch".into());
    }
    drop(reopened);
    for &height in &bridges {
        backup(&rowid_original, &selected)?;
        let mut node = Node::open(&data, &network)?;
        node.rollback_to(height - 1)?;
        drop(node);
        replay(&data, &expected, &network, height - 1, height + 2)?;
        let actual = current(&selected, height + 2)?;
        if actual.ledger_history != reference_states[&(height + 2)].ledger_history
            || actual.ledger_visible != reference_states[&(height + 2)].ledger_visible
        {
            return Err(format!("hardfork replay ledger/index mismatch at {height}").into());
        }
    }
    if states(&readonly(&archive)?, &[u32::MAX])? != archive_before {
        return Err("rollback altered retained archive history".into());
    }
    let archive_after =
        readonly(&archive)?.query_row("SELECT version,generation FROM meta", [], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, u64>(1)?))
        })?;
    if archive_after != archive_epoch {
        return Err("rollback changed the archive publication epoch".into());
    }
    println!(
        "{}",
        json!({"phase":"complete","tip":tip,"rewinds":heights.len(),
        "all_retained_history_matches":true,"genesis_replay_matches":true,"genesis_replay_height":first,
        "hardforks_checked":bridges,"hardfork_replay_matches":if bridges.is_empty() { None } else { Some(true) },
        "archive_unchanged":true,"network_downloads":0})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE meta(version INTEGER NOT NULL);
             INSERT INTO meta VALUES(3);
             CREATE TABLE history(t TEXT NOT NULL,n INTEGER NOT NULL,k BLOB NOT NULL,
                v INTEGER NOT NULL,value BLOB,PRIMARY KEY(t,n,k,v));
             INSERT INTO history VALUES('balances',1,x'61',0,x'01');
             INSERT INTO history VALUES('balances',1,x'61',1,x'02');
             INSERT INTO history VALUES('balances',1,x'61',2,x'03');
             INSERT INTO history VALUES('balances',1,x'62',0,x'04');
             INSERT INTO history VALUES('balances',1,x'62',1,NULL);
             INSERT INTO history VALUES('balances',1,x'63',0,x'');
             INSERT INTO history VALUES('chain_blocks',1,x'64',0,x'05');",
        )
        .unwrap();
    }

    #[test]
    fn detects_lost_history_tombstones_empty_values_and_ledger_corruption() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        let before = states(&conn, &[0, 1, 2]).unwrap();
        assert_eq!(
            [
                before[&0].history.rows,
                before[&1].history.rows,
                before[&2].history.rows
            ],
            [4, 6, 7]
        );
        assert_eq!(
            [
                before[&0].visible.rows,
                before[&1].visible.rows,
                before[&2].visible.rows
            ],
            [4, 3, 3]
        );
        conn.execute(
            "DELETE FROM history WHERE t='balances' AND k=x'61' AND v=0",
            [],
        )
        .unwrap();
        let lost = states(&conn, &[2]).unwrap();
        assert_eq!(lost[&2].visible, before[&2].visible);
        assert_ne!(
            lost[&2].history, before[&2].history,
            "matching current values must not hide discarded older versions"
        );
        conn.execute("UPDATE history SET value=x'06' WHERE t='chain_blocks'", [])
            .unwrap();
        let encoding = states(&conn, &[2]).unwrap();
        assert_ne!(encoding[&2].visible, lost[&2].visible);
        assert_eq!(encoding[&2].ledger_visible, lost[&2].ledger_visible);
        assert_eq!(encoding[&2].ledger_history, lost[&2].ledger_history);
        conn.execute(
            "UPDATE history SET value=x'07' WHERE t='balances' AND k=x'61' AND v=2",
            [],
        )
        .unwrap();
        let corrupt = states(&conn, &[2]).unwrap();
        assert_ne!(corrupt[&2].ledger_visible, encoding[&2].ledger_visible);
        assert_ne!(corrupt[&2].ledger_history, encoding[&2].ledger_history);
    }

    #[test]
    fn rejects_future_rows_and_wrong_metadata_instead_of_filtering_them_away() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        conn.execute("UPDATE meta SET version=1", []).unwrap();
        let expected = states(&conn, &[0]).unwrap().remove(&0).unwrap();
        assert!(current(&path, 0)
            .unwrap_err()
            .to_string()
            .contains("future versions"));
        conn.execute("DELETE FROM history WHERE v>=1", []).unwrap();
        assert_eq!(current(&path, 0).unwrap(), expected);
        conn.execute("UPDATE meta SET version=2", []).unwrap();
        assert!(current(&path, 0)
            .unwrap_err()
            .to_string()
            .contains("wrong version"));
    }
}
