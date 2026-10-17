//! Durable selected-chain state. Ledger effects, lossless block/transaction
//! records, height indexes and the tip commit together at the block's height.
//! This module verifies extensions; fork choice and orphan retention are separate.
pub mod genesis;
use crate::{
    block,
    transaction::{bytes, uint},
};
use mmx_consensus::{
    proof::{ProofError, ProofResult},
    transitions::history::HeaderSource,
    vdf::VerifiedPoint,
    Hash, Record,
};
use mmx_db::Write;
use mmx_types::{mmx, VnxType};
use mmx_vm::{storage::TransactionBatch, Address, StorageDb};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Input(#[from] mmx_consensus::Error),
    #[error(transparent)]
    Wire(#[from] vnx::Error),
    #[error(transparent)]
    Storage(#[from] mmx_vm::storage::Error),
    #[error(transparent)]
    Database(#[from] mmx_db::Error),
    #[error(transparent)]
    Validation(#[from] crate::transaction::ValidationError),
    #[error(transparent)]
    Block(#[from] block::Rejected),
    #[error(transparent)]
    Header(#[from] mmx_consensus::header::HeaderError),
}
pub type ChainResult<T> = Result<T, ChainError>;
fn invalid(message: &'static str) -> ChainError {
    ChainError::Invalid(message)
}

pub struct ChainState {
    storage: StorageDb,
    params: mmx::ChainParams,
}
impl ChainState {
    pub fn memory(params: mmx::ChainParams) -> Self {
        Self {
            storage: StorageDb::memory(),
            params,
        }
    }
    pub fn open(path: impl AsRef<Path>, params: mmx::ChainParams) -> ChainResult<Self> {
        let chain = Self {
            storage: StorageDb::open(path)?,
            params,
        };
        let db = chain.storage.database();
        if db.version() == 0 {
            if db.get("chain_meta", b"tip", None).is_some() {
                return Err(invalid("tip in an empty chain"));
            }
        } else {
            if db.get("chain_meta", b"params", None)
                != Some(chain.params.encode(&mmx_types::vnx_registry()?)?.as_slice())
            {
                return Err(invalid("chain parameter mismatch"));
            }
            let tip = chain.tip()?.ok_or_else(|| invalid("missing chain tip"))?;
            if tip.height.checked_add(1) != Some(db.version())
                || chain.hash_at(tip.height)? != Some(tip.hash.0)
            {
                return Err(invalid("chain tip/version mismatch"));
            }
        }
        Ok(chain)
    }
    /// Read-only ledger access. Use chain methods to publish/rewind state so
    /// callers cannot separate the tip/index commit from its ledger effects.
    pub fn storage(&self) -> &StorageDb {
        &self.storage
    }
    pub fn params(&self) -> &mmx::ChainParams {
        &self.params
    }
    pub fn tip(&self) -> ChainResult<Option<mmx::BlockHeader>> {
        let Some(hash) = self.storage.database().get("chain_meta", b"tip", None) else {
            return Ok(None);
        };
        self.header(&hash.try_into().map_err(|_| invalid("tip hash length"))?)?
            .map_or_else(|| Err(invalid("missing tip block")), |h| Ok(Some(h)))
    }
    pub fn hash_at(&self, height: u32) -> ChainResult<Option<Hash>> {
        self.storage
            .database()
            .get("chain_height", &height.to_be_bytes(), None)
            .map(|v| v.try_into().map_err(|_| invalid("height hash length")))
            .transpose()
    }
    pub fn block(&self, hash: &Hash) -> ChainResult<Option<Record>> {
        let Some(wire) = self.storage.database().get("chain_blocks", hash, None) else {
            return Ok(None);
        };
        let block = Record::decode(wire)?;
        if block.name != "mmx.Block" || bytes::<32>(block.field("hash")?)? != *hash {
            return Err(invalid("block archive key mismatch"));
        }
        Ok(Some(block))
    }
    pub fn header(&self, hash: &Hash) -> ChainResult<Option<mmx::BlockHeader>> {
        self.storage
            .database()
            .get("chain_headers", hash, None)
            .map(|wire| {
                let header = mmx::BlockHeader::decode(wire)?;
                if header.hash.0 != *hash {
                    return Err(invalid("header archive key mismatch"));
                }
                Ok(header)
            })
            .transpose()
    }
    pub fn transaction(&self, id: &Address) -> ChainResult<Option<Record>> {
        self.storage
            .database()
            .get("chain_transactions", id, None)
            .map(|wire| {
                let tx = Record::decode(wire)?;
                if tx.name != "mmx.Transaction" || bytes::<32>(tx.field("id")?)? != *id {
                    return Err(invalid("transaction archive key mismatch"));
                }
                Ok(tx)
            })
            .transpose()
    }
    pub fn initialize(&mut self, assets: &genesis::Assets) -> ChainResult<block::Receipt> {
        if self.storage.database().version() != 0 || self.tip()?.is_some() {
            return Err(invalid("chain already initialized"));
        }
        let block = genesis::build(&self.params, assets)?;
        let (receipt, batch) = genesis::effects(&self.storage, &self.params, &block)?;
        self.publish(&block, &batch, true)?;
        Ok(receipt)
    }
    pub fn append(
        &mut self,
        candidate: &Record,
        points: &[VerifiedPoint],
    ) -> ChainResult<block::Receipt> {
        let previous = self
            .tip()?
            .ok_or_else(|| invalid("chain is not initialized"))?;
        if previous.height.checked_add(1) != Some(self.storage.database().version()) {
            return Err(invalid("chain tip/version mismatch"));
        }
        let expected_vdf_payout = self.vdf_payout(uint(candidate, "height")? as u32, &previous)?;
        let context = block::BlockContext {
            params: self.params.clone(),
            previous,
            expected_vdf_payout,
        };
        block::verify_with_vdf(&context, candidate, self, points)?;
        let (receipt, batch) = block::prepare_batch(&self.storage, &context, candidate)?;
        self.publish(candidate, &batch, false)?;
        Ok(receipt)
    }
    fn vdf_payout(&self, height: u32, parent: &mmx::BlockHeader) -> ChainResult<Option<Address>> {
        let interval = self.params.vdf_reward_interval;
        if interval == 0 {
            return Err(invalid("zero VDF reward interval"));
        }
        if !height.is_multiple_of(interval) {
            return Ok(None);
        }
        let mut ancestors = Vec::new();
        let mut current = parent.clone();
        for _ in 0..interval {
            ancestors.push(current.clone());
            if current.height == 0 {
                break;
            }
            current = self
                .header(&current.prev.0)?
                .ok_or_else(|| invalid("missing payout ancestor"))?;
        }
        Ok(block::economics::vdf_reward_winner(interval, &ancestors))
    }
    fn publish(
        &mut self,
        block: &Record,
        batch: &TransactionBatch,
        genesis: bool,
    ) -> ChainResult<()> {
        let height = uint(block, "height")? as u32;
        let next = height
            .checked_add(1)
            .filter(|v| *v != u32::MAX)
            .ok_or_else(|| invalid("chain height limit"))?;
        if self.storage.database().version() != height || self.storage.database().has_pending() {
            return Err(invalid(
                "chain snapshot has pending writes or wrong version",
            ));
        }
        let hash = bytes::<32>(block.field("hash")?)?;
        // Slice without rehashing: native Block and Header have distinct type
        // identities, and authenticated ancestry retains the original Block ID.
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
            Write::put("chain_blocks", hash, block.encode()?),
            Write::put("chain_headers", hash, header.encode()?),
            Write::put("chain_height", height.to_be_bytes(), hash),
            Write::put("chain_meta", b"tip", hash),
        ];
        if genesis {
            writes.push(Write::put(
                "chain_meta",
                b"params",
                self.params.encode(&mmx_types::vnx_registry()?)?,
            ));
        }
        for tx in block.field("tx_list")?.seq()? {
            let tx = tx.record()?;
            writes.push(Write::put(
                "chain_transactions",
                bytes::<32>(tx.field("id")?)?,
                tx.encode()?,
            ));
        }
        self.storage.commit_transaction(batch, writes, next)?;
        Ok(())
    }
    /// Remove blocks and ledger effects at heights >= `height`, matching native
    /// Node::revert. Zero clears genesis too; a replacement branch can then be
    /// appended explicitly. Orphan archives/fork choice are not maintained here.
    pub fn revert(&mut self, height: u32) -> ChainResult<()> {
        self.storage.database_mut().revert(height)?;
        Ok(())
    }
}
impl HeaderSource for ChainState {
    fn get_header(&self, hash: &Hash) -> ProofResult<Option<mmx::BlockHeader>> {
        self.header(hash)
            .map_err(|e| ProofError::Invalid(e.to_string()))
    }
}
