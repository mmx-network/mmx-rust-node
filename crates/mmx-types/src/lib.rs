//! MMX protocol declarations generated from the checked-in VNI schema snapshot.
//!
//! The `mmx` and `vnx` modules contain wire types, typed RPC clients and service
//! dispatchers. Use `vnx_registry()` with the VNX runtime to encode these types.
//! Native MMX behavior (validation, consensus hashing and execution) is ported
//! separately; VNX wire serialization is not transaction hash serialization.
//!
//! Fixed byte arrays retain their declared wire representation. MMX address
//! presentation, amount arithmetic and cryptographic operations belong to the
//! MMX implementation rather than to the generated declarations.

// Generated bindings preserve VNI signatures and defaults. Keep their emitted
// style separate from the lint policy for handwritten MMX code and tests.
#[allow(clippy::all)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated.rs"));
}

pub use generated::*;

pub use ::vnx::VnxType;
