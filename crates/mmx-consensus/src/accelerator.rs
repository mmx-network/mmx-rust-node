//! Shared, bounded GPU work with portable CPU fallback. Driver failures disable
//! the engine once; a proof-output mismatch is an invalid proof, not a driver error.
use crate::{pos, Hash};
use mmx_opencl::{DeviceInfo, OpenCl};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

// Native indexes a named device among all GPUs with that exact name. Without
// a name it indexes the requested platform, or the first available platform.
fn select_device<'a>(
    devices: &'a [DeviceInfo],
    index: usize,
    platform: Option<&str>,
    name: Option<&str>,
) -> mmx_opencl::Result<&'a DeviceInfo> {
    let platform = platform
        .filter(|p| !p.is_empty())
        .or_else(|| devices.first().map(|d| d.platform.as_str()));
    devices
        .iter()
        .filter(|d| match name {
            Some(name) => d.name == name,
            None => Some(d.platform.as_str()) == platform,
        })
        .nth(index)
        .ok_or_else(|| mmx_opencl::Error("no matching OpenCL GPU device".into()))
}

pub struct Accelerator {
    device: DeviceInfo,
    engine: Mutex<Option<OpenCl>>,
}
impl std::fmt::Debug for Accelerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Accelerator")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}
impl Accelerator {
    /// Production selects GPU devices. Tests can also select a software ICD.
    pub fn new(device: usize, platform: Option<&str>, gpu_only: bool) -> mmx_opencl::Result<Self> {
        let mut engine = OpenCl::new(device, platform, gpu_only)?;
        // Fail closed on bad compilers/devices before constructing any consensus
        // evidence, exercising both kernels, byte order and multi-launch hashing.
        let inputs = [[0u8; 32], std::array::from_fn(|i| i as u8), [255; 32]];
        let expected: Vec<Hash> = inputs
            .iter()
            .map(|input| {
                let mut hash = *input;
                for _ in 0..2501 {
                    hash = Sha256::digest(hash).into();
                }
                hash
            })
            .collect();
        if engine.repeated_sha256(&inputs, 2501)? != expected {
            return Err(mmx_opencl::Error("OpenCL SHA256 self-test failed".into()));
        }
        let keys = [[0u8; 64], std::array::from_fn(|i| i as u8), [255; 64]];
        if engine.memory_hashes(&keys)? != keys.iter().map(pos::memory_hash).collect::<Vec<_>>() {
            return Err(mmx_opencl::Error(
                "OpenCL memory-hash self-test failed".into(),
            ));
        }
        Ok(Self {
            device: engine.device.clone(),
            engine: Mutex::new(Some(engine)),
        })
    }
    /// Configured production selection: -1 disables; failure preserves CPU work.
    pub fn configured(
        device: i32,
        platform: Option<&str>,
        name: Option<&str>,
    ) -> Option<Arc<Self>> {
        if device < 0 {
            return None;
        }
        let name = name.filter(|n| !n.is_empty());
        let selected = (|| {
            let devices = mmx_opencl::devices(None, true)?;
            let selected = select_device(&devices, device as usize, platform, name)?;
            let engine = Self::new(selected.index, None, true)?;
            if engine.device.name != selected.name || engine.device.platform != selected.platform {
                return Err(mmx_opencl::Error(
                    "OpenCL device list changed during selection".into(),
                ));
            }
            Ok(engine)
        })();
        match selected {
            Ok(engine) => {
                eprintln!(
                    "Using OpenCL GPU '{}' [{}] ({})",
                    engine.device.name, device, engine.device.platform
                );
                Some(Arc::new(engine))
            }
            Err(e) => {
                eprintln!("OpenCL unavailable; using CPU: {e}");
                None
            }
        }
    }
    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }
    pub fn enabled(&self) -> bool {
        self.engine.lock().is_ok_and(|e| e.is_some())
    }
    fn run<T>(&self, operation: impl FnOnce(&mut OpenCl) -> mmx_opencl::Result<T>) -> Option<T> {
        let mut guard = self.engine.lock().ok()?;
        let engine = guard.as_mut()?;
        match operation(engine) {
            Ok(result) => Some(result),
            Err(e) => {
                eprintln!("OpenCL work failed; disabling GPU and using CPU: {e}");
                guard.take();
                None
            }
        }
    }
    pub(crate) fn repeated_sha256(
        &self,
        inputs: &[[u8; 32]],
        iterations: u32,
    ) -> Option<Vec<Hash>> {
        self.run(|engine| engine.repeated_sha256(inputs, iterations))
    }
    pub(crate) fn memory_hashes(&self, keys: &[[u8; 64]]) -> Option<Vec<[u8; 128]>> {
        self.run(|engine| engine.memory_hashes(keys))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_gpu_indexes_are_relative_to_name_and_unnamed_indexes_to_platform() {
        let devices: Vec<_> = [
            ("A", "first"),
            ("B", "second"),
            ("B", "first"),
            ("C", "second"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (name, platform))| DeviceInfo {
            index,
            name: name.into(),
            platform: platform.into(),
        })
        .collect();
        assert_eq!(
            select_device(&devices, 0, None, Some("B")).unwrap().index,
            1
        );
        assert_eq!(
            select_device(&devices, 1, None, Some("B")).unwrap().index,
            2
        );
        // As in native, a name takes priority over the platform setting.
        assert_eq!(
            select_device(&devices, 0, Some("first"), Some("B"))
                .unwrap()
                .index,
            1
        );
        assert_eq!(select_device(&devices, 1, None, None).unwrap().index, 2);
        assert_eq!(
            select_device(&devices, 1, Some("second"), None)
                .unwrap()
                .index,
            3
        );
        for (index, platform, name) in [
            (2, None, Some("B")),
            (0, Some("missing"), None),
            (0, None, Some("missing")),
        ] {
            assert!(select_device(&devices, index, platform, name).is_err());
        }
        assert!(select_device(&[], 0, None, None).is_err());
    }
    #[test]
    fn driver_error_disables_engine_and_proofs_are_rechecked_on_cpu() {
        if std::env::var("MMX_OPENCL_TESTS").as_deref() != Ok("1") {
            return;
        }
        let a = Accelerator::new(
            0,
            None,
            std::env::var("MMX_OPENCL_ALL_DEVICES").as_deref() != Ok("1"),
        )
        .unwrap();
        assert!(a.enabled());
        assert!(a
            .run::<()>(|_| Err(mmx_opencl::Error("injected driver error".into())))
            .is_none());
        assert!(!a.enabled());
        let first = Sha256::digest([0; 64]);
        let mut reward = Sha256::new();
        reward.update(first);
        reward.update([0; 32]);
        let output: Hash = Sha256::digest(reward.finalize()).into();
        let mut r = crate::Record::from_generated(&mmx_types::mmx::ProofOfTime {
            segment_size: 1,
            segments: vec![output.into()],
            ..Default::default()
        })
        .unwrap();
        assert!(crate::vdf::WorkVerifiedProof::verify_with(r.clone(), &a).is_ok());
        r.fields.insert(
            "segments".into(),
            crate::Value::Sequence(vec![crate::Value::Bytes(vec![0; 32])]),
        );
        assert!(crate::vdf::WorkVerifiedProof::verify_with(r, &a).is_err());
    }
    #[test]
    fn disabled_engine_and_poisoned_lock_fall_back_without_creating_evidence() {
        let a = Accelerator {
            device: DeviceInfo {
                index: 0,
                name: "test".into(),
                platform: "test".into(),
            },
            engine: Mutex::new(None),
        };
        assert!(!a.enabled());
        assert!(a.memory_hashes(&[[0; 64]]).is_none());
        let _ = std::panic::catch_unwind(|| {
            let _guard = a.engine.lock().unwrap();
            panic!("test");
        });
        assert!(!a.enabled());
        assert!(a.repeated_sha256(&[[0; 32]], 1).is_none());
    }
}
