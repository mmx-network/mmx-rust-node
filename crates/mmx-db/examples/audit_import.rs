//! Compare requested complete tables, at current or historical DB versions.
use mmx_db::native::audit::{Options, Verifier};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err("usage: audit_import SOURCE_DB NATIVE_DB TABLE [DB_VERSION ...]; DB_VERSION is exclusive (block height + 1), default current; paced at 2 MiB/s".into());
    }
    let verifier = Verifier::open(&args[0], &args[1])?;
    let versions = if args.len() == 3 {
        vec![verifier.version()]
    } else {
        args[3..]
            .iter()
            .map(|s| s.parse())
            .collect::<Result<Vec<u32>, _>>()?
    };
    for version in versions {
        let mut last = Instant::now();
        verifier.table(&args[2], version, Options::default(), |r| {
            if r.finished || last.elapsed() >= Duration::from_secs(1) {
                println!(
                    "table={} version={} keys={} visible_rows={} transfer_bytes={} finished={}",
                    args[2], r.version, r.keys, r.visible_rows, r.transfer_bytes, r.finished
                );
                last = Instant::now();
            }
            Ok(())
        })?;
    }
    Ok(())
}
