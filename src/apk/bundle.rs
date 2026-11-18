//! Turning whatever the user handed us into a flat list of APK files.
//!
//! `cookie bake` accepts a single APK, several APKs (a base plus its splits),
//! or one bundle file: `.xapk`, `.apks` or `.apkm`. Bundles are themselves
//! zips with the APKs inside, so we unpack them into scratch space first.

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::paths::{self, ScratchDir};

/// Extensions we recognise as "a zip full of APKs".
const BUNDLE_EXTENSIONS: &[&str] = &["xapk", "apks", "apkm"];

/// The APKs to bake, plus the scratch directory keeping extracted ones alive.
#[derive(Debug)]
pub struct Inputs {
    pub apks: Vec<PathBuf>,
    /// Dropped last, which deletes anything we unpacked.
    _scratch: Option<ScratchDir>,
}

/// Does this path look like a bundle rather than a plain APK?
pub fn is_bundle(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| BUNDLE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Resolve CLI arguments into APK files on disk.
pub fn resolve(args: &[PathBuf]) -> Result<Inputs> {
    if args.is_empty() {
        bail!("no input files given");
    }

    for path in args {
        if !path.exists() {
            bail!("{} does not exist", path.display());
        }
        if path.is_dir() {
            bail!(
                "{} is a directory.\n\
                 Pass the APK files themselves, for example:\n  \
                 cookie bake {}/base.apk {}/split_config.arm64_v8a.apk",
                path.display(),
                path.display(),
                path.display()
            );
        }
    }

    let bundles: Vec<&PathBuf> = args.iter().filter(|p| is_bundle(p)).collect();

    if !bundles.is_empty() {
        if args.len() > 1 {
            bail!(
                "{} is a bundle, so it has to be baked on its own.\n\
                 Run: cookie bake {}",
                bundles[0].display(),
                bundles[0].display()
            );
        }
        let scratch = ScratchDir::new("bake")?;
        let apks = unpack_bundle(&args[0], scratch.path())?;
        return Ok(Inputs {
            apks,
            _scratch: Some(scratch),
        });
    }

    // Plain APKs: use them where they are.
    for path in args {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if ext.as_deref() != Some("apk") {
            bail!(
                "{} does not look like an APK or a bundle.\n\
                 Cookie bakes .apk files, or one .xapk / .apks / .apkm bundle.",
                path.display()
            );
        }
    }

    Ok(Inputs {
        apks: args.to_vec(),
        _scratch: None,
    })
}

/// Extract every APK from a bundle zip into `dest`.
///
/// Entry names are reduced to their final component before use, so a hostile
/// archive cannot write outside `dest` no matter what paths it declares.
fn unpack_bundle(bundle: &Path, dest: &Path) -> Result<Vec<PathBuf>> {
    let file = File::open(bundle).with_context(|| format!("cannot open {}", bundle.display()))?;
    let mut zip = zip::ZipArchive::new(file).with_context(|| {
        format!(
            "{} is not a readable bundle. Bundles are zip files containing APKs.",
            bundle.display()
        )
    })?;

    let mut written = Vec::new();
    let mut used_names: HashSet<String> = HashSet::new();

    for i in 0..zip.len() {
        let mut entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(err) => {
                bail!("cannot read entry {i} of {}: {err}", bundle.display());
            }
        };
        if entry.is_dir() {
            continue;
        }
        let raw_name = match entry.name() {
            Ok(name) => name.into_owned(),
            // An entry with an unrepresentable name is not something we can
            // use; skip it rather than fail the whole bundle.
            Err(_) => continue,
        };
        if !raw_name.to_ascii_lowercase().ends_with(".apk") {
            continue;
        }

        let base = paths::sanitize_file_name(&raw_name)
            .ok_or_else(|| anyhow!("{} contains an APK with an unusable name", bundle.display()))?;

        let mut name = base.clone();
        let mut n = 1;
        while !used_names.insert(name.clone()) {
            name = format!("{n}_{base}");
            n += 1;
        }

        let out_path = dest.join(&name);
        let mut out = File::create(&out_path)
            .with_context(|| format!("cannot write {}", out_path.display()))?;
        io::copy(&mut entry, &mut out)
            .with_context(|| format!("cannot extract {raw_name} from {}", bundle.display()))?;
        written.push(out_path);
    }

    if written.is_empty() {
        bail!(
            "{} contains no APK files.\n\
             If this is a store download, it may need unpacking first.",
            bundle.display()
        );
    }

    written.sort();
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_detection() {
        assert!(is_bundle(Path::new("a.xapk")));
        assert!(is_bundle(Path::new("a.APKS")));
        assert!(is_bundle(Path::new("a.apkm")));
        assert!(!is_bundle(Path::new("a.apk")));
        assert!(!is_bundle(Path::new("a")));
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(resolve(&[]).is_err());
    }

    #[test]
    fn missing_file_is_rejected() {
        let err = resolve(&[PathBuf::from("/nope/definitely-not-here.apk")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not exist"), "{err}");
    }
}
