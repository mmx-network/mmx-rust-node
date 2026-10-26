//! Runnable MMX ledger node. Consensus records never pass through JSON on the
//! peer path; RPC presentation is separate from validation and persistence.
mod http;
mod inbox;
mod io;
pub mod node;
pub mod peer;
mod peer_state;
pub mod production;
mod proof_batch;
mod relay;
pub mod router;
pub mod rpc;
pub mod sync;
mod transactions;
mod vdf_worker;

pub use node::Node;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub(crate) fn error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}
