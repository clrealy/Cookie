//! The jar: Cookie's record of what is installed.
//!
//! One directory per app under `~/.local/share/cookie/jar/<package>/`, holding
//! the cookie's `manifest.toml`, its icon, and an `installed.toml` noting when
//! and from where it was installed.
//!
//! The APKs themselves are not kept: they already live inside the Android
//! runtime, and a second copy of a 300 MB app helps nobody.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cpk::manifest::CpkManifest;
use crate::paths;
use crate::util;

const INSTALLED_FILE: &str = "installed.toml";
const MANIFEST_FILE: &str = "manifest.toml";

/// What Cookie records at install time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledRecord {
    /// Unix timestamp of the install.
    pub installed_at: i64,
    /// Which backend it went into.
    pub backend: String,
    /// Where the `.cpk` was when we installed it, for reference only.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_cpk: Option<String>,
    /// Icon filename inside the cookie's jar directory.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub icon: Option<String>,
    /// Signing certificate digest at install time. Milestone 3 compares this
    /// before updating an app in place.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature_sha256: Option<String>,
}

/// One installed cookie, as read back off disk.
#[derive(Debug, Clone)]
pub struct Cookie {
    pub package: String,
    pub manifest: CpkManifest,
    pub record: InstalledRecord,
    pub dir: PathBuf,
}

impl Cookie {
    pub fn name(&self) -> &str {
        &self.manifest.cookie.name
    }

    /// Absolute path to this cookie's icon, if it has one on disk.
    pub fn icon_path(&self) -> Option<PathBuf> {
        let name = self.record.icon.as_ref()?;
        let path = self.dir.join(name);
        path.exists().then_some(path)
    }

    pub fn installed_at(&self) -> String {
        util::human_datetime(self.record.installed_at)
    }
}

/// Record an install in the jar.
pub fn put(
    manifest: &CpkManifest,
    icon: Option<&[u8]>,
    source_cpk: Option<&Path>,
    backend: &str,
) -> Result<PathBuf> {
    put_in(&paths::jar_dir()?, manifest, icon, source_cpk, backend)
}

/// As [`put`], but into a jar directory of your choosing.
///
/// Every function here comes in a `_in` form taking the jar root explicitly.
/// It keeps the real work free of any dependency on `$HOME`, which means the
/// tests exercise the same code the binary runs instead of a stand-in.
pub fn put_in(
    jar: &Path,
    manifest: &CpkManifest,
    icon: Option<&[u8]>,
    source_cpk: Option<&Path>,
    backend: &str,
) -> Result<PathBuf> {
    let dir = jar.join(&manifest.cookie.package);
    paths::ensure_dir(&dir)?;

    let icon_name = match (icon, &manifest.cookie.icon) {
        (Some(bytes), Some(entry)) if !bytes.is_empty() => {
            // Keep whatever extension the cookie used.
            let ext = Path::new(entry)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("png");
            let name = format!("icon.{ext}");
            fs::write(dir.join(&name), bytes)
                .with_context(|| format!("cannot write the icon into {}", dir.display()))?;
            Some(name)
        }
        _ => None,
    };

    let record = InstalledRecord {
        installed_at: util::now_unix(),
        backend: backend.to_string(),
        source_cpk: source_cpk.map(|p| p.display().to_string()),
        icon: icon_name,
        signature_sha256: manifest.baked.signature_sha256.clone(),
    };

    let manifest_toml = manifest
        .to_toml()
        .context("cannot serialise the cookie's manifest")?;
    fs::write(dir.join(MANIFEST_FILE), manifest_toml)
        .with_context(|| format!("cannot write {MANIFEST_FILE} into {}", dir.display()))?;

    let record_toml =
        toml::to_string_pretty(&record).context("cannot serialise the install record")?;
    fs::write(dir.join(INSTALLED_FILE), record_toml)
        .with_context(|| format!("cannot write {INSTALLED_FILE} into {}", dir.display()))?;

    Ok(dir)
}

