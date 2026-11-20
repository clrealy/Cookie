//! Noticing apps Cookie did not install, and taking them in.
//!
//! Cookie's jar records what Cookie put there. The Android runtime it shares
//! does not care who installed what — an app sideloaded by hand, or fetched
//! by a store running inside the container, is installed just the same and
//! Cookie knows nothing about it. It gets no jar entry, no launcher, and
//! `cookie crumbs` cannot find it.
//!
//! So Cookie compares the two and offers to take in the difference. Adoption
//! reads the app's own APK back out of the container, which is the only place
//! its icon, version and signer actually live — the result is a jar entry
//! indistinguishable from one `cookie eat` would have written.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::apk;
use crate::backend::{AndroidBackend, InstalledApp};
use crate::cpk::manifest::{BakedMeta, CookieMeta, CpkManifest, CPK_FORMAT_VERSION};
use crate::jar::{self, Cookie};
use crate::paths::ScratchDir;
use crate::util;

/// How the jar and the runtime compare.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciliation {
    /// In both. The ordinary case.
    pub matched: Vec<String>,
    /// Installed in Android, absent from the jar — Cookie did not put it
    /// there and currently cannot see it.
    pub strays: Vec<InstalledApp>,
    /// In the jar, gone from Android. Removed behind Cookie's back.
    pub stale: Vec<String>,
}

impl Reconciliation {
    pub fn is_clean(&self) -> bool {
        self.strays.is_empty() && self.stale.is_empty()
    }
}

/// Compare what the jar holds against what the runtime actually has.
///
/// Pure, so every shape is testable without a running Android.
pub fn reconcile(jar: &[Cookie], installed: &[InstalledApp]) -> Reconciliation {
    let known: BTreeMap<&str, &Cookie> = jar.iter().map(|c| (c.package.as_str(), c)).collect();
    let present: BTreeMap<&str, &InstalledApp> =
        installed.iter().map(|a| (a.package.as_str(), a)).collect();

    let mut result = Reconciliation::default();

    for (package, app) in &present {
        if known.contains_key(package) {
            result.matched.push((*package).to_string());
        } else {
            result.strays.push((*app).clone());
        }
    }
    for package in known.keys() {
        if !present.contains_key(package) {
            result.stale.push((*package).to_string());
        }
    }

    result.matched.sort();
    result.stale.sort();
    result.strays.sort_by(|a, b| a.package.cmp(&b.package));
    result
}

/// What adoption managed to find out about an app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detail {
    /// The APK was read, so the entry has everything a baked one would.
    FromApk,
    /// The APK could not be reached; the entry has only a name and a
    /// package id.
    NameOnly,
}

/// Take a stray into the jar.
///
/// Reads the app's APK out of the container where it can, so the entry
/// carries the real version, ABIs, icon and signer rather than a stub.
pub fn adopt(
    backend: &dyn AndroidBackend,
    app: &InstalledApp,
) -> Result<(CpkManifest, Detail, Option<Vec<u8>>)> {
    let paths = backend
        .installed_apk_paths(&app.package)
        .unwrap_or_default();

    // The base APK is the one without a `split` name. Reading them all and
    // asking is more reliable than guessing from the filename.
    let mut base = None;
    let mut abis: Vec<String> = Vec::new();
    for path in &paths {
        let Ok(info) = apk::inspect(path) else {
            continue;
        };
        for abi in &info.abis {
            if !abis.contains(abi) {
                abis.push(abi.clone());
            }
        }
        if !info.manifest.is_split() {
            base = Some(info);
        }
    }
    abis.sort();

    let Some(base) = base else {
        return Ok((stub_manifest(app), Detail::NameOnly, None));
    };

    let icon = base
        .icon
        .as_ref()
        .and_then(|candidate| apk::read_icon(&base.path, candidate).ok());

    let signature = apk::signing::read(&base.path)
        .ok()
        .filter(|s| !s.is_unknown());

    let manifest = CpkManifest {
        cpk_version: CPK_FORMAT_VERSION,
        cookie: CookieMeta {
            name: app.name.clone().unwrap_or_else(|| base.display_name()),
            package: base.manifest.package.clone(),
            version_name: base.manifest.version_name.clone(),
            version_code: base.manifest.version_code,
            abis,
            icon: icon.as_ref().map(|_| "icon.png".to_string()),
            min_sdk: base.manifest.min_sdk,
            target_sdk: base.manifest.target_sdk,
            launchable_activity: base.manifest.launchable_activity.clone(),
        },
        baked: BakedMeta {
            // Said plainly, because it matters: this entry was reconstructed
            // from an installed app, not built from a cookie. There is no
            // .cpk behind it to reinstall from.
            by: format!("cookie {} (adopted)", env!("CARGO_PKG_VERSION")),
            at: util::iso8601_utc(util::now_unix()),
            base_apk: "app/base.apk".to_string(),
            splits: Vec::new(),
            signature,
            legacy_signature_sha256: None,
            files: BTreeMap::new(),
        },
    };

    Ok((manifest, Detail::FromApk, icon))
}

/// The least Cookie can record when the APK cannot be read.
fn stub_manifest(app: &InstalledApp) -> CpkManifest {
    CpkManifest {
        cpk_version: CPK_FORMAT_VERSION,
        cookie: CookieMeta {
            name: app.name.clone().unwrap_or_else(|| {
                let tail = app.package.rsplit('.').next().unwrap_or(&app.package);
                let mut chars = tail.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => app.package.clone(),
                }
            }),
            package: app.package.clone(),
            version_name: None,
            version_code: None,
            abis: Vec::new(),
            icon: None,
            min_sdk: None,
            target_sdk: None,
            launchable_activity: None,
        },
        baked: BakedMeta {
            by: format!("cookie {} (adopted)", env!("CARGO_PKG_VERSION")),
            at: util::iso8601_utc(util::now_unix()),
            base_apk: "app/base.apk".to_string(),
            splits: Vec::new(),
            signature: None,
            legacy_signature_sha256: None,
            files: BTreeMap::new(),
        },
    }
}

