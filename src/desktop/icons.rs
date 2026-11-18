//! Putting icons where icon themes can find them.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::paths;

/// The directories `hicolor`'s own `index.theme` defines for applications.
///
/// An icon has to land in one of these to be found by name. Anything of a
/// different size is referenced by absolute path instead.
const HICOLOR_SIZES: &[u32] = &[16, 22, 24, 32, 36, 48, 64, 72, 96, 128, 192, 256, 512];

/// How a `.desktop` file should refer to an icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconRef {
    /// A themed name, e.g. `cookie-com.example.teapot`. Scales properly and
    /// gets cached by the desktop environment.
    Themed(String),
    /// An absolute path, for icons that cannot go in the theme.
    Path(PathBuf),
    /// No icon available at all.
    None,
}

impl IconRef {
    /// The value for the `Icon=` key, if there is one.
    pub fn value(&self) -> Option<String> {
        match self {
            IconRef::Themed(name) => Some(name.clone()),
            IconRef::Path(path) => Some(path.display().to_string()),
            IconRef::None => None,
        }
    }
}

/// Read a PNG's dimensions out of its IHDR chunk.
///
/// The header is fixed-layout: 8 bytes of signature, then a chunk length and
/// the tag `IHDR`, then width and height as big-endian u32.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || !bytes.starts_with(SIGNATURE) {
        return None;
    }
    if &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

/// The hicolor directory a square icon of this size belongs in, if any.
fn hicolor_size(width: u32, height: u32) -> Option<u32> {
    if width != height {
        return None;
    }
    HICOLOR_SIZES.contains(&width).then_some(width)
}

/// Install an icon into the hicolor theme, or fall back to a path reference.
///
/// `name` is the themed icon name, without an extension.
/// `fallback` is where to point when the icon cannot be themed — normally the
/// copy already sitting in the app's jar directory.
pub fn install(
    icons_root: &Path,
    name: &str,
    bytes: &[u8],
    context: &str,
    fallback: Option<&Path>,
) -> Result<IconRef> {
    install_in(icons_root, "apps", name, bytes, context, fallback)
}

/// As [`install`], but into a named hicolor context such as `mimetypes`.
pub fn install_in(
    icons_root: &Path,
    hicolor_context: &str,
    name: &str,
    bytes: &[u8],
    description: &str,
    fallback: Option<&Path>,
) -> Result<IconRef> {
    let Some((width, height)) = png_size(bytes) else {
        // Not a PNG we understand — a WebP icon, most likely. Icon themes
        // only index PNG, SVG and XPM, so point at the file directly.
        return Ok(match fallback {
            Some(path) if path.exists() => IconRef::Path(path.to_path_buf()),
            _ => IconRef::None,
        });
    };

    let Some(size) = hicolor_size(width, height) else {
        // A real PNG, but not a size hicolor indexes.
        return Ok(match fallback {
            Some(path) if path.exists() => IconRef::Path(path.to_path_buf()),
            _ => IconRef::None,
        });
    };

    let dir = icons_root
        .join("hicolor")
        .join(format!("{size}x{size}"))
        .join(hicolor_context);
    paths::ensure_dir(&dir)?;

    let target = dir.join(format!("{name}.png"));
    fs::write(&target, bytes).with_context(|| {
        format!(
            "cannot install the {description} icon at {}",
            target.display()
        )
    })?;

    Ok(IconRef::Themed(name.to_string()))
}

