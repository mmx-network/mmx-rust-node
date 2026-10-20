//! Dependency-ready dispatch; workers never wait on another worker or publish
//! ledger changes. The owner merges completed VM deltas before releasing users.
use super::*;
use mmx_vm::{storage, value::FLAG_KEY, Var};
use std::sync::{mpsc, RwLock};

type Outcome = Result<(TransactionResult, TransactionBatch), transaction::Rejected>;
type Cell = (Address, u64, Option<u64>);
#[derive(Default)]
struct State {
    writes: BTreeMap<Cell, Var>,
    keys: BTreeMap<(Address, mmx_vm::Value), u64>,
    balances: BTreeMap<(Address, Address), u128>,
}
struct View<'a, S> {
    backend: &'a S,
    state: RwLock<State>,
}
impl<S: Storage> View<'_, S> {
    fn absorb(&self, batch: &mut TransactionBatch) -> storage::Result<()> {
        let mut state = self
            .state
            .write()
            .map_err(|_| storage::Error::Invalid("block overlay poisoned"))?;
        // Native interned keys are immutable. Validate the entire incoming
        // delta before moving it; no database or socket work holds this lock.
        let mut seen = BTreeMap::new();
        for (contract, write) in &batch.writes {
            let cell = (*contract, write.address, write.key);
            if write.key.is_none()
                && seen
                    .get(&cell)
                    .or_else(|| state.writes.get(&cell))
                    .is_some_and(|var: &Var| var.flags & FLAG_KEY != 0)
            {
                return Err(storage::Error::Invalid("cannot overwrite key"));
            }
            seen.insert(cell, write.var.clone());
        }
        for ((contract, address, key), var) in seen {
            if key.is_none() && var.flags & FLAG_KEY != 0 {
                state.keys.insert((contract, var.value.clone()), address);
            }
            state.writes.insert((contract, address, key), var);
        }
        batch.writes.clear();
        for balance in batch.balances.drain(..) {
            state
                .balances
                .insert((balance.contract, balance.currency), balance.amount);
        }
        Ok(())
    }
}
impl<S: Storage> Storage for View<'_, S> {
    fn read(&self, c: &Address, a: u64, k: Option<u64>) -> storage::Result<Option<Var>> {
        let value = self
            .state
            .read()
            .map_err(|_| storage::Error::Invalid("block overlay poisoned"))?
            .writes
            .get(&(*c, a, k))
            .cloned();
        match value {
            Some(value) => Ok(Some(value)),
            None => self.backend.read(c, a, k),
        }
    }
    fn lookup(&self, c: &Address, v: &mmx_vm::Value) -> storage::Result<Option<u64>> {
        let value = self
            .state
            .read()
            .map_err(|_| storage::Error::Invalid("block overlay poisoned"))?
            .keys
            .get(&(*c, v.clone()))
            .copied();
        match value {
            Some(value) => Ok(Some(value)),
            None => self.backend.lookup(c, v),
        }
    }
    fn get_balance(&self, c: &Address, k: &Address) -> storage::Result<Option<u128>> {
        let value = self
            .state
            .read()
            .map_err(|_| storage::Error::Invalid("block overlay poisoned"))?
            .balances
            .get(&(*c, *k))
            .copied();
        match value {
            Some(value) => Ok(Some(value)),
            None => self.backend.get_balance(c, k),
        }
    }
    fn get_contract(&self, address: &Address) -> storage::Result<Option<storage::ContractData>> {
        self.backend.get_contract(address)
    }
    fn get_transaction_index(
        &self,
        id: &Address,
    ) -> storage::Result<Option<storage::TransactionIndex>> {
        self.backend.get_transaction_index(id)
    }
    fn apply(&mut self, _: &Address, _: &[storage::StorageWrite]) -> storage::Result<()> {
        Err(storage::Error::Invalid("block execution view is read-only"))
    }
}

