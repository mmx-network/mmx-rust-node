//! Files chosen by the user in a save or open dialog (contact lists).
use mmxwallet::service::{Error, Result};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

/// Largest file that can be imported: room for the largest contact list
/// (1000 contacts with the longest names and notes, about 1.2 MB).
pub const MAX_IMPORT_BYTES: u64 = 2 * 1024 * 1024;
/// Largest file that can be exported: the same, so every exported file can
/// be imported again.
pub const MAX_EXPORT_BYTES: usize = MAX_IMPORT_BYTES as usize;
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
/// mark is dropped. A file that cannot be opened or read is an `io_error`;
/// one that is too large or not text is an `invalid_argument`.
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

    /// A file that cannot be read is reported as a read failure, not as a
    /// file without contacts.
    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_an_io_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        std::fs::write(&path, b"[]").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Permissions do not apply to a superuser.
        if File::open(&path).is_err() {
            assert_eq!(read_import(&path).unwrap_err().code, "io_error");
        }
    }

    /// A number from the wallet UI's src/lib/appstate.ts.
    fn ui_limit(name: &str) -> usize {
        let source = include_str!("../../src/lib/appstate.ts");
        let start = source
            .find(&format!("export const {name} = "))
            .unwrap_or_else(|| panic!("{name} not found"))
            + name.len()
            + 16;
        source[start..]
            .split(';')
            .next()
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_else(|| panic!("{name} is not a number"))
    }

    /// The largest contacts file the UI can export (serializeContacts in
    /// appstate.ts: pretty JSON with two spaces) can be imported again.
    #[test]
    fn the_largest_contact_list_round_trips() {
        let (count, name, note) = (
            ui_limit("MAX_CONTACTS"),
            ui_limit("CONTACT_NAME_MAX"),
            ui_limit("CONTACT_NOTE_MAX"),
        );
        assert!(count >= 1000 && name >= 64 && note >= 200);
        let contact = serde_json::json!({
            "name": "\u{1f511}".repeat(name),
            "address": "mmx1mktnzjz82tzre764afvttx0rj2886g24gnmuzxntcvnzdnm9kkvsqwz36c",
            "note": "\u{1f511}".repeat(note),
        });
        let file = serde_json::json!({
            "format": "mmx-wallet-contacts",
            "version": 1,
            "contacts": vec![contact; count],
        });
        let contents = serde_json::to_string_pretty(&file).unwrap() + "\n";
        assert!(contents.len() > 1024 * 1024, "{}", contents.len());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contacts.json");
        write_export(&path, &contents).unwrap();
        assert_eq!(read_import(&path).unwrap(), contents);
    }

    #[test]
    fn export_limit() {
        let dir = tempfile::tempdir().unwrap();
        let big = " ".repeat(MAX_EXPORT_BYTES + 1);
        let e = write_export(&dir.path().join("a.json"), &big).unwrap_err();
        assert_eq!(e.code, "invalid_argument");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // The largest export can be imported.
        let path = dir.path().join("b.json");
        write_export(&path, &big[1..]).unwrap();
        assert_eq!(read_import(&path).unwrap().len(), MAX_EXPORT_BYTES);
    }
}
