//! Durable selected-chain state. Ledger effects, lossless block/transaction
//! records, height indexes and the tip commit together at the block's height.
//! ChainState verifies extensions/replacements; `forks` adds orphan retention
//! and branch selection using the native policy in `choice`.
mod cache;
pub mod choice;
mod codec;
pub mod forks;
pub mod genesis;
pub mod network;
pub mod sync;
pub mod voting;
mod work;
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
use std::{path::Path, sync::Arc, time::Instant};

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
    #[error(transparent)]
    Vdf(#[from] mmx_consensus::vdf::VdfError),
    #[error(transparent)]
    Vote(#[from] mmx_consensus::vote::VoteError),
    #[error(transparent)]
    Proof(#[from] mmx_consensus::proof::ProofError),
}
pub type ChainResult<T> = Result<T, ChainError>;
/// Native Node skips VDF verification only while fetching historical blocks.
/// Both modes verify space work, authentication, transitions and full execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationMode {
    Live,
    HistoricalSync,
}
fn invalid(message: &'static str) -> ChainError {
    ChainError::Invalid(message)
}

pub struct ChainState {
    storage: StorageDb,
    params: mmx::ChainParams,
    headers: cache::Headers,
}
impl ChainState {
    pub fn memory(params: mmx::ChainParams) -> Self {
        Self {
            storage: StorageDb::memory(),
            params,
            headers: Default::default(),
        }
    }
    pub fn open(path: impl AsRef<Path>, params: mmx::ChainParams) -> ChainResult<Self> {
        Self::with_storage(StorageDb::open(path)?, params)
    }
    pub fn open_native(path: impl AsRef<Path>, params: mmx::ChainParams) -> ChainResult<Self> {
        Self::with_storage(StorageDb::open_native(path)?, params)
    }
    fn with_storage(storage: StorageDb, params: mmx::ChainParams) -> ChainResult<Self> {
        let chain = Self {
            storage,
            params,
            headers: Default::default(),
        };
        let db = chain.storage.database();
        if db.version() == 0 {
            if db.get("chain_meta", b"tip", None)?.is_some() {
                return Err(invalid("tip in an empty chain"));
            }
        } else {
            if db.get("chain_meta", b"params", None)?
                != Some(chain.params.encode(&mmx_types::vnx_registry()?)?)
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
        let Some(hash) = self.storage.database().get("chain_meta", b"tip", None)? else {
            return Ok(None);
        };
        self.header(&hash.try_into().map_err(|_| invalid("tip hash length"))?)?
            .map_or_else(|| Err(invalid("missing tip block")), |h| Ok(Some(h)))
    }
    pub fn hash_at(&self, height: u32) -> ChainResult<Option<Hash>> {
        self.storage
            .database()
            .get("chain_height", &height.to_be_bytes(), None)?
            .map(|v| v.try_into().map_err(|_| invalid("height hash length")))
            .transpose()
    }
    pub fn block(&self, hash: &Hash) -> ChainResult<Option<Record>> {
        let Some(wire) = self.storage.database().get("chain_blocks", hash, None)? else {
            return Ok(None);
        };
        let block = codec::decode(&wire)?;
        if block.name != "mmx.Block" || bytes::<32>(block.field("hash")?)? != *hash {
            return Err(invalid("block archive key mismatch"));
        }
        Ok(Some(block))
    }
    pub fn header(&self, hash: &Hash) -> ChainResult<Option<mmx::BlockHeader>> {
        Ok(self.shared_header(hash)?.map(|h| h.as_ref().clone()))
    }
    fn shared_header(&self, hash: &Hash) -> ChainResult<Option<Arc<mmx::BlockHeader>>> {
        if let Some(header) = self.headers.get_shared(hash) {
            return Ok(Some(header));
        }
        let legacy = self
            .storage
            .database()
            .get("chain_headers", hash, None)?
            .map(|wire| {
                let header = mmx::BlockHeader::decode(&wire)?;
                if header.hash.0 != *hash {
                    return Err(invalid("header archive key mismatch"));
                }
                Ok(header)
            })
            .transpose()?;
        let header = match legacy {
            Some(header) => Some(header),
            None => self.block(hash)?.as_ref().map(block_header).transpose()?,
        };
        Ok(header.map(|h| self.headers.insert(h)))
    }
    pub fn transaction(&self, id: &Address) -> ChainResult<Option<Record>> {
        self.storage
            .database()
            .get("chain_transactions", id, None)?
            .map(|wire| {
                let tx = codec::decode(&wire)?;
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
        self.append_mode(candidate, points, ValidationMode::Live)
    }
    pub fn append_sync(&mut self, candidate: &Record) -> ChainResult<block::Receipt> {
        self.append_mode(candidate, &[], ValidationMode::HistoricalSync)
    }
    fn append_mode(
        &mut self,
        candidate: &Record,
        points: &[VerifiedPoint],
        mode: ValidationMode,
    ) -> ChainResult<block::Receipt> {
        self.append_evidence(candidate, points, mode, None)
    }
    fn append_evidence(
        &mut self,
        candidate: &Record,
        points: &[VerifiedPoint],
        mode: ValidationMode,
        evidence: Option<&mmx_consensus::transitions::VerifiedHeader>,
    ) -> ChainResult<block::Receipt> {
        let began = Instant::now();
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
        let prepared_context = Instant::now();
        block::verify_history_evidence(
            &context,
            candidate,
            self,
            (mode == ValidationMode::Live).then_some(points),
            evidence,
        )?;
        let verified = Instant::now();
        let (receipt, batch) = block::prepare_batch(&self.storage, &context, candidate)?;
        let executed = Instant::now();
        self.publish(candidate, &batch, false)?;
        if block::profile_replay() && context.previous.height.wrapping_add(1).is_multiple_of(128) {
            eprintln!("Replay block: height={} context_us={:.3} history_us={:.3} body_us={:.3} publish_us={:.3}",
                context.previous.height.wrapping_add(1),
                prepared_context.duration_since(began).as_secs_f64() * 1e6,
                verified.duration_since(prepared_context).as_secs_f64() * 1e6,
                executed.duration_since(verified).as_secs_f64() * 1e6,
                executed.elapsed().as_secs_f64() * 1e6);
        }
        Ok(receipt)
    }
    /// Speculate on a detached ledger. A failed block or journal write cannot
    /// expose a partially reverted chain. `height` is the first replaced block.
    pub fn reorganize(
        &mut self,
        height: u32,
        blocks: &[(Record, Vec<VerifiedPoint>)],
    ) -> ChainResult<Vec<block::Receipt>> {
        self.reorganize_stream(
            height,
            blocks
                .iter()
                .map(|(b, p)| Ok((b.clone(), p.clone(), ValidationMode::Live, None))),
            true,
        )
    }
    pub(crate) fn reorganize_stream(
        &mut self,
        height: u32,
        blocks: impl IntoIterator<
            Item = ChainResult<(
                Record,
                Vec<VerifiedPoint>,
                ValidationMode,
                Option<mmx_consensus::transitions::VerifiedHeader>,
            )>,
        >,
        collect_receipts: bool,
    ) -> ChainResult<Vec<block::Receipt>> {
        if height == 0 {
            return Err(invalid("cannot replace genesis"));
        }
        let mut branch = Self {
            storage: StorageDb::new(self.storage.database().fork(height)?),
            params: self.params.clone(),
            headers: self.headers.before(height),
        };
        let mut receipts = Vec::new();
        for entry in blocks {
            let (block, points, mode, evidence) = entry?;
            let receipt = branch.append_evidence(&block, &points, mode, evidence.as_ref())?;
            if collect_receipts {
                receipts.push(receipt);
            }
        }
        let publication = Instant::now();
        self.storage
            .database_mut()
            .replace_suffix(height, branch.storage.database())?;
        if block::profile_replay() {
            eprintln!(
                "Replay publication: start={height} blocks={} seconds={:.6}",
                branch.storage.database().version().saturating_sub(height),
                publication.elapsed().as_secs_f64()
            );
        }
        self.headers = branch.headers;
        Ok(receipts)
    }
    fn vdf_payout(&self, height: u32, parent: &mmx::BlockHeader) -> ChainResult<Option<Address>> {
        let interval = self.params.vdf_reward_interval;
        if interval == 0 {
            return Err(invalid("zero VDF reward interval"));
        }
        if !height.is_multiple_of(interval) {
            return Ok(None);
        }
        let mut ancestors: Vec<Arc<mmx::BlockHeader>> = Vec::new();
        for _ in 1..interval {
            let current = ancestors.last().map_or(parent, Arc::as_ref);
            if current.height == 0 {
                break;
            }
            let previous = self
                .shared_header(&current.prev.0)?
                .ok_or_else(|| invalid("missing payout ancestor"))?;
            ancestors.push(previous);
        }
        Ok(block::economics::vdf_reward_winner_iter(
            interval,
            std::iter::once(parent).chain(ancestors.iter().map(Arc::as_ref)),
        ))
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
        let header = block_header(block)?;
        let mut writes = vec![
            Write::put("chain_blocks", hash, codec::encode(block)?),
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
                codec::encode(tx)?,
            ));
        }
        self.storage.commit_transaction(batch, writes, next)?;
        self.headers.insert(header);
        Ok(())
    }
    /// Remove blocks and ledger effects at heights >= `height`, matching native
    /// Node::revert. Zero clears genesis too; a replacement branch can then be
    /// appended explicitly. Orphan archives/fork choice are not maintained here.
    pub fn revert(&mut self, height: u32) -> ChainResult<()> {
        self.storage.database_mut().revert(height)?;
        self.headers.clear();
        Ok(())
    }
}
// Slice without rehashing: native Block and Header have distinct type identities.
// Full immutable blocks retain the exact header fields, including legacy fields
// outside the hash. Persisting both duplicated the entire space-proof payload.
fn block_header(block: &Record) -> ChainResult<mmx::BlockHeader> {
    Ok(mmx_consensus::header::to_typed(block)?)
}
impl HeaderSource for ChainState {
    fn get_header(&self, hash: &Hash) -> ProofResult<Option<mmx::BlockHeader>> {
        self.header(hash)
            .map_err(|e| ProofError::Invalid(e.to_string()))
    }
    fn get_shared_header(&self, hash: &Hash) -> ProofResult<Option<Arc<mmx::BlockHeader>>> {
        self.shared_header(hash)
            .map_err(|e| ProofError::Invalid(e.to_string()))
    }
}