/// At most the configured Rayon width runs or awaits completion. Ready tasks
/// have block-index priority; no queued task consumes a worker waiting on a DAG
/// predecessor. The owner observes each completion before starting dependents.
fn schedule<T: Send>(
    predecessors: &[BTreeSet<usize>],
    run: impl Fn(usize) -> T + Sync,
    mut complete: impl FnMut(usize, &mut T),
) -> Vec<T> {
    let width = rayon::current_num_threads().min(predecessors.len()).max(1);
    let mut waiting: Vec<_> = predecessors.iter().map(BTreeSet::len).collect();
    let mut successors = vec![Vec::new(); predecessors.len()];
    let mut ready = BTreeSet::new();
    for (index, depends) in predecessors.iter().enumerate() {
        if depends.is_empty() {
            ready.insert(index);
        }
        for &previous in depends {
            assert!(previous < index, "block dependency must precede its user");
            successors[previous].push(index);
        }
    }
    let mut results: Vec<Option<T>> = (0..predecessors.len()).map(|_| None).collect();
    let (send, receive) = mpsc::sync_channel(width);
    rayon::in_place_scope(|scope| {
        let mut active = 0;
        let mut done = 0;
        while done < results.len() {
            while active < width {
                let Some(index) = ready.pop_first() else {
                    break;
                };
                let send = send.clone();
                let run = &run;
                scope.spawn(move |_| {
                    // A worker panic must not leave the owner waiting forever
                    // on a channel whose original sender is still alive.
                    let value =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(index)));
                    let _ = send.send((index, value));
                });
                active += 1;
            }
            assert!(active != 0, "block dependency graph cannot make progress");
            let (index, value) = receive.recv().expect("execution workers disconnected");
            let mut value = match value {
                Ok(value) => value,
                Err(panic) => std::panic::resume_unwind(panic),
            };
            complete(index, &mut value);
            results[index] = Some(value);
            active -= 1;
            done += 1;
            for &next in &successors[index] {
                waiting[next] -= 1;
                if waiting[next] == 0 {
                    ready.insert(next);
                }
            }
        }
    });
    results.into_iter().map(Option::unwrap).collect()
}

pub(super) fn execute<S: Storage + Sync>(
    storage: &S,
    context: &TransactionContext,
    txs: &[&Record],
    plan: &ExecutionPlan,
    rejected: &mut Rejected,
) -> Result<(Vec<TransactionResult>, TransactionBatch), ValidationError> {
    // Empty/single-TX blocks pay no scheduling cost. A nested Rayon caller must
    // not occupy its only worker waiting for tasks on that same pool.
    if txs.len() < 2
        || plan
            .predecessors
            .iter()
            .enumerate()
            .skip(1)
            .all(|(index, depends)| depends.contains(&(index - 1)))
        || rayon::current_thread_index().is_some()
        || rayon::current_num_threads() == 1
    {
        return serial(storage, context, txs, plan, rejected);
    }
    let view = View {
        backend: storage,
        state: RwLock::new(State::default()),
    };
    let outcomes = schedule(
        &plan.predecessors,
        |index| {
            let mut context = context.clone();
            context.locked_contracts = Some(plan.locks[index].clone());
            let mut info = ErrorInfo::default();
            let mut total_cost = 0;
            transaction::build(
                storage,
                &view,
                &context,
                txs[index],
                &mut info,
                &mut total_cost,
            )
            .map_err(|error| transaction::Rejected {
                error,
                info,
                total_cost,
            })
        },
        |_, outcome: &mut Outcome| {
            if let Ok((result, batch)) = outcome {
                if let Err(error) = view.absorb(batch) {
                    *outcome = Err(transaction::Rejected {
                        error: error.into(),
                        info: ErrorInfo::default(),
                        total_cost: result.total_cost.into(),
                    });
                }
            }
        },
    );
    let mut results = Vec::with_capacity(txs.len());
    let mut batch = TransactionBatch::default();
    // Completion order never changes contract/index publication or which
    // transaction error is reported. No part of a rejected block is published.
    for (index, outcome) in outcomes.into_iter().enumerate() {
        rejected.transaction = Some(index);
        let (result, effects) = outcome.map_err(|failure| {
            rejected.info = failure.info;
            rejected.total_cost = failure.total_cost;
            failure.error
        })?;
        batch.contracts.extend(effects.contracts);
        batch.transactions.extend(effects.transactions);
        results.push(result);
    }
    batch.writes = view
        .state
        .into_inner()
        .map_err(|_| storage::Error::Invalid("block overlay poisoned"))?
        .writes
        .into_iter()
        .map(|((contract, address, key), var)| {
            (contract, storage::StorageWrite { address, key, var })
        })
        .collect();
    rejected.transaction = None;
    Ok((results, batch))
}