/// Remove every themed copy of an icon.
pub fn remove(icons_root: &Path, name: &str) -> Result<usize> {
    let mut removed = 0;
    for size in HICOLOR_SIZES {
        for context in ["apps", "mimetypes"] {
            let path = icons_root
                .join("hicolor")
                .join(format!("{size}x{size}"))
                .join(context)
                .join(format!("{name}.png"));
            if path.exists() {
                fs::remove_file(&path)
                    .with_context(|| format!("cannot remove {}", path.display()))?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets;
    use crate::paths::ScratchDir;

    #[test]
    fn reads_the_size_of_our_own_icon() {
        assert_eq!(png_size(assets::COOKIE_ICON_256), Some((256, 256)));
        assert_eq!(png_size(assets::COOKIE_ICON_128), Some((128, 128)));
    }

    #[test]
    fn rejects_things_that_are_not_pngs() {
        assert_eq!(png_size(b""), None);
        assert_eq!(png_size(b"RIFF....WEBPVP8 "), None);
        assert_eq!(png_size(&[0u8; 64]), None);
        // Right signature, wrong chunk.
        let mut fake = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        fake.extend_from_slice(&[0, 0, 0, 13]);
        fake.extend_from_slice(b"IDAT");
        fake.extend_from_slice(&[0u8; 8]);
        assert_eq!(png_size(&fake), None);
    }

    #[test]
    fn rejects_a_zero_sized_png() {
        let mut fake = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        fake.extend_from_slice(&[0, 0, 0, 13]);
        fake.extend_from_slice(b"IHDR");
        fake.extend_from_slice(&0u32.to_be_bytes());
        fake.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(png_size(&fake), None);
    }

    #[test]
    fn only_standard_square_sizes_are_themed() {
        assert_eq!(hicolor_size(256, 256), Some(256));
        assert_eq!(hicolor_size(48, 48), Some(48));
        assert_eq!(hicolor_size(144, 144), None, "144 is not a hicolor size");
        assert_eq!(hicolor_size(256, 128), None, "not square");
    }

    #[test]
    fn a_standard_png_goes_into_the_theme() {
        let scratch = ScratchDir::new("icons").expect("scratch");
        let icons = scratch.path().join("icons");
        let result =
            install(&icons, "cookie", assets::COOKIE_ICON_256, "test", None).expect("install");
        assert_eq!(result, IconRef::Themed("cookie".to_string()));
        assert!(icons.join("hicolor/256x256/apps/cookie.png").is_file());
        assert_eq!(result.value().as_deref(), Some("cookie"));
    }

    #[test]
    fn a_mimetype_icon_goes_into_its_own_context() {
        let scratch = ScratchDir::new("icons").expect("scratch");
        let icons = scratch.path().join("icons");
        install_in(
            &icons,
            "mimetypes",
            "application-x-cookie-package",
            assets::COOKIE_ICON_256,
            "test",
            None,
        )
        .expect("install");
        assert!(icons
            .join("hicolor/256x256/mimetypes/application-x-cookie-package.png")
            .is_file());
    }

    #[test]
    fn an_unthemable_icon_falls_back_to_its_path() {
        let scratch = ScratchDir::new("icons").expect("scratch");
        let icons = scratch.path().join("icons");
        let fallback = scratch.path().join("icon.webp");
        fs::write(&fallback, b"RIFF....WEBP").expect("write");

        let result =
            install(&icons, "app", b"RIFF....WEBP", "test", Some(&fallback)).expect("install");
        assert_eq!(result, IconRef::Path(fallback.clone()));
        assert_eq!(
            result.value().as_deref(),
            Some(fallback.to_str().unwrap_or(""))
        );
    }

    #[test]
    fn no_icon_and_no_fallback_means_no_icon() {
        let scratch = ScratchDir::new("icons").expect("scratch");
        let result = install(
            &scratch.path().join("icons"),
            "app",
            b"not a png",
            "test",
            None,
        )
        .expect("install");
        assert_eq!(result, IconRef::None);
        assert_eq!(result.value(), None);
    }

    #[test]
    fn removal_cleans_every_size() {
        let scratch = ScratchDir::new("icons").expect("scratch");
        let icons = scratch.path().join("icons");
        install(&icons, "cookie", assets::COOKIE_ICON_256, "test", None).expect("256");
        install(&icons, "cookie", assets::COOKIE_ICON_128, "test", None).expect("128");

        assert_eq!(remove(&icons, "cookie").expect("remove"), 2);
        assert!(!icons.join("hicolor/256x256/apps/cookie.png").exists());
        assert!(!icons.join("hicolor/128x128/apps/cookie.png").exists());
        // Removing again is fine.
        assert_eq!(remove(&icons, "cookie").expect("remove"), 0);
    }
}
