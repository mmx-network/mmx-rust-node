//! Native signed Router identity exchange, shared by inbound and outbound peers.
use super::*;
use rand::RngCore;

pub(super) struct Identity {
    pub id: Hash,
    pub role: u32,
    // The lease keeps the shared identity registry live only for this socket.
    pub _socket: Arc<TcpStream>,
}

impl Client {
    pub(super) fn authenticate(&mut self) -> Result<()> {
        let mut challenge = [0; 32];
        rand::thread_rng().fill_bytes(&mut challenge);
        let values = self.call_many(vec![
            record("mmx.Router.get_id", []),
            record("mmx.Router.get_info", []),
            record(
                "mmx.Router.sign_msg",
                [("msg".into(), Value::Bytes(challenge.to_vec()))],
            ),
        ])?;
        let id: Hash = values[0].bytes()?.try_into()?;
        let info = values[1].record()?;
        let role = number(info, "type")?;
        if id == self.peer_state.id() || crate::node::hash(info, "id")? != id {
            return Err(error("peer is not a distinct node identity"));
        }
        if number(info, "version")? < 103
            || ![
                mmx_types::mmx::node_type_e::FULL_NODE.value,
                mmx_types::mmx::node_type_e::LIGHT_NODE.value,
                mmx_types::mmx::node_type_e::API_CLIENT.value,
            ]
            .contains(&role)
        {
            return Err(error("unsupported peer identity protocol or role"));
        }
        let pair = values[2].seq()?;
        if pair.len() != 2 {
            return Err(error("invalid peer identity proof"));
        }
        let key = pair[0].bytes()?.try_into()?;
        if mmx_crypto::public_key_address(key) != id
            || !mmx_crypto::verify(&challenge, key, pair[1].bytes()?.try_into()?)?
        {
            return Err(error("peer identity verification failed"));
        }
        // Native light/API peers need not export a full ledger service. Only
        // full nodes qualify as sync observers or receive local transactions.
        let synced = if role == mmx_types::mmx::node_type_e::FULL_NODE.value {
            let ledger = self.call_many(vec![
                record(
                    "mmx.Node.get_block_hash",
                    [("height".into(), Value::Uint(0))],
                ),
                record("mmx.Node.get_synced_height", []),
            ])?;
            let genesis: Hash = match ledger[0].optional()? {
                Some(value) => value.bytes()?.try_into()?,
                None => self
                    .call(record("mmx.Node.get_genesis_hash", []))?
                    .bytes()?
                    .try_into()?,
            };
            if genesis != self.peer_state.genesis() {
                return Err(error("peer genesis mismatch"));
            }
            ledger[1]
                .optional()?
                .map(|value| -> Result<u32> { Ok(u32::try_from(value.uint()?)?) })
                .transpose()?
                .is_some()
        } else {
            false
        };
        let socket = self.peer_state.claim(id, &self.connection.stream)?;
        self.identity = Some(Identity {
            id,
            role,
            _socket: socket,
        });
        self.relay_peer = self
            .connection
            .start_relay(self.relay.clone(), role, synced)?;
        for (message, size) in std::mem::take(&mut self.auth_gossip) {
            self.relay_peer.note(&message)?;
            self.relay.admit_gossip(message, size, &self.inbox)?;
        }
        self.auth_bytes = 0;
        Ok(())
    }
}
