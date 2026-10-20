//! C++ TimeLord interval semantics, with a dedicated serial hashing thread.
//! Signing/network work never runs on that thread and history never touches disk.
pub mod engine;
pub mod identity;
mod worker;
pub use worker::{Affinity, Control, TimeLord};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}
