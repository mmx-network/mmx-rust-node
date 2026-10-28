//! Window and host-theme icons for portable AppImages.
//! The installed app-menu icons keep their existing artwork and padding.
use std::{
    fs, io,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub const ICON: &[u8] = include_bytes!("../icons/window-icon.png");

const DOCK_ICONS: [(u32, &[u8]); 2] = [
    (48, include_bytes!("../icons/48x48.png")),
    (64, include_bytes!("../icons/64x64.png")),
];

pub fn data_directory() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        })
}

/// GNOME can match an AppImage window to an installed deb launcher, ignoring
/// its window icons. Provide exact dock sizes through the host's icon theme.
/// Large app-menu icons are deliberately left to the existing installation.
pub fn install_dock_icons(data: &Path) -> io::Result<bool> {
    let theme = data.join("icons/hicolor");
    let mut changed = false;
    for (size, contents) in DOCK_ICONS {
        let path = theme.join(format!("{size}x{size}/apps/mmx-wallet-gui.png"));
        let mut previous = Vec::new();
        if let Ok(file) = fs::File::open(&path) {
            file.take(contents.len() as u64 + 1)
                .read_to_end(&mut previous)?;
        }
        if previous == contents {
            continue;
        }
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        std::io::Write::write_all(&mut file, contents)?;
        file.persist(path).map_err(|error| error.error)?;
        changed = true;
    }
    if changed {
        // Icon-theme consumers monitor the top-level directory's mtime.
        fs::File::open(&theme)?.set_modified(SystemTime::now())?;
        fs::File::open(data.join("icons"))?.set_modified(SystemTime::now())?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_icon_has_enough_pixels_for_high_density_docks() {
        assert_eq!(&ICON[..8], b"\x89PNG\r\n\x1a\n");
        let width = u32::from_be_bytes(ICON[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(ICON[20..24].try_into().unwrap());
        assert!(width >= 512 && height >= 512);
    }

    #[test]
    fn dock_icons_upgrade_without_changing_menu_artwork_or_rewriting_on_startup() {
        let data = tempfile::tempdir().unwrap();
        let menu = data
            .path()
            .join("icons/hicolor/128x128/apps/mmx-wallet-gui.png");
        fs::create_dir_all(menu.parent().unwrap()).unwrap();
        fs::write(&menu, b"existing menu icon").unwrap();
        assert!(install_dock_icons(data.path()).unwrap());
        let theme = data.path().join("icons/hicolor");
        let modified = theme.metadata().unwrap().modified().unwrap();
        assert!(!install_dock_icons(data.path()).unwrap());
        assert_eq!(theme.metadata().unwrap().modified().unwrap(), modified);
        assert_eq!(fs::read(&menu).unwrap(), b"existing menu icon");
        for (size, contents) in DOCK_ICONS {
            let path = theme.join(format!("{size}x{size}/apps/mmx-wallet-gui.png"));
            assert_eq!(fs::read(&path).unwrap(), contents);
            fs::write(path, b"old dock icon").unwrap();
        }
        assert!(install_dock_icons(data.path()).unwrap());
    }
}
