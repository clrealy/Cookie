//! Baking: turning APKs into a `.cpk`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

use super::manifest::{
    BakedMeta, CookieMeta, CpkManifest, APP_DIR, CPK_FORMAT_VERSION, MANIFEST_ENTRY,
};
use crate::apk::{self, ApkInfo};
use crate::paths;
use crate::ui;
use crate::util;

/// Cookie's own icon, used when an APK hides its launcher icon behind a
/// resource reference we cannot resolve.
const FALLBACK_ICON: &[u8] = include_bytes!("../../assets/cookie-icon.png");

/// How a set of APKs was sorted out into a base plus splits.
#[derive(Debug)]
pub struct Sorted {
    pub base: ApkInfo,
    pub splits: Vec<ApkInfo>,
}

/// Work out which APK is the base and check the set hangs together.
pub fn sort_apks(apks: Vec<ApkInfo>) -> Result<Sorted> {
    if apks.is_empty() {
        bail!("no APKs to bake");
    }

    let (bases, splits): (Vec<ApkInfo>, Vec<ApkInfo>) =
        apks.into_iter().partition(|a| !a.manifest.is_split());

    let mut bases = bases;
    if bases.is_empty() {
        bail!(
            "none of these APKs is a base APK: every one declares a `split` name.\n\
             A split set needs its base APK too, usually called base.apk."
        );
    }
    if bases.len() > 1 {
        let names: Vec<String> = bases
            .iter()
            .map(|a| {
                format!(
                    "  {} ({})",
                    a.path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    a.manifest.package
                )
            })
            .collect();
        bail!(
            "found {} base APKs, but a cookie holds exactly one app:\n{}\n\
             Bake them separately.",
            bases.len(),
            names.join("\n")
        );
    }
    let base = match bases.pop() {
        Some(base) => base,
        // Unreachable given the checks above, but we do not panic to prove it.
        None => bail!("internal error: lost the base APK while sorting"),
    };

    // Everything has to belong to the same app.
    for split in &splits {
        if split.manifest.package != base.manifest.package {
            bail!(
                "{} belongs to {}, but the base APK is {}.\n\
                 These are different apps and cannot go in one cookie.",
                split
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("that split"),
                split.manifest.package,
                base.manifest.package
            );
        }
    }

    Ok(Sorted { base, splits })
}

/// Bake a sorted APK set into `out`.
///
/// Writes to a temporary file beside the target and renames on success, so an
/// interrupted bake never leaves a half-written `.cpk` behind.
pub fn bake(sorted: &Sorted, name_override: Option<&str>, out: &Path) -> Result<CpkManifest> {
    let base = &sorted.base;

    // Union of ABIs across base and splits.
    let mut abis: Vec<String> = base.abis.clone();
    for split in &sorted.splits {
        for abi in &split.abis {
            if !abis.contains(abi) {
                abis.push(abi.clone());
            }
        }
    }
    abis.sort();

    let name = name_override
        .map(str::to_string)
        .unwrap_or_else(|| base.display_name());

    // Pick an icon: base APK first, then splits, then our own.
    let (icon_bytes, icon_entry) = choose_icon(base, &sorted.splits)?;

    // Decide archive paths for every APK before writing anything.
    let base_entry = format!("{APP_DIR}/base.apk");
    let mut planned: Vec<(PathBuf, String)> = vec![(base.path.clone(), base_entry.clone())];
    let mut used: Vec<String> = vec![base_entry.clone()];
    for split in &sorted.splits {
        let file_name = split
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(paths::sanitize_file_name)
            .unwrap_or_else(|| {
                split
                    .manifest
                    .split
                    .as_deref()
                    .and_then(paths::sanitize_file_name)
                    .map(|s| format!("{s}.apk"))
                    .unwrap_or_else(|| "split.apk".to_string())
            });
        let mut entry = format!("{APP_DIR}/{file_name}");
        let mut n = 1;
        while used.contains(&entry) {
            entry = format!("{APP_DIR}/{n}_{file_name}");
            n += 1;
        }
        used.push(entry.clone());
        planned.push((split.path.clone(), entry));
    }

    let tmp_path = temp_sibling(out);
    if let Some(parent) = tmp_path.parent() {
        paths::ensure_dir(parent)?;
    }

    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let result = (|| -> Result<CpkManifest> {
        let file = File::create(&tmp_path)
            .with_context(|| format!("cannot create {}", tmp_path.display()))?;
        let mut zip = zip::ZipWriter::new(BufWriter::new(file));

        // APKs and icons are already compressed. Storing them keeps baking
        // fast and the result barely larger, and leaves the APKs readable at
        // a fixed offset for anything that wants to stream them.
        let stored = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .large_file(true);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        // Order is not load-bearing: a zip's central directory gives every
        // entry's offset, so `manifest.toml` and `icon.png` are read with a
        // single seek however large the APKs beside them are. That property
        // is the reason the format is a zip rather than a tarball.
        if let (Some(bytes), Some(entry)) = (&icon_bytes, &icon_entry) {
            zip.start_file(entry.as_str(), stored)
                .with_context(|| format!("cannot add {entry} to the cookie"))?;
            zip.write_all(bytes)
                .with_context(|| format!("cannot write {entry}"))?;
        }

        for (source, entry) in &planned {
            ui::step(&format!(
                "adding {}",
                source
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("an APK")
            ));
            zip.start_file(entry.as_str(), stored)
                .with_context(|| format!("cannot add {entry} to the cookie"))?;
            let mut reader = BufReader::new(
                File::open(source).with_context(|| format!("cannot open {}", source.display()))?,
            );
            std::io::copy(&mut reader, &mut zip)
                .with_context(|| format!("cannot copy {} into the cookie", source.display()))?;
            files.insert(entry.clone(), apk::file_digest(source)?);
        }

        let manifest = CpkManifest {
            cpk_version: CPK_FORMAT_VERSION,
            cookie: CookieMeta {
                name: name.clone(),
                package: base.manifest.package.clone(),
                version_name: base.manifest.version_name.clone(),
                version_code: base.manifest.version_code,
                abis: abis.clone(),
                icon: icon_entry.clone(),
                min_sdk: base.manifest.min_sdk,
                target_sdk: base.manifest.target_sdk,
                launchable_activity: base.manifest.launchable_activity.clone(),
            },
            baked: BakedMeta {
                by: format!("cookie {}", env!("CARGO_PKG_VERSION")),
                at: util::iso8601_utc(util::now_unix()),
                base_apk: base_entry.clone(),
                splits: planned
                    .iter()
                    .map(|(_, e)| e.clone())
                    .filter(|e| e != &base_entry)
                    .collect(),
                signature_sha256: apk::signing_digest(&base.path)?,
                files,
            },
        };

        let toml = manifest
            .to_toml()
            .context("cannot serialise manifest.toml")?;
        zip.start_file(MANIFEST_ENTRY, deflated)
            .context("cannot add manifest.toml to the cookie")?;
        zip.write_all(toml.as_bytes())
            .context("cannot write manifest.toml")?;

        let mut inner = zip.finish().context("cannot finalise the cookie archive")?;
        inner.flush().context("cannot flush the cookie archive")?;
        Ok(manifest)
    })();

    match result {
        Ok(manifest) => {
            fs::rename(&tmp_path, out).with_context(|| {
                format!(
                    "cannot move the finished cookie into place at {}",
                    out.display()
                )
            })?;
            Ok(manifest)
        }
        Err(err) => {
            let _ = fs::remove_file(&tmp_path);
            Err(err)
        }
    }
}

