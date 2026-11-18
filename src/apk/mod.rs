//! Reading facts out of APK files.
//!
//! An APK is a zip. We read its `AndroidManifest.xml` (compiled binary XML),
//! derive supported ABIs from the `lib/` layout, and pick a launcher icon.

pub mod axml;
#[cfg(test)]
pub mod axml_fixture;
pub mod bundle;
pub mod manifest;
pub mod signing;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

pub use manifest::ManifestInfo;

/// Everything Cookie learns from one APK on disk.
#[derive(Debug, Clone)]
pub struct ApkInfo {
    pub path: PathBuf,
    pub manifest: ManifestInfo,
    /// ABI directories present under `lib/`, e.g. `["arm64-v8a"]`. Empty means
    /// the APK carries no native code and runs on any ABI.
    pub abis: Vec<String>,
    /// Best guess at a launcher icon inside the APK, if there is one.
    pub icon: Option<IconCandidate>,
    /// Size of the file on disk, in bytes.
    pub size: u64,
}

impl ApkInfo {
    /// A display name for the app: the literal manifest label when we have
    /// one, otherwise the last component of the package id, capitalised.
    pub fn display_name(&self) -> String {
        if let Some(label) = &self.manifest.label {
            return label.clone();
        }
        let tail = self
            .manifest
            .package
            .rsplit('.')
            .next()
            .unwrap_or(&self.manifest.package);
        let mut chars = tail.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => self.manifest.package.clone(),
        }
    }
}

/// A candidate icon entry inside an APK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconCandidate {
    /// Path of the zip entry.
    pub entry: String,
    /// File extension, lowercased, without the dot: `png` or `webp`.
    pub extension: String,
    /// Uncompressed size, used as a tie-breaker.
    pub size: u64,
    /// Ranking score; higher is better. Exposed for tests.
    pub score: i32,
}

/// Read an APK and report what we found.
pub fn inspect(path: &Path) -> Result<ApkInfo> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let size = file
        .metadata()
        .map(|m| m.len())
        .with_context(|| format!("cannot stat {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a valid APK (zip) file", path.display()))?;

    let manifest_bytes = {
        let mut entry = zip.by_name("AndroidManifest.xml").map_err(|_| {
            anyhow!(
                "{} has no AndroidManifest.xml, so it is not an APK",
                path.display()
            )
        })?;
        let mut buf = Vec::with_capacity(entry.size().min(4 << 20) as usize);
        entry
            .read_to_end(&mut buf)
            .with_context(|| format!("cannot read AndroidManifest.xml from {}", path.display()))?;
        buf
    };

    let manifest = manifest::parse(&manifest_bytes)
        .with_context(|| format!("cannot understand the manifest in {}", path.display()))?;

    let mut abis = Vec::new();
    let mut icons: Vec<IconCandidate> = Vec::new();

    for i in 0..zip.len() {
        let entry = match zip.by_index_raw(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = match entry.name() {
            Ok(name) => name.into_owned(),
            Err(_) => continue,
        };

        if let Some(abi) = abi_from_lib_entry(&name) {
            if !abis.iter().any(|a| a == &abi) {
                abis.push(abi);
            }
        }

        if let Some(candidate) = score_icon_entry(&name, entry.size()) {
            icons.push(candidate);
        }
    }

    abis.sort();
    icons.sort_by(|a, b| b.score.cmp(&a.score).then(b.size.cmp(&a.size)));

    Ok(ApkInfo {
        path: path.to_path_buf(),
        manifest,
        abis,
        icon: icons.into_iter().next(),
        size,
    })
}

/// Pull an icon's bytes out of an APK.
pub fn read_icon(path: &Path, candidate: &IconCandidate) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("cannot read {} as a zip", path.display()))?;
    let mut entry = zip
        .by_name(&candidate.entry)
        .with_context(|| format!("{} vanished from {}", candidate.entry, path.display()))?;
    let mut buf = Vec::with_capacity(entry.size().min(16 << 20) as usize);
    entry
        .read_to_end(&mut buf)
        .with_context(|| format!("cannot read {}", candidate.entry))?;
    Ok(buf)
}

/// `lib/arm64-v8a/libfoo.so` -> `arm64-v8a`.
fn abi_from_lib_entry(name: &str) -> Option<String> {
    let rest = name.strip_prefix("lib/")?;
    let (abi, tail) = rest.split_once('/')?;
    // Only count directories that actually hold something.
    if abi.is_empty() || tail.is_empty() {
        return None;
    }
    Some(abi.to_string())
}