/// Forget an installed cookie.
pub fn remove(package: &str) -> Result<()> {
    remove_in(&paths::jar_dir()?, package)
}

/// As [`remove`], but in a jar directory of your choosing.
pub fn remove_in(jar: &Path, package: &str) -> Result<()> {
    let dir = jar.join(package);
    if !dir.exists() {
        return Ok(());
    }
    fs::remove_dir_all(&dir).with_context(|| format!("cannot remove {}", dir.display()))?;
    Ok(())
}

/// Read one cookie out of the jar.
pub fn get(package: &str) -> Result<Option<Cookie>> {
    get_in(&paths::jar_dir()?, package)
}

/// As [`get`], but in a jar directory of your choosing.
pub fn get_in(jar: &Path, package: &str) -> Result<Option<Cookie>> {
    read_dir(&jar.join(package))
}

/// Every cookie in the jar, sorted by name.
pub fn list() -> Result<Vec<Cookie>> {
    list_in(&paths::jar_dir()?)
}

/// As [`list`], but in a jar directory of your choosing.
pub fn list_in(jar: &Path) -> Result<Vec<Cookie>> {
    if !jar.is_dir() {
        return Ok(Vec::new());
    }

    let mut cookies = Vec::new();
    let entries = fs::read_dir(jar).with_context(|| format!("cannot read {}", jar.display()))?;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        // A directory we cannot parse is skipped rather than fatal: one bad
        // record should not stop `cookie jar` from listing the rest.
        if let Ok(Some(cookie)) = read_dir(&entry.path()) {
            cookies.push(cookie);
        }
    }
    cookies.sort_by_key(|c| c.name().to_lowercase());
    Ok(cookies)
}

fn read_dir(dir: &Path) -> Result<Option<Cookie>> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let record_path = dir.join(INSTALLED_FILE);
    if !manifest_path.is_file() || !record_path.is_file() {
        return Ok(None);
    }

    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("cannot read {}", manifest_path.display()))?;
    let manifest = CpkManifest::from_toml(&manifest_text)
        .with_context(|| format!("cannot understand {}", manifest_path.display()))?;

    let record_text = fs::read_to_string(&record_path)
        .with_context(|| format!("cannot read {}", record_path.display()))?;
    let record: InstalledRecord = toml::from_str(&record_text)
        .with_context(|| format!("cannot understand {}", record_path.display()))?;

    Ok(Some(Cookie {
        package: manifest.cookie.package.clone(),
        manifest,
        record,
        dir: dir.to_path_buf(),
    }))
}

/// Find a cookie by package id or by name.
///
/// Users type `cookie crumbs signal`, not the full application id, so the
/// lookup widens step by step: exact package, exact name, case-insensitive
/// name, then a unique substring match.
pub fn resolve(query: &str) -> Result<Cookie> {
    resolve_in(&paths::jar_dir()?, query)
}

