//! Three bounded streams. Each owns independent device and pinned host buffers;
//! queue ordering supplies stage dependencies, completion events guard reuse.
use crate::{
    ffi::{check, Api, Handle, Object},
    Error, OpenCl, Result,
};
use std::{ptr, rc::Rc, sync::Arc};

pub const MEMORY_PIPELINE_DEPTH: usize = 3;
pub const MAX_MEMORY_PIPELINE_BATCH: usize = 65536;
#[derive(Clone, Copy, Debug)]
pub struct TimeSpan {
    pub start: u64,
    pub end: u64,
}
#[derive(Clone, Debug)]
pub struct MemoryProfile {
    pub slot: usize,
    /// Upload, memory generation, memory mixing, download; device nanoseconds.
    pub stages: [TimeSpan; 4],
}
pub struct MemoryOutput<'a> {
    pub keys: &'a [[u8; 64]],
    pub hashes: &'a [[u8; 128]],
    pub profile: Option<MemoryProfile>,
}

struct Mapped {
    buffer: Object,
    pointer: Handle,
    queue: Rc<Object>,
}
impl Mapped {
    fn new(engine: &OpenCl, queue: Rc<Object>, bytes: usize) -> Result<Self> {
        let api = &engine.context.api;
        let buffer = Object::new(api, api.clReleaseMemObject, |err| unsafe {
            // CL_MEM_READ_WRITE | CL_MEM_ALLOC_HOST_PTR
            (api.clCreateBuffer)(engine.context.raw, 1 | 16, bytes, ptr::null_mut(), err)
        })?;
        let mut error = 0;
        // SAFETY: mapping blocks; this object owns the buffer and retains queue.
        let pointer = unsafe {
            (api.clEnqueueMapBuffer)(
                queue.raw,
                buffer.raw,
                1,
                1 | 2,
                0,
                bytes,
                0,
                ptr::null(),
                ptr::null_mut(),
                &mut error,
            )
        };
        check(error, "map pinned staging buffer")?;
        if pointer.is_null() {
            return Err(Error("null pinned mapping".into()));
        }
        // Initialize before exposing Rust slices, including unused tail capacity.
        unsafe {
            ptr::write_bytes(pointer.cast::<u8>(), 0, bytes);
        }
        Ok(Self {
            buffer,
            pointer,
            queue,
        })
    }
}
impl Drop for Mapped {
    fn drop(&mut self) {
        let api = &self.buffer.api;
        // SAFETY: complete all DMA before unmapping its host source/destination.
        // This also handles partially constructed streams and failed submissions.
        unsafe {
            (api.clFinish)(self.queue.raw);
            (api.clEnqueueUnmapMemObject)(
                self.queue.raw,
                self.buffer.raw,
                self.pointer,
                0,
                ptr::null(),
                ptr::null_mut(),
            );
            (api.clFinish)(self.queue.raw);
        }
    }
}
struct Slot {
    queue: Rc<Object>,
    keys: Object,
    scratch: Object,
    output: Object,
    host_keys: Mapped,
    host_output: Mapped,
    events: Vec<Object>,
    count: usize,
    prepared: bool,
}
impl Slot {
    fn new(engine: &OpenCl, profiling: bool, batch: usize) -> Result<Self> {
        let api = &engine.context.api;
        let queue = Rc::new(Object::new(api, api.clReleaseCommandQueue, |err| unsafe {
            (api.clCreateCommandQueue)(
                engine.context.raw,
                engine.raw_device,
                if profiling { 2 } else { 0 },
                err,
            )
        })?);
        let buffer = |bytes| {
            Object::new(api, api.clReleaseMemObject, |err| unsafe {
                (api.clCreateBuffer)(engine.context.raw, 1, bytes, ptr::null_mut(), err)
            })
        };
        Ok(Self {
            keys: buffer(batch * 64)?,
            scratch: buffer(batch * 4096)?,
            output: buffer(batch * 128)?,
            host_keys: Mapped::new(engine, queue.clone(), batch * 64)?,
            host_output: Mapped::new(engine, queue.clone(), batch * 128)?,
            queue,
            events: Vec::new(),
            count: 0,
            prepared: false,
        })
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        // SAFETY: retire every queued command before releasing any device buffers
        // or mapped storage, even if submission failed before creating its event.
        unsafe {
            (self.queue.api.clFinish)(self.queue.raw);
        }
    }
}