/// Density buckets, worst to best. The index doubles as a score.
const DENSITIES: &[&str] = &[
    "ldpi", "mdpi", "hdpi", "xhdpi", "xxhdpi", "xxxhdpi", "anydpi",
];

/// Decide whether a zip entry looks like a launcher icon, and how good a
/// candidate it is.
///
/// Resolving `android:icon` properly means parsing `resources.arsc`, which we
/// deliberately avoid in v1 of the format. This heuristic covers the
/// overwhelming majority of real apps, and `bake` falls back to Cookie's own
/// icon when it finds nothing.
fn score_icon_entry(name: &str, size: u64) -> Option<IconCandidate> {
    let lower = name.to_ascii_lowercase();
    if !lower.starts_with("res/") {
        return None;
    }
    let extension = lower.rsplit_once('.').map(|(_, e)| e)?;
    if extension != "png" && extension != "webp" {
        return None;
    }

    let stem = lower.rsplit_once('/').map(|(_, f)| f).unwrap_or(&lower);
    let looks_like_icon = ["ic_launcher", "launcher", "ic_app", "app_icon", "appicon"]
        .iter()
        .any(|needle| stem.contains(needle));
    if !looks_like_icon {
        return None;
    }

    let mut score = 0;

    // Adaptive-icon layers are a last resort: on their own they render as a
    // bare foreground with no background.
    if stem.contains("foreground") || stem.contains("background") || stem.contains("monochrome") {
        score -= 40;
    }
    // A round icon is a fine fallback but the square one is the default.
    if stem.contains("round") {
        score -= 5;
    }
    // `mipmap/` is where launcher icons belong; `drawable/` is everything else.
    if lower.starts_with("res/mipmap") {
        score += 20;
    }
    for (i, density) in DENSITIES.iter().enumerate() {
        if lower.contains(density) {
            score += (i as i32 + 1) * 2;
            break;
        }
    }
    // Prefer PNG: we can hand it straight to a .desktop file.
    if extension == "png" {
        score += 3;
    }

    Some(IconCandidate {
        entry: name.to_string(),
        extension: extension.to_string(),
        size,
        score,
    })
}

/// SHA-256 of a whole file, as lowercase hex.
pub fn file_digest(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_extraction() {
        assert_eq!(
            abi_from_lib_entry("lib/arm64-v8a/libfoo.so").as_deref(),
            Some("arm64-v8a")
        );
        assert_eq!(
            abi_from_lib_entry("lib/x86_64/libbar.so").as_deref(),
            Some("x86_64")
        );
        assert_eq!(abi_from_lib_entry("lib/arm64-v8a/"), None);
        assert_eq!(abi_from_lib_entry("res/drawable/x.png"), None);
        assert_eq!(abi_from_lib_entry("lib/"), None);
    }

    #[test]
    fn mipmap_beats_drawable() {
        let m = score_icon_entry("res/mipmap-xxxhdpi/ic_launcher.png", 100).expect("icon");
        let d = score_icon_entry("res/drawable-xxxhdpi/ic_launcher.png", 100).expect("icon");
        assert!(m.score > d.score, "{} vs {}", m.score, d.score);
    }

    #[test]
    fn higher_density_beats_lower() {
        let hi = score_icon_entry("res/mipmap-xxxhdpi/ic_launcher.png", 10).expect("icon");
        let lo = score_icon_entry("res/mipmap-mdpi/ic_launcher.png", 10).expect("icon");
        assert!(hi.score > lo.score);
    }

    #[test]
    fn adaptive_layers_are_a_last_resort() {
        let full = score_icon_entry("res/mipmap-hdpi/ic_launcher.png", 10).expect("icon");
        let fg = score_icon_entry("res/mipmap-hdpi/ic_launcher_foreground.png", 10).expect("icon");
        assert!(full.score > fg.score);
    }

    #[test]
    fn png_beats_webp_at_equal_density() {
        let png = score_icon_entry("res/mipmap-hdpi/ic_launcher.png", 10).expect("icon");
        let webp = score_icon_entry("res/mipmap-hdpi/ic_launcher.webp", 10).expect("icon");
        assert!(png.score > webp.score);
    }

    #[test]
    fn ignores_unrelated_resources() {
        assert!(score_icon_entry("res/drawable/background.png", 10).is_none());
        assert!(score_icon_entry("res/mipmap-hdpi/ic_launcher.xml", 10).is_none());
        assert!(score_icon_entry("assets/ic_launcher.png", 10).is_none());
        assert!(score_icon_entry("res/mipmap-hdpi/notes.txt", 10).is_none());
    }
}
