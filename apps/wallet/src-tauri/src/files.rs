//! Files chosen by the user in a save or open dialog (contact lists).
use mmxwallet::service::{Error, Result};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

/// Largest file that can be imported.
pub const MAX_IMPORT_BYTES: u64 = 1024 * 1024;
/// Largest file that can be exported.
pub const MAX_EXPORT_BYTES: usize = 4 * 1024 * 1024;
const MAX_NAME_CHARS: usize = 100;
const DEFAULT_NAME: &str = "export.json";

fn too_large() -> Error {
    Error::new("invalid_argument", "the file is too large")
}

/// A plain file name for the save dialog: letters, digits, dot, dash and
/// underscore (other runs become one dash), no leading dot or dash.
pub fn export_name(suggested: &str) -> String {
    let mut name = String::with_capacity(suggested.len().min(MAX_NAME_CHARS));
    for c in suggested.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            name.push(c);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let name: String = name
        .trim_start_matches(['.', '-'])
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    if name.is_empty() {
        DEFAULT_NAME.into()
    } else {
        name
    }
}

pub fn check_export(contents: &str) -> Result<()> {
    if contents.len() > MAX_EXPORT_BYTES {
        return Err(too_large());
    }
    Ok(())
}

/// Writes the file through a temporary file in the same folder, so an
/// existing file is either replaced completely or left as it was.
pub fn write_export(path: &Path, contents: &str) -> Result<()> {
    check_export(contents)?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(contents.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| Error::from(e.error))?;
    Ok(())
}

/// Reads a text file of at most [`MAX_IMPORT_BYTES`]. A leading byte order
/// mark is dropped.
pub fn read_import(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::new("invalid_argument", "not a file"));
    }
    if metadata.len() > MAX_IMPORT_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
    file.take(MAX_IMPORT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(too_large());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::new("invalid_argument", "the file is not text"))?;
    Ok(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_names_are_plain() {
        assert_eq!(export_name("mmx-contacts.json"), "mmx-contacts.json");
        assert_eq!(export_name("../../etc/passwd"), "etc-passwd");
        assert_eq!(export_name("..\\..\\x.json"), "x.json");
        assert_eq!(export_name("my contacts (1).json"), "my-contacts-1-.json");
        assert_eq!(export_name(".hidden"), "hidden");
        assert_eq!(export_name("C:\\Users\\a.json"), "C-Users-a.json");
        assert_eq!(export_name(""), DEFAULT_NAME);
        assert_eq!(export_name("///"), DEFAULT_NAME);
        assert_eq!(export_name("\u{202e}nosj.exe"), "nosj.exe");
        assert_eq!(export_name(&"a".repeat(500)).len(), MAX_NAME_CHARS);
    }

    #[test]
    fn export_and_import_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contacts.json");
        write_export(&path, "[1]").unwrap();
        assert_eq!(read_import(&path).unwrap(), "[1]");
        // Replaces an existing file completely.
        write_export(&path, "{}").unwrap();
        assert_eq!(read_import(&path).unwrap(), "{}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "exported contacts are private");
        }
    }

    #[test]
    fn import_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");

        std::fs::write(&path, b"\xef\xbb\xbf{\"a\":1}").unwrap();
        assert_eq!(read_import(&path).unwrap(), "{\"a\":1}");

        std::fs::write(&path, vec![b' '; MAX_IMPORT_BYTES as usize]).unwrap();
        assert_eq!(read_import(&path).unwrap().len(), MAX_IMPORT_BYTES as usize);

        std::fs::write(&path, vec![b' '; MAX_IMPORT_BYTES as usize + 1]).unwrap();
        assert_eq!(read_import(&path).unwrap_err().code, "invalid_argument");

        std::fs::write(&path, b"\xff\xfe").unwrap();
        assert_eq!(read_import(&path).unwrap_err().code, "invalid_argument");

        assert!(read_import(dir.path()).is_err());
        assert_eq!(
            read_import(&dir.path().join("missing")).unwrap_err().code,
            "io_error"
        );
    }

    #[test]
    fn export_limit() {
        let dir = tempfile::tempdir().unwrap();
        let big = " ".repeat(MAX_EXPORT_BYTES + 1);
        let e = write_export(&dir.path().join("a.json"), &big).unwrap_err();
        assert_eq!(e.code, "invalid_argument");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