/// Adopt a stray and write it into the jar.
pub fn adopt_into_jar(
    backend: &dyn AndroidBackend,
    app: &InstalledApp,
) -> Result<(Cookie, Detail)> {
    let (manifest, detail, icon) = adopt(backend, app)?;

    // Nothing is copied into the jar but the icon; the APK stays where
    // Android put it, as it does for a baked cookie.
    let _scratch: Option<ScratchDir> = None;
    jar::put(&manifest, icon.as_deref(), None, backend.name())
        .with_context(|| format!("cannot record {} in the jar", manifest.cookie.package))?;

    let cookie = jar::get(&manifest.cookie.package)?
        .with_context(|| format!("{} vanished from the jar", manifest.cookie.package))?;
    Ok((cookie, detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpk::manifest::CookieMeta;
    use crate::jar::InstalledRecord;
    use std::path::PathBuf;

    fn installed(package: &str, name: Option<&str>) -> InstalledApp {
        InstalledApp {
            package: package.to_string(),
            name: name.map(str::to_string),
        }
    }

    fn in_jar(package: &str) -> Cookie {
        let manifest = CpkManifest {
            cpk_version: CPK_FORMAT_VERSION,
            cookie: CookieMeta {
                name: package.to_string(),
                package: package.to_string(),
                version_name: None,
                version_code: None,
                abis: Vec::new(),
                icon: None,
                min_sdk: None,
                target_sdk: None,
                launchable_activity: None,
            },
            baked: BakedMeta {
                by: "test".into(),
                at: "2026-11-20T00:00:00Z".into(),
                base_apk: "app/base.apk".into(),
                splits: Vec::new(),
                signature: None,
                legacy_signature_sha256: None,
                files: BTreeMap::new(),
            },
        };
        Cookie {
            package: package.to_string(),
            record: InstalledRecord {
                installed_at: 0,
                backend: "waydroid".into(),
                source_cpk: None,
                icon: None,
                signature: None,
            },
            manifest,
            dir: PathBuf::from("/nowhere"),
        }
    }

    #[test]
    fn an_app_in_both_is_matched() {
        let result = reconcile(
            &[in_jar("com.example.teapot")],
            &[installed("com.example.teapot", Some("Teapot"))],
        );
        assert_eq!(result.matched, vec!["com.example.teapot".to_string()]);
        assert!(result.strays.is_empty());
        assert!(result.stale.is_empty());
        assert!(result.is_clean());
    }

    /// The case this exists for: something else installed an app.
    #[test]
    fn an_app_android_has_and_the_jar_does_not_is_a_stray() {
        let result = reconcile(&[], &[installed("com.aurora.store", Some("Aurora Store"))]);
        assert_eq!(result.strays.len(), 1);
        assert_eq!(result.strays[0].package, "com.aurora.store");
        assert!(!result.is_clean());
    }

    #[test]
    fn an_app_the_jar_has_and_android_does_not_is_stale() {
        let result = reconcile(&[in_jar("com.example.gone")], &[]);
        assert_eq!(result.stale, vec!["com.example.gone".to_string()]);
        assert!(!result.is_clean());
    }

    #[test]
    fn both_kinds_of_drift_are_reported_together() {
        let result = reconcile(
            &[in_jar("com.example.kept"), in_jar("com.example.gone")],
            &[
                installed("com.example.kept", None),
                installed("com.aurora.store", None),
            ],
        );
        assert_eq!(result.matched, vec!["com.example.kept".to_string()]);
        assert_eq!(result.stale, vec!["com.example.gone".to_string()]);
        assert_eq!(result.strays.len(), 1);
    }

    #[test]
    fn nothing_anywhere_is_clean() {
        assert!(reconcile(&[], &[]).is_clean());
    }

    #[test]
    fn results_are_sorted_so_the_output_is_stable() {
        let result = reconcile(
            &[in_jar("b.gone"), in_jar("a.gone")],
            &[installed("z.stray", None), installed("a.stray", None)],
        );
        assert_eq!(
            result.stale,
            vec!["a.gone".to_string(), "b.gone".to_string()]
        );
        assert_eq!(result.strays[0].package, "a.stray");
        assert_eq!(result.strays[1].package, "z.stray");
    }

    #[test]
    fn a_stub_still_produces_a_usable_name() {
        let manifest = stub_manifest(&installed("com.aurora.store", None));
        assert_eq!(manifest.cookie.package, "com.aurora.store");
        assert_eq!(manifest.cookie.name, "Store");
        assert!(manifest.baked.by.contains("adopted"));

        let named = stub_manifest(&installed("com.aurora.store", Some("Aurora Store")));
        assert_eq!(named.cookie.name, "Aurora Store");
    }

    /// An adopted entry has no cookie behind it, and the record says so
    /// rather than looking like something `cookie eat` wrote.
    #[test]
    fn an_adopted_entry_admits_what_it_is() {
        let manifest = stub_manifest(&installed("com.example.x", None));
        assert!(
            manifest.baked.by.contains("adopted"),
            "{}",
            manifest.baked.by
        );
        assert!(manifest.baked.files.is_empty());
    }
}