/// As [`resolve`], but in a jar directory of your choosing.
pub fn resolve_in(jar: &Path, query: &str) -> Result<Cookie> {
    let cookies = list_in(jar)?;
    if cookies.is_empty() {
        bail!(
            "the jar is empty — nothing is installed yet.\n\
             Bake an app and eat it:\n  \
             cookie bake <app.apk>\n  cookie eat <app.cpk>"
        );
    }

    let needle = query.trim();
    let lower = needle.to_lowercase();

    if let Some(found) = cookies.iter().find(|c| c.package == needle) {
        return Ok(found.clone());
    }
    if let Some(found) = cookies.iter().find(|c| c.name() == needle) {
        return Ok(found.clone());
    }
    if let Some(found) = cookies
        .iter()
        .find(|c| c.name().to_lowercase() == lower || c.package.to_lowercase() == lower)
    {
        return Ok(found.clone());
    }

    let partial: Vec<&Cookie> = cookies
        .iter()
        .filter(|c| {
            c.name().to_lowercase().contains(&lower) || c.package.to_lowercase().contains(&lower)
        })
        .collect();

    match partial.len() {
        1 => match partial.into_iter().next() {
            Some(found) => Ok(found.clone()),
            None => bail!("internal error: lost the matched cookie"),
        },
        0 => bail!(
            "no cookie called `{needle}` in the jar.\n\
             What is in there:\n{}",
            cookies
                .iter()
                .map(|c| format!("  {}  ({})", c.name(), c.package))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        _ => bail!(
            "`{needle}` matches more than one cookie:\n{}\n\
             Use the full name or the package id.",
            partial
                .iter()
                .map(|c| format!("  {}  ({})", c.name(), c.package))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpk::manifest::{BakedMeta, CookieMeta, CPK_FORMAT_VERSION};
    use crate::paths::ScratchDir;
    use std::collections::BTreeMap;

    fn manifest(name: &str, package: &str, abis: &[&str]) -> CpkManifest {
        CpkManifest {
            cpk_version: CPK_FORMAT_VERSION,
            cookie: CookieMeta {
                name: name.to_string(),
                package: package.to_string(),
                version_name: Some("1.0".into()),
                version_code: Some(1),
                abis: abis.iter().map(|a| a.to_string()).collect(),
                icon: Some("icon.png".into()),
                min_sdk: None,
                target_sdk: None,
                launchable_activity: None,
            },
            baked: BakedMeta {
                by: "cookie test".into(),
                at: "2026-11-18T00:00:00Z".into(),
                base_apk: "app/base.apk".into(),
                splits: Vec::new(),
                signature_sha256: Some("abc123".into()),
                files: BTreeMap::new(),
            },
        }
    }

    /// A jar with Signal, Slack and F-Droid in it.
    fn stocked_jar() -> (ScratchDir, PathBuf) {
        let scratch = ScratchDir::new("jar-test").expect("scratch");
        let jar = scratch.path().join("jar");
        for (name, package) in [
            ("Signal", "org.thoughtcrime.securesms"),
            ("Slack", "com.slack"),
            ("F-Droid", "org.fdroid.fdroid"),
        ] {
            put_in(
                &jar,
                &manifest(name, package, &["arm64-v8a"]),
                None,
                None,
                "waydroid",
            )
            .expect("put");
        }
        (scratch, jar)
    }

    #[test]
    fn put_then_get_roundtrips() {
        let scratch = ScratchDir::new("jar-test").expect("scratch");
        let jar = scratch.path().join("jar");
        let m = manifest("Teapot", "com.example.teapot", &["arm64-v8a"]);

        let dir = put_in(&jar, &m, Some(b"\x89PNG fake"), None, "waydroid").expect("put");
        assert!(dir.join("manifest.toml").is_file());
        assert!(dir.join("installed.toml").is_file());
        assert!(dir.join("icon.png").is_file());

        let found = get_in(&jar, "com.example.teapot")
            .expect("get")
            .expect("present");
        assert_eq!(found.name(), "Teapot");
        assert_eq!(found.package, "com.example.teapot");
        assert_eq!(found.record.backend, "waydroid");
        assert_eq!(found.record.signature_sha256.as_deref(), Some("abc123"));
        assert!(found.icon_path().is_some());
    }

    #[test]
    fn an_icon_is_optional() {
        let scratch = ScratchDir::new("jar-test").expect("scratch");
        let jar = scratch.path().join("jar");
        let m = manifest("Plain", "com.example.plain", &[]);
        put_in(&jar, &m, None, None, "waydroid").expect("put");

        let found = get_in(&jar, "com.example.plain")
            .expect("get")
            .expect("present");
        assert_eq!(found.record.icon, None);
        assert_eq!(found.icon_path(), None);
    }

    #[test]
    fn listing_is_sorted_by_name() {
        let (_scratch, jar) = stocked_jar();
        let names: Vec<String> = list_in(&jar)
            .expect("list")
            .iter()
            .map(|c| c.name().to_string())
            .collect();
        assert_eq!(names, vec!["F-Droid", "Signal", "Slack"]);
    }

    #[test]
    fn an_empty_jar_lists_nothing() {
        let scratch = ScratchDir::new("jar-test").expect("scratch");
        assert!(list_in(&scratch.path().join("nope"))
            .expect("list")
            .is_empty());
    }

    #[test]
    fn a_damaged_entry_does_not_hide_the_rest() {
        let (_scratch, jar) = stocked_jar();
        // Corrupt one record; the other two should still list.
        fs::write(jar.join("com.slack/manifest.toml"), "not valid toml {{{").expect("write");
        let names: Vec<String> = list_in(&jar)
            .expect("list")
            .iter()
            .map(|c| c.name().to_string())
            .collect();
        assert_eq!(names, vec!["F-Droid", "Signal"]);
    }

    #[test]
    fn resolve_finds_by_package_name_and_case() {
        let (_scratch, jar) = stocked_jar();
        assert_eq!(
            resolve_in(&jar, "org.thoughtcrime.securesms")
                .expect("by package")
                .name(),
            "Signal"
        );
        assert_eq!(
            resolve_in(&jar, "Signal").expect("by name").name(),
            "Signal"
        );
        assert_eq!(
            resolve_in(&jar, "signal").expect("by case").name(),
            "Signal"
        );
        assert_eq!(
            resolve_in(&jar, "f-droid").expect("by case").name(),
            "F-Droid"
        );
    }

    #[test]
    fn resolve_accepts_a_unique_fragment() {
        let (_scratch, jar) = stocked_jar();
        assert_eq!(
            resolve_in(&jar, "fdroid").expect("fragment").name(),
            "F-Droid"
        );
        assert_eq!(
            resolve_in(&jar, "thoughtcrime").expect("fragment").name(),
            "Signal"
        );
    }

    #[test]
    fn resolve_rejects_an_ambiguous_fragment() {
        let (_scratch, jar) = stocked_jar();
        // "S" matches both Signal and Slack.
        let err = resolve_in(&jar, "s").unwrap_err().to_string();
        assert!(err.contains("matches more than one"), "{err}");
        assert!(err.contains("Signal"), "{err}");
        assert!(err.contains("Slack"), "{err}");
    }

    #[test]
    fn resolve_lists_what_is_there_when_nothing_matches() {
        let (_scratch, jar) = stocked_jar();
        let err = resolve_in(&jar, "telegram").unwrap_err().to_string();
        assert!(err.contains("no cookie called `telegram`"), "{err}");
        assert!(err.contains("Signal"), "{err}");
    }

    #[test]
    fn resolve_on_an_empty_jar_says_how_to_fill_it() {
        let scratch = ScratchDir::new("jar-test").expect("scratch");
        let err = resolve_in(&scratch.path().join("jar"), "anything")
            .unwrap_err()
            .to_string();
        assert!(err.contains("jar is empty"), "{err}");
        assert!(err.contains("cookie bake"), "{err}");
    }

    #[test]
    fn remove_takes_the_cookie_out() {
        let (_scratch, jar) = stocked_jar();
        remove_in(&jar, "com.slack").expect("remove");
        assert!(get_in(&jar, "com.slack").expect("get").is_none());
        assert_eq!(list_in(&jar).expect("list").len(), 2);
    }

    #[test]
    fn removing_something_absent_is_fine() {
        let (_scratch, jar) = stocked_jar();
        remove_in(&jar, "com.not.here").expect("remove is idempotent");
        assert_eq!(list_in(&jar).expect("list").len(), 3);
    }

    #[test]
    fn put_overwrites_an_earlier_record() {
        let scratch = ScratchDir::new("jar-test").expect("scratch");
        let jar = scratch.path().join("jar");
        put_in(
            &jar,
            &manifest("Old", "com.example.app", &[]),
            None,
            None,
            "waydroid",
        )
        .expect("first");
        put_in(
            &jar,
            &manifest("New", "com.example.app", &[]),
            None,
            None,
            "oven",
        )
        .expect("second");

        let found = get_in(&jar, "com.example.app")
            .expect("get")
            .expect("present");
        assert_eq!(found.name(), "New");
        assert_eq!(found.record.backend, "oven");
        assert_eq!(list_in(&jar).expect("list").len(), 1);
    }
}
