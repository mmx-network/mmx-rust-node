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
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Clone)]
pub(crate) struct PeerState {
    key: Arc<SigningKey>,
    id: Hash,
    info: Arc<Record>,
    genesis: Hash,
    height: Arc<AtomicU64>,
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
            key: Arc::new(key.clone()),
            id,
            genesis,
            info: Arc::new(Record::from_generated(&mmx::node_info_t {
                id: mmx::hash_t::from(id),
                version: 103,
                r#type: mmx::node_type_e::FULL_NODE,
            })?),
            height: Arc::new(AtomicU64::new(height.into())),
        })
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
