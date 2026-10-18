//! Offline physical-layout rewrite. Store's exclusive writer lock prevents
//! changing a running node; every committed version and tombstone is retained.
use mmx_db::Store;
use std::{error::Error, time::Instant};
fn main() -> Result<(), Box<dyn Error>> {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    if paths.is_empty() {
        return Err("supply indexed database directories".into());
    }
    for path in paths {
        let began = Instant::now();
        let mut store = Store::open_indexed(&path)?;
        let version = store.version();
        let changed = store.optimize_layout()?;
        println!(
            "{}: version={version}, rewritten={changed}, seconds={:.3}",
            std::path::Path::new(&path).display(),
            began.elapsed().as_secs_f64()
        );
    }
    Ok(())
}
