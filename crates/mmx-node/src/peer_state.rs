//! Native Router identity replies and the last published node height. Socket
//! workers can answer these without waiting for ledger validation or disk I/O.
use crate::{
    node::{hash, number, record},
    Result,
};
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use mmx_consensus::{Hash, Record, Value};
use mmx_types::mmx;
use std::sync::{
    atomic::{AtomicU32, AtomicU64, Ordering},
    Arc, Mutex, Weak,
};

#[derive(Clone)]
pub(crate) struct PeerState {
    pub router: Arc<crate::router::RouterConfig>,
    pub connections: crate::router::Connections,
    key: Arc<SigningKey>,
    id: Hash,
    info: Arc<Record>,
    genesis: Hash,
    height: Arc<AtomicU64>,
    earliest: Arc<AtomicU32>,
    identities: Arc<Mutex<std::collections::BTreeMap<Hash, Weak<std::net::TcpStream>>>>,
}
const SYNCED: u64 = 1 << 32;
impl PeerState {
    pub fn new(key: &SigningKey, genesis: Hash, height: u32) -> Result<Self> {
        let id = mmx_crypto::public_key_address(
            key.verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()?,
        );
        Ok(Self {
            router: Arc::new(crate::router::RouterConfig::default()),
            connections: Default::default(),
            key: Arc::new(key.clone()),
            id,
            genesis,
            info: Arc::new(Record::from_generated(&mmx::node_info_t {
                id: mmx::hash_t::from(id),
                version: 103,
                r#type: mmx::node_type_e::FULL_NODE,
            })?),
            height: Arc::new(AtomicU64::new(height.into())),
            earliest: Arc::new(AtomicU32::new(0)),
            identities: Default::default(),
        })
    }
    pub fn id(&self) -> Hash {
        self.id
    }
    pub fn genesis(&self) -> Hash {
        self.genesis
    }
    /// Native Router replaces the old connection only after the new socket
    /// has proved ownership of the same identity. Weak leases cannot retain
    /// closed sockets, and pruning bounds the registry by live connections.
    pub fn claim(
        &self,
        id: Hash,
        stream: &std::net::TcpStream,
    ) -> Result<Arc<std::net::TcpStream>> {
        if id == self.id {
            return Err(crate::error("peer is our own identity"));
        }
        let socket = Arc::new(stream.try_clone()?);
        let mut identities = self
            .identities
            .lock()
            .map_err(|_| crate::error("peer identities poisoned"))?;
        identities.retain(|_, connection| connection.strong_count() != 0);
        if let Some(previous) = identities
            .insert(id, Arc::downgrade(&socket))
            .and_then(|p| p.upgrade())
        {
            let _ = previous.shutdown(std::net::Shutdown::Both);
        }
        Ok(socket)
    }
    pub fn publish(&self, height: u32, synced: bool) {
        self.height.store(
            u64::from(height) | if synced { SYNCED } else { 0 },
            Ordering::Release,
        );
    }
    pub fn begin_resync(&self) {
        self.height.fetch_and(!SYNCED, Ordering::Release);
    }
    pub fn set_earliest_height(&self, height: u32) {
        self.earliest.store(height, Ordering::Release);
    }
    pub fn call(&self, request: &Record) -> Result<Option<Record>> {
        let value = match request.name.as_str() {
            "mmx.Router.get_id" => Value::Bytes(self.id.to_vec()),
            "mmx.Router.get_info" => Value::Record(self.info.as_ref().clone()),
            "mmx.Router.sign_msg" => {
                let signature: Signature = self.key.sign_prehash(&hash(request, "msg")?)?;
                Value::Sequence(vec![
                    Value::Bytes(
                        self.key
                            .verifying_key()
                            .to_encoded_point(true)
                            .as_bytes()
                            .to_vec(),
                    ),
                    Value::Bytes(signature.to_bytes().to_vec()),
                ])
            }
            "mmx.Router.get_peers" => Value::Sequence(vec![]),
            "mmx.Node.get_height" => {
                Value::Uint(u64::from(self.height.load(Ordering::Acquire) as u32))
            }
            "mmx.Node.get_synced_height" => {
                let state = self.height.load(Ordering::Acquire);
                Value::Optional(
                    (state & SYNCED != 0).then(|| Box::new(Value::Uint(u64::from(state as u32)))),
                )
            }
            "mmx.Node.get_genesis_hash" => Value::Bytes(self.genesis.to_vec()),
            "mmx.Node.get_block_hash" | "mmx.Node.get_block_hash_ex"
                if number(request, "height")? < self.earliest.load(Ordering::Acquire) =>
            {
                Value::Optional(None)
            }
            "mmx.Node.get_block_at" | "mmx.Node.get_header_at"
                if number(request, "height")? < self.earliest.load(Ordering::Acquire) =>
            {
                Value::Null
            }
            "mmx.Node.get_block_hash" if number(request, "height")? == 0 => {
                Value::Optional(Some(Box::new(Value::Bytes(self.genesis.to_vec()))))
            }
            "mmx.Node.get_block_hash" | "mmx.Node.get_block_hash_ex"
                if number(request, "height")? > self.height.load(Ordering::Acquire) as u32 =>
            {
                Value::Optional(None)
            }
            "mmx.Node.get_block_at" | "mmx.Node.get_header_at"
                if number(request, "height")? > self.height.load(Ordering::Acquire) as u32 =>
            {
                Value::Null
            }
            _ => return Ok(None),
        };
        Ok(Some(record(
            &format!("{}.return", request.name),
            [("_ret_0".into(), value)],
        )))
    }
}
