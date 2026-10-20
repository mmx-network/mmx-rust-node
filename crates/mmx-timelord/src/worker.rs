use crate::{
    engine::{Completed, Config, Engine, Status},
    error, Result,
};
use mmx_types::mmx;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
struct Shared {
    engine: Mutex<Engine>,
    changed: Condvar,
    shutdown: AtomicBool,
    generation: AtomicU64,
}
#[derive(Clone)]
pub struct Control(Arc<Shared>);
impl Control {
    pub fn submit(&self, request: mmx::IntervalRequest) -> Result<()> {
        let mut engine = self
            .0
            .engine
            .lock()
            .map_err(|_| error("timelord lock poisoned"))?;
        if self.0.shutdown.load(Ordering::Relaxed) {
            return Err(error("timelord is shutting down"));
        }
        engine.submit(request)?;
        self.0
            .generation
            .store(engine.status().generation, Ordering::Release);
        self.0.changed.notify_all();
        Ok(())
    }
    pub fn stop(&self) -> Result<()> {
        let mut engine = self
            .0
            .engine
            .lock()
            .map_err(|_| error("timelord lock poisoned"))?;
        engine.stop();
        self.0
            .generation
            .store(engine.status().generation, Ordering::Release);
        self.0.changed.notify_all();
        Ok(())
    }
    pub fn status(&self) -> Result<Status> {
        Ok(self
            .0
            .engine
            .lock()
            .map_err(|_| error("timelord lock poisoned"))?
            .status())
    }
    pub fn refresh_publication(&self) -> Result<()> {
        self.0
            .engine
            .lock()
            .map_err(|_| error("timelord lock poisoned"))?
            .refresh_publication();
        Ok(())
    }
    pub fn wait_completed(&self, timeout: Duration, max: usize) -> Result<Vec<Completed>> {
        let engine = self
            .0
            .engine
            .lock()
            .map_err(|_| error("timelord lock poisoned"))?;
        let mut engine = if !engine.ready() && !self.0.shutdown.load(Ordering::Relaxed) {
            self.0
                .changed
                .wait_timeout(engine, timeout)
                .map_err(|_| error("timelord lock poisoned"))?
                .0
        } else {
            engine
        };
        Ok(engine.take_completed(max))
    }
    pub fn current(&self, proof: &Completed) -> bool {
        !self.0.shutdown.load(Ordering::Acquire)
            && self.0.generation.load(Ordering::Acquire) == proof.generation
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub enum Affinity {
    #[default]
    Auto,
    Any,
    Core(usize),
}
pub struct TimeLord {
    control: Control,
    worker: Option<JoinHandle<Result<()>>>,
    cpu: Option<usize>,
    backend: mmx_crypto::recursive::SingleBackend,
}
impl TimeLord {
    /// Pin only the hashing thread when explicitly requested, leaving RPC,
    /// signing and the rest of the user's applications on their existing CPUs.
    pub fn spawn(config: Config, affinity: Affinity) -> Result<Self> {
        let shared = Arc::new(Shared {
            engine: Mutex::new(Engine::new(config)?),
            changed: Condvar::new(),
            shutdown: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        });
        let control = Control(shared.clone());
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("mmx-timelord".into())
            .spawn(move || {
                let cpu = match affinity {
                    Affinity::Core(cpu) => pin(cpu).map(|()| Some(cpu)),
                    Affinity::Any => Ok(None),
                    Affinity::Auto => prefer_efficiency_core(),
                };
                let cpu = match cpu {
                    Ok(cpu) => cpu,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return Err(e);
                    }
                };
                let _ = ready_tx.send(Ok((cpu, mmx_crypto::recursive::single_backend())));
                run(shared)
            })?;
        let (cpu, backend) = match ready_rx.recv()? {
            Ok(ready) => ready,
            Err(message) => {
                let _ = worker.join();
                return Err(error(message));
            }
        };
        Ok(Self {
            control,
            worker: Some(worker),
            cpu,
            backend,
        })
    }
    pub fn control(&self) -> Control {
        self.control.clone()
    }
    pub fn cpu(&self) -> Option<usize> {
        self.cpu
    }
    pub fn backend(&self) -> mmx_crypto::recursive::SingleBackend {
        self.backend
    }
    pub fn shutdown(&mut self) -> Result<()> {
        let guard = self
            .control
            .0
            .engine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.control.0.shutdown.store(true, Ordering::Release);
        self.control.0.changed.notify_all();
        drop(guard);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| error("timelord worker panicked"))??;
        }
        Ok(())
    }
}
impl Drop for TimeLord {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
fn run(shared: Arc<Shared>) -> Result<()> {
    loop {
        let (job, segment) = {
            let mut engine = shared
                .engine
                .lock()
                .map_err(|_| error("timelord lock poisoned"))?;
            loop {
                if shared.shutdown.load(Ordering::Acquire) {
                    return Ok(());
                }
                if let Some(job) = engine.job() {
                    break (job, engine.segment_iters());
                }
                engine = shared
                    .changed
                    .wait(engine)
                    .map_err(|_| error("timelord lock poisoned"))?;
            }
        };
        let start = Instant::now();
        let mut output = job.input;
        let mut remaining = u64::from(segment);
        // Keep native's register-resident whole-chain kernel, checking cheap
        // atomics between 65,536 iterations for bounded restart/shutdown latency.
        while remaining > 0 {
            if shared.shutdown.load(Ordering::Acquire) {
                return Ok(());
            }
            if shared.generation.load(Ordering::Acquire) != job.generation {
                break;
            }
            let count = remaining.min(65_536);
            mmx_crypto::recursive::hash_one(&mut output, count);
            remaining -= count;
        }
        if remaining != 0 {
            continue;
        }
        let elapsed = start.elapsed();
        let mut engine = shared
            .engine
            .lock()
            .map_err(|_| error("timelord lock poisoned"))?;
        if engine.complete(&job, output, elapsed) {
            shared.changed.notify_all();
        }
    }
}
#[cfg(target_os = "linux")]
fn pin(cpu: usize) -> Result<()> {
    if cpu >= libc::CPU_SETSIZE as usize {
        return Err(error("timelord CPU index exceeds CPU_SETSIZE"));
    }
    // SAFETY: initialized cpu_set_t, checked index, current thread (pid zero),
    // and the exact platform structure size. Kernel enforces allowed affinity.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        if libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn pin(_: usize) -> Result<()> {
    Err(error(
        "explicit timelord CPU affinity is currently supported on Linux",
    ))
}

#[cfg(all(target_os = "linux", any(target_arch = "x86", target_arch = "x86_64")))]
fn prefer_efficiency_core() -> Result<Option<usize>> {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::__cpuid;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::__cpuid;
    // Probe only this worker, respecting the inherited cpuset. CPUID leaf 1A
    // describes the currently selected logical processor; model names and
    // frequency guesses cannot reliably distinguish P-cores and E-cores.
    unsafe {
        let vendor = __cpuid(0);
        if vendor.ebx != 0x756e6547
            || vendor.edx != 0x49656e69
            || vendor.ecx != 0x6c65746e
            || vendor.eax < 0x1a
        {
            return Ok(None);
        }
        let mut allowed: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of_val(&allowed), &mut allowed) != 0 {
            return Ok(None);
        }
        for cpu in 0..libc::CPU_SETSIZE as usize {
            if !libc::CPU_ISSET(cpu, &allowed) || pin(cpu).is_err() {
                continue;
            }
            let core = __cpuid(0x1a).eax >> 24;
            let basic = __cpuid(1).ecx;
            let sha = __cpuid(7).ebx & (1 << 29) != 0;
            if core == 0x20 && sha && basic & (1 << 9) != 0 && basic & (1 << 19) != 0 {
                return Ok(Some(cpu));
            }
        }
        if libc::sched_setaffinity(0, std::mem::size_of_val(&allowed), &allowed) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(None)
}
#[cfg(not(all(target_os = "linux", any(target_arch = "x86", target_arch = "x86_64"))))]
fn prefer_efficiency_core() -> Result<Option<usize>> {
    Ok(None)
}
