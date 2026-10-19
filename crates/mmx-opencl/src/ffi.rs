//! OpenCL 1.2 ABI. All symbols retain their loader through `Arc<Api>`.
//! No OpenCL headers or link-time SDK are needed by binaries using this crate.
use crate::{Error, Result};
use libloading::Library;
use std::{
    ffi::{c_char, c_void},
    sync::Arc,
};

pub type Handle = *mut c_void;
pub type Release = unsafe extern "system" fn(Handle) -> i32;
type ContextCallback =
    Option<unsafe extern "system" fn(*const c_char, *const c_void, usize, *mut c_void)>;
type BuildCallback = Option<unsafe extern "system" fn(Handle, *mut c_void)>;

macro_rules! api {
    ($($name:ident: $ty:ty),* $(,)?) => {
        #[allow(non_snake_case)]
        pub struct Api { $(pub $name: $ty,)* _library: Library }
        impl Api {
            pub fn load() -> Result<Arc<Self>> {
                #[cfg(target_os = "windows")]
                let names = ["OpenCL.dll"];
                #[cfg(target_os = "macos")]
                let names = ["/System/Library/Frameworks/OpenCL.framework/OpenCL"];
                #[cfg(not(any(target_os = "windows", target_os = "macos")))]
                let names = ["libOpenCL.so.1", "libOpenCL.so"];
                let mut errors = Vec::new();
                for name in names {
                    // SAFETY: load only the system OpenCL ICD with its documented ABI.
                    let library = match unsafe { Library::new(name) } {
                        Ok(lib) => lib,
                        Err(e) => { errors.push(e.to_string()); continue; }
                    };
                    // SAFETY: exact OpenCL 1.2 signatures, copied function pointers
                    // remain valid because this Api owns the library until last use.
                    return unsafe { Ok(Arc::new(Self {
                        $($name: *library.get::<$ty>(concat!(stringify!($name), "\0").as_bytes())
                            .map_err(|e| Error(e.to_string()))?,)*
                        _library: library,
                    })) };
                }
                Err(Error(format!("OpenCL loader unavailable: {}", errors.join("; "))))
            }
        }
    };
}
api! {
    clGetPlatformIDs: unsafe extern "system" fn(u32, *mut Handle, *mut u32) -> i32,
    clGetPlatformInfo: unsafe extern "system" fn(Handle, u32, usize, *mut c_void, *mut usize) -> i32,
    clGetDeviceIDs: unsafe extern "system" fn(Handle, u64, u32, *mut Handle, *mut u32) -> i32,
    clGetDeviceInfo: unsafe extern "system" fn(Handle, u32, usize, *mut c_void, *mut usize) -> i32,
    clCreateContext: unsafe extern "system" fn(*const isize, u32, *const Handle, ContextCallback, *mut c_void, *mut i32) -> Handle,
    clReleaseContext: Release,
    clCreateCommandQueue: unsafe extern "system" fn(Handle, Handle, u64, *mut i32) -> Handle,
    clReleaseCommandQueue: Release,
    clCreateProgramWithSource: unsafe extern "system" fn(Handle, u32, *const *const c_char, *const usize, *mut i32) -> Handle,
    clBuildProgram: unsafe extern "system" fn(Handle, u32, *const Handle, *const c_char, BuildCallback, *mut c_void) -> i32,
    clGetProgramBuildInfo: unsafe extern "system" fn(Handle, Handle, u32, usize, *mut c_void, *mut usize) -> i32,
    clReleaseProgram: Release,
    clCreateKernel: unsafe extern "system" fn(Handle, *const c_char, *mut i32) -> Handle,
    clReleaseKernel: Release,
    clCreateBuffer: unsafe extern "system" fn(Handle, u64, usize, *mut c_void, *mut i32) -> Handle,
    clReleaseMemObject: Release,
    clSetKernelArg: unsafe extern "system" fn(Handle, u32, usize, *const c_void) -> i32,
    clEnqueueWriteBuffer: unsafe extern "system" fn(Handle, Handle, u32, usize, usize, *const c_void, u32, *const Handle, *mut Handle) -> i32,
    clEnqueueReadBuffer: unsafe extern "system" fn(Handle, Handle, u32, usize, usize, *mut c_void, u32, *const Handle, *mut Handle) -> i32,
    clEnqueueNDRangeKernel: unsafe extern "system" fn(Handle, Handle, u32, *const usize, *const usize, *const usize, u32, *const Handle, *mut Handle) -> i32,
    clFinish: unsafe extern "system" fn(Handle) -> i32,
}
pub fn check(code: i32, operation: &str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error(format!("{operation}: OpenCL error {code}")))
    }
}
pub struct Object {
    pub raw: Handle,
    pub api: Arc<Api>,
    release: Release,
}
impl Object {
    pub fn new(
        api: &Arc<Api>,
        release: Release,
        create: impl FnOnce(*mut i32) -> Handle,
    ) -> Result<Self> {
        let mut code = 0;
        let raw = create(&mut code);
        check(code, "create OpenCL object")?;
        if raw.is_null() {
            return Err(Error("OpenCL returned a null object".into()));
        }
        Ok(Self {
            raw,
            api: api.clone(),
            release,
        })
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        // SAFETY: one release for this owned live object, while its ICD is loaded.
        unsafe {
            (self.release)(self.raw);
        }
    }
}
