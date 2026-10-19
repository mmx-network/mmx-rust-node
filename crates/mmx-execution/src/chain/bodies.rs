//! One immutable header/transaction payload per block, with point-read locators.
//! Archive publication precedes selected ledger/index publication. Snapshots pin
//! the archive's native files, including across purge and branch replacement.
use super::{block_header, codec, invalid, ChainResult};
use crate::transaction::bytes;
use mmx_consensus::{Hash, Record, Value};
use mmx_db::{Store, Write};
use mmx_types::mmx;

const SPLIT: &[u8; 8] = b"MMXB\x01\0\0\0";
pub(super) const EXTERNAL: &[u8; 8] = b"MMXL\x01\0\0\0";
const TRANSACTION: &[u8; 8] = b"MMXT\x01\0\0\0";
#[derive(Clone, Copy)]
pub(super) struct Tables {
    pub blocks: &'static str,
    pub headers: &'static str,
    pub transactions: &'static str,
}
pub(super) const ARCHIVE: Tables = Tables {
    blocks: "fork_blocks",
    headers: "fork_headers",
    transactions: "fork_transactions",
};
pub(super) const SELECTED: Tables = Tables {
    blocks: "chain_blocks",
    headers: "chain_headers",
    transactions: "chain_body_transactions",
};
fn key(hash: &Hash, index: u32) -> [u8; 36] {
    let mut key = [0; 36];
    key[..32].copy_from_slice(hash);
    key[32..].copy_from_slice(&index.to_be_bytes());
    key
}
pub(super) fn locator(hash: &Hash, index: u32) -> Vec<u8> {
    [TRANSACTION.as_slice(), &key(hash, index)].concat()
}
pub(super) fn transaction_location(value: &[u8]) -> ChainResult<Option<(Hash, u32)>> {
    if !value.starts_with(b"MMXT") {
        return Ok(None);
    }
    if value.len() != 44 || !value.starts_with(TRANSACTION) {
        return Err(invalid("invalid transaction locator"));
    }
    Ok(Some((
        value[8..40].try_into().unwrap(),
        u32::from_be_bytes(value[40..].try_into().unwrap()),
    )))
}
fn count(value: &[u8]) -> ChainResult<Option<u32>> {
    if !value.starts_with(b"MMXB") {
        return Ok(None);
    }
    if value.len() != 12 || !value.starts_with(SPLIT) {
        return Err(invalid("invalid split block descriptor"));
    }
    Ok(Some(u32::from_le_bytes(value[8..].try_into().unwrap())))
}
pub(super) fn writes(block: &Record, tables: Tables) -> ChainResult<Vec<Write>> {
    let hash = bytes::<32>(block.field("hash")?)?;
    let transactions = block.field("tx_list")?.seq()?;
    let n = u32::try_from(transactions.len()).map_err(|_| invalid("block transaction count"))?;
    let header = Record {
        name: "mmx.BlockHeader".into(),
        fields: block
            .fields
            .iter()
            .filter(|(name, _)| name.as_str() != "tx_list")
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    };
    let mut writes = vec![
        Write::put(
            tables.blocks,
            hash,
            [SPLIT.as_slice(), &n.to_le_bytes()].concat(),
        ),
        Write::put(tables.headers, hash, codec::encode(&header)?),
    ];
    for (index, tx) in transactions.iter().enumerate() {
        writes.push(Write::put(
            tables.transactions,
            key(&hash, index as u32),
            codec::encode(tx.record()?)?,
        ));
    }
    Ok(writes)
}
fn stored_header(db: &Store, tables: Tables, hash: &Hash) -> ChainResult<Option<Record>> {
    let Some(wire) = db.get(tables.headers, hash, None)? else {
        return Ok(None);
    };
    let header = codec::decode(&wire)?;
    if header.name != "mmx.BlockHeader" || bytes::<32>(header.field("hash")?)? != *hash {
        return Err(invalid("header archive key mismatch"));
    }
    Ok(Some(header))
}
pub(super) fn header_record(
    db: &Store,
    tables: Tables,
    hash: &Hash,
) -> ChainResult<Option<Record>> {
    if let Some(header) = stored_header(db, tables, hash)? {
        return Ok(Some(header));
    }
    // Older native records contain the complete body. Read in place, without
    // converting history or introducing payload writes during an RPC.
    Ok(block(db, tables, hash)?.map(|mut block| {
        block.name = "mmx.BlockHeader".into();
        block.fields.remove("tx_list");
        block
    }))
}
pub(super) fn header(
    db: &Store,
    tables: Tables,
    hash: &Hash,
) -> ChainResult<Option<mmx::BlockHeader>> {
    header_record(db, tables, hash)?
        .as_ref()
        .map(block_header)
        .transpose()
}
pub(super) fn block(db: &Store, tables: Tables, hash: &Hash) -> ChainResult<Option<Record>> {
    let Some(wire) = db.get(tables.blocks, hash, None)? else {
        return Ok(None);
    };
    let block = if let Some(n) = count(&wire)? {
        let mut header =
            stored_header(db, tables, hash)?.ok_or_else(|| invalid("missing block header"))?;
        if header.field("tx_count")?.uint()? != u64::from(n) {
            return Err(invalid("block descriptor transaction count mismatch"));
        }
        // Do not allocate from an untrusted/corrupt count. Each iteration first
        // reads an independently checksummed, bounded native value.
        let mut transactions = Vec::new();
        for index in 0..n {
            let tx = db
                .get(tables.transactions, &key(hash, index), None)?
                .ok_or_else(|| invalid("missing block transaction"))?;
            transactions.push(Value::Record(codec::decode(&tx)?));
        }
        header.name = "mmx.Block".into();
        header
            .fields
            .insert("tx_list".into(), Value::Sequence(transactions));
        header
    } else {
        codec::decode(&wire)?
    };
    if block.name != "mmx.Block" || bytes::<32>(block.field("hash")?)? != *hash {
        return Err(invalid("block archive key mismatch"));
    }
    Ok(Some(block))
}
pub(super) fn transaction(
    db: &Store,
    tables: Tables,
    hash: &Hash,
    index: u32,
) -> ChainResult<Record> {
    let wire = db
        .get(tables.blocks, hash, None)?
        .ok_or_else(|| invalid("missing transaction block"))?;
    if let Some(n) = count(&wire)? {
        if index >= n {
            return Err(invalid("transaction locator index out of bounds"));
        }
        let wire = db
            .get(tables.transactions, &key(hash, index), None)?
            .ok_or_else(|| invalid("missing indexed transaction"))?;
        Ok(codec::decode(&wire)?)
    } else {
        let block = codec::decode(&wire)?;
        Ok(block
            .field("tx_list")?
            .seq()?
            .get(index as usize)
            .ok_or_else(|| invalid("transaction locator index out of bounds"))?
            .record()?
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_payload_write_and_independent_header_transaction_reads() {
        let mut txs = Vec::new();
        for i in 0..2u8 {
            let mut tx = Record::from_generated(&mmx::Transaction::default()).unwrap();
            tx.fields.insert("id".into(), Value::Bytes(vec![i; 32]));
            let mut output = Record::from_generated(&mmx::txout_t::default()).unwrap();
            output.fields.insert(
                "memo".into(),
                Value::Optional(Some(Box::new(Value::String(vec![b'x'; 65536])))),
            );
            tx.fields.insert(
                "outputs".into(),
                Value::Sequence(vec![Value::Record(output)]),
            );
            txs.push(tx);
        }
        let mut block = Record::from_generated(&mmx::Block {
            tx_count: 2,
            ..Default::default()
        })
        .unwrap();
        block.fields.insert(
            "tx_list".into(),
            Value::Sequence(txs.iter().cloned().map(Value::Record).collect()),
        );
        let hash = bytes::<32>(block.field("hash").unwrap()).unwrap();
        let old_dir = tempfile::tempdir().unwrap();
        let new_dir = tempfile::tempdir().unwrap();
        let mut old = Store::open_native(old_dir.path()).unwrap();
        let mut new = Store::open_native(new_dir.path()).unwrap();
        old.commit_batch(
            1,
            [
                Write::put(ARCHIVE.blocks, hash, codec::encode(&block).unwrap()),
                Write::put(SELECTED.blocks, hash, codec::encode(&block).unwrap()),
                Write::put(
                    "chain_transactions",
                    [0; 32],
                    codec::encode(&txs[0]).unwrap(),
                ),
                Write::put(
                    "chain_transactions",
                    [1; 32],
                    codec::encode(&txs[1]).unwrap(),
                ),
            ],
        )
        .unwrap();
        let mut writes = writes(&block, ARCHIVE).unwrap();
        writes.push(Write::put(SELECTED.blocks, hash, EXTERNAL));
        for (i, tx) in txs.iter().enumerate() {
            writes.push(Write::put(
                "chain_transactions",
                tx.field("id").unwrap().bytes().unwrap(),
                locator(&hash, i as u32),
            ));
        }
        new.commit_batch(1, writes).unwrap();
        let log_size = |path: &std::path::Path| {
            std::fs::metadata(path.join("native-data-0000000000000000.log"))
                .unwrap()
                .len()
        };
        let old_bytes = log_size(old_dir.path());
        let new_bytes = log_size(new_dir.path());
        eprintln!("body fixture journal bytes: old={old_bytes}, new={new_bytes}");
        assert!(new_bytes * 2 < old_bytes);
        assert_eq!(
            super::block(&new, ARCHIVE, &hash).unwrap().as_ref(),
            Some(&block)
        );
        assert_eq!(transaction(&new, ARCHIVE, &hash, 0).unwrap(), txs[0]);
        // An unrelated damaged transaction cannot force a header/point lookup
        // through the complete block, as the former combined payload did.
        new.commit_batch(
            2,
            [Write::put(ARCHIVE.transactions, key(&hash, 1), b"damaged")],
        )
        .unwrap();
        assert!(header(&new, ARCHIVE, &hash).unwrap().is_some());
        assert_eq!(transaction(&new, ARCHIVE, &hash, 0).unwrap(), txs[0]);
        assert!(super::block(&new, ARCHIVE, &hash).is_err());
        assert!(transaction(&new, ARCHIVE, &hash, 2).is_err());
        assert!(transaction_location(b"MMXT\x02\0\0\0").is_err());
    }
}