/// Find the best icon available, falling back to Cookie's own.
fn choose_icon(base: &ApkInfo, splits: &[ApkInfo]) -> Result<(Option<Vec<u8>>, Option<String>)> {
    let from = std::iter::once(base).chain(splits.iter());
    for info in from {
        if let Some(candidate) = &info.icon {
            match apk::read_icon(&info.path, candidate) {
                Ok(bytes) if !bytes.is_empty() => {
                    return Ok((Some(bytes), Some(format!("icon.{}", candidate.extension))));
                }
                Ok(_) => {}
                Err(err) => {
                    // A missing icon is cosmetic; say so and carry on.
                    ui::warn(&format!(
                        "could not read the icon out of {}: {err}",
                        info.path.display()
                    ));
                }
            }
        }
    }
    Ok((Some(FALLBACK_ICON.to_vec()), Some("icon.png".to_string())))
}

/// `foo.cpk` -> `foo.cpk.partial`
fn temp_sibling(out: &Path) -> PathBuf {
    let mut name = out
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("cookie.cpk")
        .to_string();
    name.push_str(".partial");
    match out.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk::manifest::ManifestInfo;

    fn fake(package: &str, split: Option<&str>) -> ApkInfo {
        ApkInfo {
            path: PathBuf::from(format!("/tmp/{}.apk", split.unwrap_or("base"))),
            manifest: ManifestInfo {
                package: package.to_string(),
                split: split.map(str::to_string),
                ..Default::default()
            },
            abis: Vec::new(),
            icon: None,
            size: 0,
        }
    }

    #[test]
    fn sorts_a_base_and_its_splits() {
        let sorted = sort_apks(vec![
            fake("a.b", Some("config.arm64_v8a")),
            fake("a.b", None),
        ])
        .expect("sorts");
        assert!(!sorted.base.manifest.is_split());
        assert_eq!(sorted.splits.len(), 1);
    }

    #[test]
    fn rejects_two_base_apks() {
        let err = sort_apks(vec![fake("a.b", None), fake("c.d", None)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("found 2 base APKs"), "{err}");
        assert!(err.contains("Bake them separately"), "{err}");
    }

    #[test]
    fn rejects_splits_with_no_base() {
        let err = sort_apks(vec![fake("a.b", Some("config.x86"))])
            .unwrap_err()
            .to_string();
        assert!(err.contains("none of these APKs is a base APK"), "{err}");
    }

    #[test]
    fn rejects_a_split_from_another_app() {
        let err = sort_apks(vec![fake("a.b", None), fake("x.y", Some("config.arm"))])
            .unwrap_err()
            .to_string();
        assert!(err.contains("different apps"), "{err}");
    }

    #[test]
    fn rejects_an_empty_set() {
        assert!(sort_apks(Vec::new()).is_err());
    }

    #[test]
    fn temp_file_sits_beside_the_target() {
        assert_eq!(
            temp_sibling(Path::new("/a/b/teapot.cpk")),
            PathBuf::from("/a/b/teapot.cpk.partial")
        );
        assert_eq!(
            temp_sibling(Path::new("teapot.cpk")),
            PathBuf::from("teapot.cpk.partial")
        );
    }

    #[test]
    fn fallback_icon_is_a_real_png() {
        assert!(FALLBACK_ICON.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(FALLBACK_ICON.len() > 100);
    }
}
