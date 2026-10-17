//! Initialize/reopen a durable chain using original MMX configuration/assets.
use mmx_execution::chain::{genesis, network::Network, ChainState};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: bootstrap MMX_ROOT CONFIGURATION DATABASE".into());
    }
    let network = Network::load(&args[0], &args[1])?;
    let genesis = genesis::build(&network.params, &network.assets)?;
    let hash: [u8; 32] = genesis.field("hash")?.bytes()?.try_into()?;
    let name = network.params.network.clone();
    let mut chain = ChainState::open(&args[2], network.params)?;
    let action = if chain.tip()?.is_none() {
        chain.initialize(&network.assets)?;
        "Initialized"
    } else {
        if chain.hash_at(0)? != Some(hash) {
            return Err("bootstrap assets do not match stored genesis".into());
        }
        "Opened"
    };
    let tip = chain.tip()?.ok_or("missing chain tip")?;
    println!(
        "{action} {name} at height {}: {}",
        tip.height,
        vnx::util::to_hex_string(&tip.hash.0, false, false)
    );
    Ok(())
}
