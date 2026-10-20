//! Native timelord_sk.dat (VNX generic fixed byte array). Never replace an
//! unreadable/corrupt identity silently; new files are exclusive and mode 0600.
use crate::{error, Result};
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use mmx_consensus::{vdf, Record};
use mmx_types::mmx;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;
const PREFIX: [u8; 10] = [17, 0, 3, 0, 11, 0, 32, 0, 1, 0];
pub struct Identity(SigningKey);
impl Identity {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self> {
        Ok(Self(SigningKey::from_slice(bytes)?))
    }
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match Self::load(path) {
            Ok(key) => return Ok(key),
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(e) => return Err(e),
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(42));
        bytes.extend(PREFIX);
        bytes.extend_from_slice(&key.to_bytes());
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        match file.persist_noclobber(path) {
            Ok(_) => (),
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Self::load(path)
            }
            Err(e) => return Err(e.error.into()),
        }
        #[cfg(unix)]
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            File::open(parent)?.sync_all()?;
        }
        Ok(Self(key))
    }
    fn load(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(path)?;
        let mut bytes = Zeroizing::new(Vec::new());
        (&mut file).take(43).read_to_end(&mut bytes)?;
        if bytes.len() != 42 || bytes[..10] != PREFIX {
            return Err(error(
                "invalid native timelord key file; existing file preserved",
            ));
        }
        let secret = Zeroizing::new(<[u8; 32]>::try_from(&bytes[10..])?);
        let key = Self::from_bytes(&secret)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if file.metadata()?.permissions().mode() & 0o077 != 0 {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
        }
        Ok(key)
    }
    pub fn public_key(&self) -> [u8; 33] {
        self.0
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap()
    }
    pub fn sign(&self, mut proof: mmx::ProofOfTime) -> Result<mmx::ProofOfTime> {
        proof.timelord_key = self.public_key().into();
        let hash = Record::from_generated(&proof)?.calc_hash(false, 0)?;
        proof.hash = hash.into();
        let signature: Signature = self.0.sign_prehash(&hash)?;
        let signature: [u8; 64] = signature.to_bytes().into();
        proof.timelord_sig = signature.into();
        proof.content_hash = vdf::calc_content_hash(&Record::from_generated(&proof)?)?.into();
        Ok(proof)
    }
}