pub struct MemoryPipeline {
    slots: Vec<Slot>,
    engine: OpenCl,
    profiling: bool,
    poisoned: bool,
    batch_size: usize,
}
// SAFETY: all Rc clones/mappings are private to this owner and move together;
// no reference-count operations or queue/mapping mutations run concurrently.
// OpenCL handles/mappings have no host-thread affinity. The type is not Sync.
unsafe impl Send for MemoryPipeline {}
impl MemoryPipeline {
    pub(crate) fn new(engine: OpenCl, profiling: bool, batch_size: usize) -> Result<Self> {
        if batch_size == 0 || batch_size > MAX_MEMORY_PIPELINE_BATCH {
            return Err(Error("invalid memory pipeline capacity".into()));
        }
        let slots = (0..MEMORY_PIPELINE_DEPTH)
            .map(|_| Slot::new(&engine, profiling, batch_size))
            .collect::<Result<_>>()?;
        Ok(Self {
            slots,
            engine,
            profiling,
            poisoned: false,
            batch_size,
        })
    }
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }
    pub fn memory_kernel(&self) -> &'static str {
        self.engine.memory_kernel()
    }
    pub fn device(&self) -> &crate::DeviceInfo {
        &self.engine.device
    }
    fn slot(&mut self, index: usize) -> Result<&mut Slot> {
        if self.poisoned {
            return Err(Error("memory pipeline failed to drain".into()));
        }
        self.slots
            .get_mut(index)
            .ok_or_else(|| Error("invalid memory pipeline slot".into()))
    }
    /// Fill pinned keys directly. A slot cannot be prepared while its previous
    /// batch is in flight; the returned borrow also prevents concurrent submit.
    pub fn prepare(&mut self, index: usize, count: usize) -> Result<&mut [[u8; 64]]> {
        if count == 0 || count > self.batch_size {
            return Err(Error("invalid memory pipeline batch length".into()));
        }
        let slot = self.slot(index)?;
        if slot.prepared || !slot.events.is_empty() {
            return Err(Error("memory pipeline slot is occupied".into()));
        }
        slot.count = count;
        slot.prepared = true;
        // SAFETY: completed, initialized mapping; no queued use of this slot.
        Ok(unsafe { std::slice::from_raw_parts_mut(slot.host_keys.pointer.cast(), count) })
    }
    pub fn submit(&mut self, index: usize) -> Result<()> {
        let slot = self.slot(index)?;
        if !slot.prepared || !slot.events.is_empty() {
            return Err(Error("memory pipeline slot is not prepared".into()));
        }
        let result = self.enqueue(index);
        if result.is_err() {
            let _ = self.drain();
        }
        result
    }
    fn enqueue(&mut self, index: usize) -> Result<()> {
        let engine = &self.engine;
        let slot = &mut self.slots[index];
        let api = &engine.context.api;
        let queue = slot.queue.raw;
        // SAFETY: host mappings and device buffers are separately owned, live and
        // sized for count <= MEMORY_BATCH. All transfers are nonblocking; their
        // pointers remain owned and unavailable for reuse until finish/drain.
        // One in-order queue per slot orders write -> generate -> mix -> read.
        let upload = event(api, |ev| unsafe {
            (api.clEnqueueWriteBuffer)(
                queue,
                slot.keys.raw,
                0,
                0,
                slot.count * 64,
                slot.host_keys.pointer,
                0,
                ptr::null(),
                ev,
            )
        })?;
        slot.events.push(upload);
        engine.argument(&engine.generate, 0, &slot.keys.raw)?;
        engine.argument(&engine.generate, 1, &slot.scratch.raw)?;
        engine.argument(&engine.generate, 2, &(slot.count as u32))?;
        slot.events.push(kernel_event(
            api,
            queue,
            engine.generate.raw,
            if engine.memory_local == 128 {
                slot.count.div_ceil(256) * 256
            } else {
                slot.count
            },
            if engine.memory_local == 128 {
                Some(256)
            } else {
                None
            },
        )?);
        engine.argument(&engine.memory, 0, &slot.scratch.raw)?;
        engine.argument(&engine.memory, 1, &slot.output.raw)?;
        engine.argument(&engine.memory, 2, &(slot.count as u32))?;
        slot.events.push(kernel_event(
            api,
            queue,
            engine.memory.raw,
            (slot.count * 32).div_ceil(engine.memory_local) * engine.memory_local,
            Some(engine.memory_local),
        )?);
        slot.events.push(event(api, |ev| unsafe {
            (api.clEnqueueReadBuffer)(
                queue,
                slot.output.raw,
                0,
                0,
                slot.count * 128,
                slot.host_output.pointer,
                0,
                ptr::null(),
                ev,
            )
        })?);
        // Submit immediately, so CPU preparation/finalization of other batches
        // overlaps device work without a clFinish or a blocking transfer.
        check(unsafe { (api.clFlush)(queue) }, "flush memory pipeline")?;
        slot.prepared = false;
        Ok(())
    }
    /// Wait only for this slot's download. Other slots can remain in flight.
    /// Borrowed keys/results remain valid until the next mutable pipeline call.
    pub fn finish(&mut self, index: usize) -> Result<MemoryOutput<'_>> {
        let profiling = self.profiling;
        let slot = self.slot(index)?;
        if slot.prepared || slot.events.len() != 4 {
            return Err(Error("memory pipeline slot is not submitted".into()));
        }
        let api = &slot.queue.api;
        check(
            unsafe { (api.clWaitForEvents)(1, &slot.events[3].raw) },
            "wait for memory pipeline download",
        )?;
        let profile = if profiling {
            // Profiling is queried only after every individual event is reported
            // complete, including on drivers that retire stream events lazily.
            let events: Vec<_> = slot.events.iter().map(|event| event.raw).collect();
            check(
                unsafe { (api.clWaitForEvents)(events.len() as u32, events.as_ptr()) },
                "wait for profiling events",
            )?;
            let mut stages = [TimeSpan { start: 0, end: 0 }; 4];
            for (dst, ev) in stages.iter_mut().zip(&slot.events) {
                *dst = TimeSpan {
                    start: timestamp(ev, 0x1282)?,
                    end: timestamp(ev, 0x1283)?,
                };
            }
            Some(MemoryProfile {
                slot: index,
                stages,
            })
        } else {
            None
        };
        slot.events.clear();
        // SAFETY: the download's completion also completes the earlier upload and
        // both kernels in this queue. Distinct slots have disjoint mappings.
        Ok(unsafe {
            MemoryOutput {
                keys: std::slice::from_raw_parts(slot.host_keys.pointer.cast(), slot.count),
                hashes: std::slice::from_raw_parts(slot.host_output.pointer.cast(), slot.count),
                profile,
            }
        })
    }
    /// Drain all queues before error recovery/cancellation and permit reuse.
    pub fn drain(&mut self) -> Result<()> {
        let mut error = None;
        for slot in &mut self.slots {
            if let Err(e) = check(
                unsafe { (slot.queue.api.clFinish)(slot.queue.raw) },
                "drain memory pipeline",
            ) {
                error.get_or_insert(e);
            }
            slot.events.clear();
            slot.prepared = false;
            slot.count = 0;
        }
        self.poisoned = error.is_some();
        error.map_or(Ok(()), Err)
    }
}
fn event(api: &Arc<Api>, enqueue: impl FnOnce(*mut Handle) -> i32) -> Result<Object> {
    Object::new(api, api.clReleaseEvent, |error| {
        let mut raw = ptr::null_mut();
        let code = enqueue(&mut raw);
        // SAFETY: Object::new provides a live error output for this call.
        unsafe {
            *error = code;
        }
        raw
    })
}
fn kernel_event(
    api: &Arc<Api>,
    queue: Handle,
    kernel: Handle,
    width: usize,
    local: Option<usize>,
) -> Result<Object> {
    // SAFETY: scalar NDRange values and kernel arguments are live during enqueue;
    // OpenCL captures argument values before a later slot changes this kernel.
    event(api, |ev| unsafe {
        (api.clEnqueueNDRangeKernel)(
            queue,
            kernel,
            1,
            ptr::null(),
            &width,
            local.as_ref().map_or(ptr::null(), |v| v),
            0,
            ptr::null(),
            ev,
        )
    })
}
fn timestamp(event: &Object, parameter: u32) -> Result<u64> {
    let mut value = 0u64;
    check(
        unsafe {
            (event.api.clGetEventProfilingInfo)(
                event.raw,
                parameter,
                size_of::<u64>(),
                (&mut value as *mut u64).cast(),
                ptr::null_mut(),
            )
        },
        "memory pipeline profiling",
    )?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MEMORY_BATCH;
    #[test]
    fn slots_guard_reuse_complete_independently_and_drain() {
        if std::env::var("MMX_OPENCL_TESTS").as_deref() != Ok("1") {
            return;
        }
        let mut reference = OpenCl::new(0, None, true).unwrap();
        let mut pipe = OpenCl::new(0, None, true)
            .unwrap()
            .into_memory_pipeline(true)
            .unwrap();
        assert!(pipe.prepare(MEMORY_PIPELINE_DEPTH, 1).is_err());
        assert!(pipe.prepare(0, 0).is_err());
        assert!(pipe.prepare(0, MEMORY_BATCH + 1).is_err());
        assert!(pipe.submit(0).is_err());
        assert!(pipe.finish(0).is_err());
        let keys: Vec<Vec<[u8; 64]>> = (0..MEMORY_PIPELINE_DEPTH)
            .map(|slot| {
                (0..MEMORY_BATCH - slot * 31)
                    .map(|i| std::array::from_fn(|j| (i * 137 + j * 71 + slot * 23) as u8))
                    .collect()
            })
            .collect();
        for (slot, keys) in keys.iter().enumerate() {
            pipe.prepare(slot, keys.len())
                .unwrap()
                .copy_from_slice(keys);
            assert!(pipe.prepare(slot, 1).is_err());
            pipe.submit(slot).unwrap();
            assert!(pipe.prepare(slot, 1).is_err());
            assert!(pipe.submit(slot).is_err());
        }
        // Retirement need not follow submission order because storage is disjoint.
        for slot in [2, 0, 1] {
            let actual = pipe.finish(slot).unwrap();
            assert_eq!(actual.keys, &keys[slot]);
            assert_eq!(actual.hashes, reference.memory_hashes(&keys[slot]).unwrap());
            let stages = actual.profile.unwrap().stages;
            for stage in &stages {
                assert!(stage.start <= stage.end);
            }
            for stages in stages.windows(2) {
                assert!(stages[0].end <= stages[1].start);
            }
        }
        pipe.prepare(0, 7).unwrap().copy_from_slice(&keys[0][..7]);
        pipe.submit(0).unwrap();
        pipe.prepare(1, 3).unwrap();
        pipe.drain().unwrap();
        pipe.prepare(0, 1).unwrap()[0] = [123; 64];
        pipe.submit(0).unwrap();
        assert_eq!(
            pipe.finish(0).unwrap().hashes,
            reference.memory_hashes(&[[123; 64]]).unwrap()
        );
        // Drop while all queues still own asynchronous host pointers.
        for slot in 0..MEMORY_PIPELINE_DEPTH {
            pipe.prepare(slot, MEMORY_BATCH)
                .unwrap()
                .fill([slot as u8; 64]);
            pipe.submit(slot).unwrap();
        }
        drop(pipe);
    }
}
