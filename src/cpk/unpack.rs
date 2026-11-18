//! Reading a `.cpk` back out again.
//!
//! Because a cookie is a zip, `manifest.toml` and `icon.png` can be read on
//! their own without touching the APKs next to them. `cookie jar` leans on
//! that: listing twenty installed apps stays instant no matter how big they
//! are.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use super::manifest::{CpkManifest, MANIFEST_ENTRY};

/// Read just the metadata out of a `.cpk`.
pub fn read_manifest(path: &Path) -> Result<CpkManifest> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).with_context(|| {
        format!(
            "{} is not a readable cookie.\n\
             A .cpk is a zip archive; this file does not look like one.",
            path.display()
        )
    })?;

    let mut entry = zip.by_name(MANIFEST_ENTRY).map_err(|_| {
        anyhow!(
            "{} has no {MANIFEST_ENTRY}, so it is not a cookie.\n\
             If you meant to install an APK, bake it first: cookie bake <app.apk>",
            path.display()
        )
    })?;

    let mut text = String::new();
    entry
        .read_to_string(&mut text)
        .with_context(|| format!("cannot read {MANIFEST_ENTRY} from {}", path.display()))?;

    CpkManifest::from_toml(&text).with_context(|| format!("cannot understand {}", path.display()))
}

/// Read the icon out of a `.cpk`, if it has one.
pub fn read_icon(path: &Path, manifest: &CpkManifest) -> Result<Option<Vec<u8>>> {
    let Some(entry_name) = &manifest.cookie.icon else {
        return Ok(None);
    };
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("cannot read {} as a zip", path.display()))?;
    let mut entry = match zip.by_name(entry_name) {
        Ok(e) => e,
        // A missing icon is cosmetic, never fatal.
        Err(_) => return Ok(None),
    };
    let mut buf = Vec::with_capacity(entry.size().min(16 << 20) as usize);
    entry
        .read_to_end(&mut buf)
        .with_context(|| format!("cannot read {entry_name} from {}", path.display()))?;
    Ok(Some(buf))
}

/// Extract every APK from a cookie into `dest`, returning their paths with
/// the base APK first.
///
/// Entry names come from the cookie's own manifest and are reduced to a bare
/// filename before use, so nothing can be written outside `dest`.
pub fn extract_apks(path: &Path, manifest: &CpkManifest, dest: &Path) -> Result<Vec<PathBuf>> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("cannot read {} as a zip", path.display()))?;

    let mut out = Vec::new();
    for entry_name in manifest.apk_entries() {
        let safe = crate::paths::sanitize_file_name(entry_name).ok_or_else(|| {
            anyhow!(
                "{} declares an APK with an unusable name: {entry_name}",
                path.display()
            )
        })?;
        let target = dest.join(&safe);

        let mut entry = zip.by_name(entry_name).map_err(|_| {
            anyhow!(
                "{} is incomplete: its manifest lists {entry_name}, but the file is not there.\n\
                 Re-bake the app from its APK.",
                path.display()
            )
        })?;

        let mut writer =
            File::create(&target).with_context(|| format!("cannot write {}", target.display()))?;
        io::copy(&mut entry, &mut writer)
            .with_context(|| format!("cannot extract {entry_name}"))?;
        out.push(target);
    }

    if out.is_empty() {
        bail!("{} contains no APKs", path.display());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_a_non_zip_gives_a_helpful_error() {
        let dir = crate::paths::ScratchDir::new("test-unpack").expect("scratch");
        let fake = dir.path().join("not.cpk");
        std::fs::write(&fake, b"definitely not a zip").expect("write");
        let err = read_manifest(&fake).unwrap_err().to_string();
        assert!(err.contains("not a readable cookie"), "{err}");
    }

    #[test]
    fn reading_a_missing_file_gives_a_helpful_error() {
        let err = read_manifest(Path::new("/nope/missing.cpk"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot open"), "{err}");
    }
}
