//! High-resolution, compact icon for portable AppImage windows.
//! The installed app-menu icons keep their existing artwork and padding.
pub const ICON: &[u8] = include_bytes!("../icons/window-icon.png");

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
}
