//! Files compiled into the binary.

/// Cookie's own icon, 256x256 PNG with transparency.
///
/// Used as the launcher icon for Cookie itself, as the icon shown for `.cpk`
/// files in a file manager, and as the fallback when an APK's own launcher
/// icon cannot be found.
pub const COOKIE_ICON_256: &[u8] = include_bytes!("../assets/cookie-icon.png");

/// The same icon at 128x128, for icon themes that want a smaller variant.
pub const COOKIE_ICON_128: &[u8] = include_bytes!("../assets/cookie-128.png");