fn serial<S: Storage>(
    storage: &S,
    context: &TransactionContext,
    txs: &[&Record],
    plan: &ExecutionPlan,
    rejected: &mut Rejected,
) -> Result<(Vec<TransactionResult>, TransactionBatch), ValidationError> {
    let mut execution = Shared(Rc::new(RefCell::new(Overlay::new(storage))));
    let mut results = Vec::with_capacity(txs.len());
    let mut batch = TransactionBatch::default();
    let mut context = context.clone();
    for (index, tx) in txs.iter().enumerate() {
        rejected.transaction = Some(index);
        context.locked_contracts = Some(plan.locks[index].clone());
        let (result, effects) = transaction::build(
            storage,
            &execution,
            &context,
            tx,
            &mut rejected.info,
            &mut rejected.total_cost,
        )?;
        execution.absorb_vm(&effects)?;
        batch.contracts.extend(effects.contracts);
        batch.transactions.extend(effects.transactions);
        results.push(result);
        rejected.info = ErrorInfo::default();
        rejected.total_cost = 0;
    }
    batch.writes = execution.0.borrow().batch().writes;
    rejected.transaction = None;
    Ok((results, batch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    };
    use std::time::Duration;

    #[test]
    fn completed_dependencies_refill_immediately_while_unrelated_work_is_stalled() {
        if rayon::current_num_threads() == 1 {
            return;
        }
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let blocked = release.clone();
        let (started, wait_started) = mpsc::channel();
        let (advanced, wait_advanced) = mpsc::channel();
        let merged = Arc::new(AtomicUsize::new(0));
        let visible = merged.clone();
        let coordinator = std::thread::spawn(move || {
            let plan = [BTreeSet::new(), BTreeSet::new(), BTreeSet::from([1])];
            schedule(
                &plan,
                |index| {
                    if index == 0 {
                        started.send(()).unwrap();
                        let (lock, wake) = &*blocked;
                        let mut done = lock.lock().unwrap();
                        while !*done {
                            done = wake.wait(done).unwrap();
                        }
                    } else if index == 2 {
                        assert_eq!(
                            visible.load(Ordering::Acquire),
                            1,
                            "VM merge must precede dependency release"
                        );
                        advanced.send(()).unwrap();
                    }
                    index
                },
                |index, _| {
                    if index == 1 {
                        merged.store(1, Ordering::Release);
                    }
                },
            )
        });
        wait_started.recv_timeout(Duration::from_secs(3)).unwrap();
        let progressed = wait_advanced.recv_timeout(Duration::from_secs(3));
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        assert!(
            progressed.is_ok(),
            "a completed branch must not wait for a stalled peer task"
        );
        assert_eq!(coordinator.join().unwrap(), vec![0, 1, 2]);
    }
    #[test]
    fn worker_panics_unwind_the_owner_instead_of_hanging_its_completion_channel() {
        let (done, wait) = mpsc::channel();
        let coordinator = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| {
                schedule(
                    &[BTreeSet::new(), BTreeSet::new()],
                    |i| {
                        assert_ne!(i, 0, "deliberate worker panic");
                        i
                    },
                    |_, _| {},
                )
            });
            done.send(result.is_err()).unwrap();
        });
        assert!(wait.recv_timeout(Duration::from_secs(3)).unwrap());
        coordinator.join().unwrap();
    }
}
