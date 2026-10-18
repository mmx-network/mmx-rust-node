//! Offline conversion; interrupted targets resume from their durable cursor.
use mmx_db::native::import::{self, Options};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(2..=3).contains(&args.len()) {
        return Err(
            "usage: import_indexed SOURCE_DB NEW_NATIVE_DB [MiB/s; default 4, 0 disables pacing]"
                .into(),
        );
    }
    let mut options = Options::default();
    if let Some(rate) = args.get(2) {
        let rate: u64 = rate.parse()?;
        options.bytes_per_second = (rate > 0)
            .then(|| rate.checked_mul(1024 * 1024).ok_or("rate overflow"))
            .transpose()?;
    }
    let mut last = Instant::now();
    let mut rows = u64::MAX;
    let report = import::indexed(&args[0], &args[1], options, |report| {
        if report.finished
            || (rows != report.rows
                && (rows == u64::MAX || last.elapsed() >= Duration::from_secs(1)))
        {
            eprintln!(
                "version={} rows={} value_bytes={} transfer_bytes={} resumed_rows={} finished={}",
                report.version,
                report.rows,
                report.value_bytes,
                report.transfer_bytes,
                report.resumed_rows,
                report.finished
            );
            last = Instant::now();
            rows = report.rows;
        }
        Ok(())
    })?;
    if !report.finished {
        return Err("import did not complete".into());
    }
    Ok(())
}
