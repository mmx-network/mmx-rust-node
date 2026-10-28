use super::*;
use crate::rpc::{from_json_record, tests::fixture};
use mmx_db::Write;
use vnx::serde_json::json;

fn query(address: Hash) -> Record {
    from_json_record(&json!({"__type":"mmx.Node.get_history","addresses":[address],"filter":{"since":0,"until":u32::MAX,"limit":-1,"max_search":0,"currency":[],"type":null,"memo":null,"white_list":false,"with_pending":false}})).unwrap()
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn history_ranges_page_past_one_hundred_rows_and_preserve_native_memo_and_currency_filters() {
    let (node, _, _) = fixture();
    let n = node.lock().unwrap();
    let owner = [7; 32];
    let mut writes = Vec::new();
    let mut wanted = 0;
    for ordinal in 0..250u32 {
        let height = 300 - ordinal / 2;
        let memo = if ordinal % 3 == 0 {
            Some("memo".to_string())
        } else {
            None
        };
        let currency = if ordinal % 2 == 0 { [1; 32] } else { [0; 32] };
        if memo.is_some() && currency == [1; 32] {
            wanted += 1;
        }
        let row = Record::from_generated(&mmx::tx_entry_t {
            address: owner.into(),
            contract: currency.into(),
            height,
            memo: memo.clone(),
            r#type: mmx::tx_type_e::RECEIVE,
            ..Default::default()
        })
        .unwrap();
        let suffix = [(!height).to_be_bytes(), ordinal.to_be_bytes()].concat();
        let key = [owner.as_slice(), &suffix].concat();
        writes.push(Write::put(
            index::TABLE,
            key.clone(),
            row.encode_compact().unwrap(),
        ));
        if let Some(memo) = memo {
            writes.push(Write::put(
                index::MEMO_TABLE,
                [index::prefix(&owner, Some(memo.as_bytes())), suffix].concat(),
                key,
            ));
        }
    }
    let mut db = mmx_db::Store::memory();
    db.commit_batch(301, writes).unwrap();
    let task = Task {
        storage: StorageDb::new(db),
        blocks: None,
        params: n.chain.selected().params().clone(),
        pending: vec![],
        start: 0,
        floor: 0,
        _permit: super::super::ReadPermit::acquire(&n.rpc_workers).unwrap(),
    };
    let mut q = query(owner);
    assert_eq!(
        task.call(&q, deadline())
            .unwrap()
            .field("_ret_0")
            .unwrap()
            .seq()
            .unwrap()
            .len(),
        250
    );
    let Value::Record(filter) = q.fields.get_mut("filter").unwrap() else {
        panic!()
    };
    filter.fields.insert("limit".into(), Value::Int(101));
    assert_eq!(
        task.call(&q, deadline())
            .unwrap()
            .field("_ret_0")
            .unwrap()
            .seq()
            .unwrap()
            .len(),
        102
    );
    let Value::Record(filter) = q.fields.get_mut("filter").unwrap() else {
        panic!()
    };
    filter.fields.insert("limit".into(), Value::Int(-1));
    filter.fields.insert(
        "memo".into(),
        Value::Optional(Some(Box::new(Value::String(b"memo".to_vec())))),
    );
    filter.fields.insert(
        "currency".into(),
        Value::Sequence(vec![Value::Bytes(vec![1; 32])]),
    );
    assert_eq!(
        task.call(&q, deadline())
            .unwrap()
            .field("_ret_0")
            .unwrap()
            .seq()
            .unwrap()
            .len(),
        wanted
    );
    assert!(task
        .call(&q, Instant::now())
        .unwrap_err()
        .to_string()
        .contains("deadline"));
}

#[test]
fn legacy_history_uses_a_bounded_snapshot_fallback_without_backfilling_storage() {
    let (node, block, vdf) = fixture();
    let mut n = node.lock().unwrap();
    n.chain.admit(&block, &[vdf]).unwrap();
    let root = n.chain.selected().hash_at(0).unwrap().unwrap();
    n.chain.update(root, None).unwrap();
    let owner = hash(
        &index::entries(&block, n.chain.selected().params()).unwrap()[0],
        "address",
    )
    .unwrap();
    let q = query(owner);
    let expected = response(&n, &q).unwrap();
    let mut db = mmx_db::Store::memory();
    let writes = (0..=1u32).map(|h| {
        Write::put(
            "chain_height",
            h.to_be_bytes(),
            n.chain.selected().hash_at(h).unwrap().unwrap(),
        )
    });
    db.commit_batch(2, writes).unwrap();
    let mut task = Task {
        storage: StorageDb::new(db),
        blocks: Some(n.chain.block_snapshot().unwrap()),
        params: n.chain.selected().params().clone(),
        pending: vec![],
        start: 2,
        floor: 0,
        _permit: super::super::ReadPermit::acquire(&n.rpc_workers).unwrap(),
    };
    assert_eq!(task.call(&q, deadline()).unwrap(), expected);
    assert!(task
        .storage
        .database()
        .scan(index::TABLE, &[], None)
        .unwrap()
        .is_empty());
    task.floor = 1;
    for row in task
        .call(&q, deadline())
        .unwrap()
        .field("_ret_0")
        .unwrap()
        .seq()
        .unwrap()
    {
        assert_eq!(number(row.record().unwrap(), "height").unwrap(), 1);
    }
}
