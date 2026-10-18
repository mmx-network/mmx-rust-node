//! Farming services and native disk plot reads. No ledger database is opened.
pub mod encoding;
pub mod farmer;
pub mod harvester;
pub mod plot;
pub mod service;
pub mod wallet;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) fn error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}
