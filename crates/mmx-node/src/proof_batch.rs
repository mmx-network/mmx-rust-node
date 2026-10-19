//! C++ verify_proofs' parallel work, immediate completed-result delivery and
//! full barrier before production. Only the owner applies results to the node.
use crate::{error, Result};
use rayon::prelude::*;
use std::sync::mpsc;

pub(crate) fn complete<T, R>(
    jobs: Vec<T>,
    verify: impl Fn(T) -> R + Send + Sync,
    consume: impl FnMut(R) -> Result<()>,
) -> Result<()>
where
    T: Send,
    R: Send,
{
    // Node callers run outside Rayon. An embedded caller already on a pool
    // worker must not block that pool waiting for a producer using the same
    // worker budget (in particular a single-worker pool).
    if jobs.len() < 2 || rayon::current_thread_index().is_some() {
        return jobs.into_iter().map(verify).try_for_each(consume);
    }
    parallel(jobs, verify, consume, None)
}
fn parallel<T, R>(
    jobs: Vec<T>,
    verify: impl Fn(T) -> R + Send + Sync,
    mut consume: impl FnMut(R) -> Result<()>,
    pool: Option<&rayon::ThreadPool>,
) -> Result<()>
where
    T: Send,
    R: Send,
{
    std::thread::scope(|scope| {
        // Never accumulate a second copy of the whole bounded inbound queue.
        // Waiting producers retain only their own completed result.
        let (tx, rx) = mpsc::sync_channel(16);
        let producer = scope.spawn(move || {
            let work = || {
                jobs.into_par_iter().for_each_with(tx, |tx, job| {
                    let _ = tx.send(verify(job));
                });
            };
            if let Some(pool) = pool {
                pool.install(work);
            } else {
                work();
            }
        });
        let mut result = Ok(());
        for done in rx {
            if result.is_ok() {
                result = consume(done);
            }
            // Drain even after an owner failure, so all work joins before the
            // caller can move to another chain context or return an error.
        }
        producer
            .join()
            .map_err(|_| error("proof verification worker panicked"))?;
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Condvar, Mutex,
        },
        time::Duration,
    };
    #[test]
    fn completed_proofs_are_delivered_while_an_earlier_job_is_still_running() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let held = gate.clone();
        let mut received = vec![];
        parallel(
            vec![0, 1],
            move |job| {
                if job == 0 {
                    let lock = held.0.lock().unwrap();
                    let (_guard, timeout) = held
                        .1
                        .wait_timeout_while(lock, Duration::from_secs(3), |v| !*v)
                        .unwrap();
                    assert!(
                        !timeout.timed_out(),
                        "completed work waited for the whole batch"
                    );
                }
                job
            },
            |job| {
                received.push(job);
                if job == 1 {
                    *gate.0.lock().unwrap() = true;
                    gate.1.notify_all();
                }
                Ok(())
            },
            Some(&pool),
        )
        .unwrap();
        assert_eq!(received, [1, 0]);
    }
    #[test]
    fn failure_still_joins_the_full_batch_and_nested_single_worker_calls_complete() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let finished = AtomicUsize::new(0);
        let result = parallel(
            (0..64).collect(),
            |_| {
                finished.fetch_add(1, Ordering::Relaxed);
            },
            |_| Err(error("owner failed")),
            Some(&pool),
        );
        assert_eq!(result.unwrap_err().to_string(), "owner failed");
        assert_eq!(finished.load(Ordering::Relaxed), 64);
        pool.install(|| {
            let mut output = vec![];
            complete(
                vec![1, 2],
                |v| v + 1,
                |v| {
                    output.push(v);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(output, [2, 3]);
        });
    }
}
