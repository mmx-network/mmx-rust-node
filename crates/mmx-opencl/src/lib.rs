//! Bounded OpenCL 1.2 work engines, loaded at runtime. This crate cannot create
//! consensus evidence; callers must compare outputs and perform admission.
mod ffi;
mod pipeline;
use ffi::{check, Api, Handle, Object};
pub use pipeline::{MemoryOutput, MemoryPipeline, MemoryProfile, TimeSpan, MEMORY_PIPELINE_DEPTH};
use std::{ffi::CString, ptr};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;
pub const MEMORY_BATCH: usize = 4096;
const MAX_SEGMENTS: usize = 65536;
const MAX_INFO: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub index: usize,
    pub name: String,
    pub platform: String,
}
fn info(api: &Api, handle: Handle, key: u32, platform: bool) -> Result<String> {
    let function = if platform {
        api.clGetPlatformInfo
    } else {
        api.clGetDeviceInfo
    };
    let mut size = 0;
    // SAFETY: size query has no destination, followed by a bounded buffer of the returned size.
    unsafe {
        check(
            function(handle, key, 0, ptr::null_mut(), &mut size),
            "query info size",
        )?;
    }
    if size > MAX_INFO {
        return Err(Error("OpenCL info size limit".into()));
    }
    let mut bytes = vec![0u8; size];
    unsafe {
        check(
            function(
                handle,
                key,
                size,
                bytes.as_mut_ptr().cast(),
                ptr::null_mut(),
            ),
            "query info",
        )?;
    }
    Ok(String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .to_owned())
}
fn enumerate(
    api: &Api,
    platform: Option<&str>,
    gpu_only: bool,
) -> Result<Vec<(DeviceInfo, Handle)>> {
    let mut count = 0;
    // SAFETY: all counts and output arrays match the OpenCL ABI and array sizes.
    unsafe {
        check(
            (api.clGetPlatformIDs)(0, ptr::null_mut(), &mut count),
            "enumerate platforms",
        )?;
    }
    if count > 256 {
        return Err(Error("OpenCL platform count limit".into()));
    }
    if count == 0 {
        return Ok(vec![]);
    }
    let mut platforms = vec![ptr::null_mut(); count as usize];
    unsafe {
        check(
            (api.clGetPlatformIDs)(count, platforms.as_mut_ptr(), ptr::null_mut()),
            "read platforms",
        )?;
    }
    let mut out = Vec::new();
    for p in platforms {
        let name = info(api, p, 0x0902, true)?; // CL_PLATFORM_NAME
        if platform.is_some_and(|filter| !name.contains(filter)) {
            continue;
        }
        let kind = if gpu_only { 1 << 2 } else { 0xFFFF_FFFF };
        let mut n = 0;
        let code = unsafe { (api.clGetDeviceIDs)(p, kind, 0, ptr::null_mut(), &mut n) };
        if code == -1 {
            continue;
        } // CL_DEVICE_NOT_FOUND
        check(code, "enumerate devices")?;
        if n > 256 {
            return Err(Error("OpenCL device count limit".into()));
        }
        if n == 0 {
            continue;
        }
        let mut devices = vec![ptr::null_mut(); n as usize];
        unsafe {
            check(
                (api.clGetDeviceIDs)(p, kind, n, devices.as_mut_ptr(), ptr::null_mut()),
                "read devices",
            )?;
        }
        for d in devices {
            if gpu_only {
                let mut device_type = 0u64;
                // SAFETY: CL_DEVICE_TYPE is a cl_device_type bitfield (u64).
                // Check the returned device itself as well as the discovery
                // filter; CPU/accelerator ICDs must never enter a GPU-only list.
                unsafe {
                    check(
                        (api.clGetDeviceInfo)(
                            d,
                            0x1000,
                            size_of::<u64>(),
                            (&mut device_type as *mut u64).cast(),
                            ptr::null_mut(),
                        ),
                        "query device type",
                    )?;
                }
                if device_type & (1 << 2) == 0 {
                    continue;
                }
            }
            out.push((
                DeviceInfo {
                    index: out.len(),
                    name: info(api, d, 0x102B, false)?,
                    platform: name.clone(),
                },
                d,
            ));
        }
    }
    Ok(out)
}
/// GPU indexes are in platform/device discovery order, filtered by platform.
/// `gpu_only=false` is useful for conformance tests with a CPU OpenCL ICD.
pub fn devices(platform: Option<&str>, gpu_only: bool) -> Result<Vec<DeviceInfo>> {
    let api = Api::load()?;
    Ok(enumerate(&api, platform, gpu_only)?
        .into_iter()
        .map(|(info, _)| info)
        .collect())
}
struct Buffer {
    object: Object,
    capacity: usize,
}
/// An exclusively owned command queue and kernels. Move between threads, but
/// serialize calls (including kernel argument changes) through `&mut self`.
pub struct OpenCl {
    pub device: DeviceInfo,
    raw_device: Handle,
    vdf: Object,
    generate: Object,
    memory: Object,
    vdf_buffer: Option<Buffer>,
    key_buffer: Option<Buffer>,
    memory_buffer: Option<Buffer>,
    output_buffer: Option<Buffer>,
    queue: Object,
    _program: Object,
    context: Object,
}
// SAFETY: OpenCL host handles have no thread affinity. All queue use and kernel
// argument mutation require exclusive access; no mapped memory or callbacks exist.
unsafe impl Send for OpenCl {}

