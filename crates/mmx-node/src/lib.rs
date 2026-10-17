//! Runnable MMX ledger node. Consensus records never pass through JSON on the
//! peer path; RPC presentation is separate from validation and persistence.
mod inbox;
pub mod node;
pub mod peer;
pub mod rpc;
pub mod sync;

pub use node::Node;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub(crate) fn error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}