impl OpenCl {
    pub fn new(index: usize, platform: Option<&str>, gpu_only: bool) -> Result<Self> {
        if !cfg!(target_endian = "little") {
            return Err(Error("OpenCL requires a little endian host".into()));
        }
        let api = Api::load()?;
        let (device, raw) = enumerate(&api, platform, gpu_only)?
            .into_iter()
            .find(|(d, _)| d.index == index)
            .ok_or_else(|| Error(format!("no OpenCL device {index}")))?;
        let mut little: u32 = 0;
        // SAFETY: CL_DEVICE_ENDIAN_LITTLE returns exactly one cl_bool (u32).
        unsafe {
            check(
                (api.clGetDeviceInfo)(
                    raw,
                    0x1026,
                    size_of::<u32>(),
                    (&mut little as *mut u32).cast(),
                    ptr::null_mut(),
                ),
                "device endianness",
            )?;
        }
        if little == 0 {
            return Err(Error("OpenCL requires a little endian device".into()));
        }
        let context = Object::new(&api, api.clReleaseContext, |err| unsafe {
            (api.clCreateContext)(ptr::null(), 1, &raw, None, ptr::null_mut(), err)
        })?;
        let source = concat!(include_str!("sha256.cl"), "\n", include_str!("work.cl"));
        let sources = [source.as_ptr().cast()];
        let sizes = [source.len()];
        let program = Object::new(&api, api.clReleaseProgram, |err| unsafe {
            (api.clCreateProgramWithSource)(context.raw, 1, sources.as_ptr(), sizes.as_ptr(), err)
        })?;
        let options = c"-cl-std=CL1.2";
        let code = unsafe {
            (api.clBuildProgram)(
                program.raw,
                1,
                &raw,
                options.as_ptr(),
                None,
                ptr::null_mut(),
            )
        };
        if code != 0 {
            let mut size = 0;
            unsafe {
                check(
                    (api.clGetProgramBuildInfo)(
                        program.raw,
                        raw,
                        0x1183,
                        0,
                        ptr::null_mut(),
                        &mut size,
                    ),
                    "build log size",
                )?;
            }
            if size > MAX_INFO {
                return Err(Error("OpenCL build log size limit".into()));
            }
            let mut log = vec![0u8; size];
            unsafe {
                check(
                    (api.clGetProgramBuildInfo)(
                        program.raw,
                        raw,
                        0x1183,
                        size,
                        log.as_mut_ptr().cast(),
                        ptr::null_mut(),
                    ),
                    "build log",
                )?;
            }
            return Err(Error(format!(
                "OpenCL build failed ({code}): {}",
                String::from_utf8_lossy(&log)
            )));
        }
        let kernel = |name: &str| -> Result<Object> {
            let name = CString::new(name).map_err(|e| Error(e.to_string()))?;
            Object::new(&api, api.clReleaseKernel, |err| unsafe {
                (api.clCreateKernel)(program.raw, name.as_ptr(), err)
            })
        };
        let queue = Object::new(&api, api.clReleaseCommandQueue, |err| unsafe {
            (api.clCreateCommandQueue)(context.raw, raw, 0, err)
        })?;
        Ok(Self {
            device,
            raw_device: raw,
            vdf: kernel("repeated_sha256")?,
            generate: kernel("generate_memory")?,
            memory: kernel("memory_hash")?,
            vdf_buffer: None,
            key_buffer: None,
            memory_buffer: None,
            output_buffer: None,
            queue,
            _program: program,
            context,
        })
    }
    /// Three independent in-order queues and pinned host buffers for asynchronous
    /// memory-hash batches. Existing synchronous APIs are unchanged.
    pub fn into_memory_pipeline(self, profiling: bool) -> Result<MemoryPipeline> {
        MemoryPipeline::new(self, profiling)
    }
    fn ensure(&self, buffer: &mut Option<Buffer>, size: usize) -> Result<()> {
        if buffer.as_ref().is_some_and(|b| b.capacity >= size) {
            return Ok(());
        }
        let api = &self.context.api;
        let object = Object::new(api, api.clReleaseMemObject, |err| unsafe {
            (api.clCreateBuffer)(self.context.raw, 1, size, ptr::null_mut(), err)
        })?;
        *buffer = Some(Buffer {
            object,
            capacity: size,
        });
        Ok(())
    }
    fn argument<T>(&self, kernel: &Object, i: u32, value: &T) -> Result<()> {
        // SAFETY: only scalar u32s and live cl_mem handles are passed. OpenCL copies
        // the value immediately; kernels define the same argument sizes/order.
        unsafe {
            check(
                (self.context.api.clSetKernelArg)(
                    kernel.raw,
                    i,
                    size_of::<T>(),
                    (value as *const T).cast(),
                ),
                "kernel argument",
            )
        }
    }
    fn enqueue(&self, kernel: &Object, width: usize, local: Option<usize>) -> Result<()> {
        unsafe {
            check(
                (self.context.api.clEnqueueNDRangeKernel)(
                    self.queue.raw,
                    kernel.raw,
                    1,
                    ptr::null(),
                    &width,
                    local.as_ref().map_or(ptr::null(), |v| v),
                    0,
                    ptr::null(),
                    ptr::null_mut(),
                ),
                "enqueue kernel",
            )
        }
    }
    fn upload(&self, buffer: &Buffer, bytes: &[u8]) -> Result<()> {
        if bytes.len() > buffer.capacity {
            return Err(Error("OpenCL upload bounds".into()));
        }
        // SAFETY: blocking transfer, valid slice, and bounds checked against allocation.
        unsafe {
            check(
                (self.context.api.clEnqueueWriteBuffer)(
                    self.queue.raw,
                    buffer.object.raw,
                    1,
                    0,
                    bytes.len(),
                    bytes.as_ptr().cast(),
                    0,
                    ptr::null(),
                    ptr::null_mut(),
                ),
                "upload",
            )
        }
    }
    fn download(&self, buffer: &Buffer, bytes: &mut [u8]) -> Result<()> {
        if bytes.len() > buffer.capacity {
            return Err(Error("OpenCL download bounds".into()));
        }
        // SAFETY: blocking transfer keeps host destination alive through completion.
        unsafe {
            check(
                (self.context.api.clEnqueueReadBuffer)(
                    self.queue.raw,
                    buffer.object.raw,
                    1,
                    0,
                    bytes.len(),
                    bytes.as_mut_ptr().cast(),
                    0,
                    ptr::null(),
                    ptr::null_mut(),
                ),
                "download",
            )
        }
    }
    pub fn repeated_sha256(
        &mut self,
        inputs: &[[u8; 32]],
        iterations: u32,
    ) -> Result<Vec<[u8; 32]>> {
        if inputs.is_empty() || iterations == 0 {
            return Ok(inputs.to_vec());
        }
        if inputs.len() > MAX_SEGMENTS {
            let mut result = Vec::with_capacity(inputs.len());
            for chunk in inputs.chunks(MAX_SEGMENTS) {
                result.extend(self.repeated_sha256(chunk, iterations)?);
            }
            return Ok(result);
        }
        let mut buffer = self.vdf_buffer.take();
        self.ensure(&mut buffer, inputs.len() * 32)?;
        self.vdf_buffer = buffer;
        let buffer = self.vdf_buffer.as_ref().unwrap();
        self.upload(buffer, inputs.as_flattened())?;
        self.argument(&self.vdf, 0, &buffer.object.raw)?;
        self.argument(&self.vdf, 2, &(inputs.len() as u32))?;
        let mut remaining = iterations;
        let mut launches = 0;
        while remaining > 0 {
            let step = remaining.min(2500);
            self.argument(&self.vdf, 1, &step)?;
            // Match native OCL_VDF's 64-thread groups; driver-selected 256-thread
            // groups underutilize the GPU on typical mainnet segment counts.
            self.enqueue(&self.vdf, inputs.len().div_ceil(64) * 64, Some(64))?;
            remaining -= step;
            launches += 1;
            if launches == 16 {
                // Bound queued driver commands even for large iteration counts.
                unsafe {
                    check(
                        (self.context.api.clFinish)(self.queue.raw),
                        "drain VDF queue",
                    )?;
                }
                launches = 0;
            }
        }
        let mut result = vec![[0u8; 32]; inputs.len()];
        self.download(buffer, result.as_flattened_mut())?;
        Ok(result)
    }
    pub fn memory_hashes(&mut self, keys: &[[u8; 64]]) -> Result<Vec<[u8; 128]>> {
        let mut result = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(MEMORY_BATCH) {
            // Scratch allocation is bounded at 16 MiB, reused across chunks/proofs.
            let n = chunk.len();
            let mut keys = self.key_buffer.take();
            let mut memory = self.memory_buffer.take();
            let mut output = self.output_buffer.take();
            self.ensure(&mut keys, n * 64)?;
            self.ensure(&mut memory, n * 4096)?;
            self.ensure(&mut output, n * 128)?;
            self.key_buffer = keys;
            self.memory_buffer = memory;
            self.output_buffer = output;
            let keys = self.key_buffer.as_ref().unwrap();
            let memory = self.memory_buffer.as_ref().unwrap();
            let output = self.output_buffer.as_ref().unwrap();
            self.upload(keys, chunk.as_flattened())?;
            self.argument(&self.generate, 0, &keys.object.raw)?;
            self.argument(&self.generate, 1, &memory.object.raw)?;
            self.argument(&self.generate, 2, &(n as u32))?;
            self.enqueue(&self.generate, n, None)?;
            self.argument(&self.memory, 0, &memory.object.raw)?;
            self.argument(&self.memory, 1, &output.object.raw)?;
            self.argument(&self.memory, 2, &(n as u32))?;
            self.enqueue(&self.memory, n * 32, Some(32))?;
            let start = result.len();
            result.resize(start + n, [0; 128]);
            self.download(output, result[start..].as_flattened_mut())?;
        }
        Ok(result)
    }
}
impl Drop for OpenCl {
    fn drop(&mut self) {
        // SAFETY: drain device commands before releasing the engine's objects.
        unsafe {
            (self.context.api.clFinish)(self.queue.raw);
        }
    }
}
