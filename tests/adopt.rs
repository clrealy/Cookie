//! Adopting an app Cookie did not install.
//!
//! The case this exists for: a store running inside the container — Aurora
//! Store, say — installs something. Android has it, Cookie has never heard
//! of it, and so it has no launcher and `cookie crumbs` cannot find it.
//!
//! Adoption reads the app's own APK back out, so the resulting jar entry is
//! as complete as one `cookie eat` would have written. These tests drive that
//! against a real signed fixture through a stand-in backend.

use std::path::{Path, PathBuf};

use anyhow::Result;
use cookie::adopt::{self, Detail};
use cookie::backend::{
    AndroidBackend, ArmTranslation, InstallOptions, InstalledApp, LogSpec, OvenState, OvenStatus,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A backend that reports exactly what a test tells it to.
struct Stub {
    installed: Vec<InstalledApp>,
    apks: Vec<PathBuf>,
}

impl AndroidBackend for Stub {
    fn name(&self) -> &'static str {
        "stub"
    }
    fn ensure_available(&self) -> Result<()> {
        Ok(())
    }
    fn status(&self) -> Result<OvenStatus> {
        Ok(OvenStatus {
            state: OvenState::Running,
            container_running: Some(true),
            session_running: Some(true),
            vendor_type: None,
            ip_address: None,
            raw: String::new(),
        })
    }
    fn start(&self) -> Result<()> {
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        Ok(())
    }
    fn install(&self, _apks: &[PathBuf], _options: &InstallOptions) -> Result<()> {
        Ok(())
    }
    fn uninstall(&self, _package: &str) -> Result<()> {
        Ok(())
    }
    fn list_installed(&self) -> Result<Vec<InstalledApp>> {
        Ok(self.installed.clone())
    }
    fn launch(&self, _package: &str) -> Result<()> {
        Ok(())
    }
    fn pid_of(&self, _package: &str) -> Result<Option<u32>> {
        Ok(None)
    }
    fn logs(&self, _spec: &LogSpec) -> Result<()> {
        Ok(())
    }
    fn ensure_multi_window(&self) -> Result<()> {
        Ok(())
    }
    fn arm_translation(&self) -> Result<Option<ArmTranslation>> {
        Ok(None)
    }
    fn window_class(&self, package: &str) -> Option<String> {
        Some(format!("stub.{package}"))
    }
    fn installed_apk_paths(&self, _package: &str) -> Result<Vec<PathBuf>> {
        Ok(self.apks.clone())
    }
}

/// The whole point: an adopted entry carries everything a baked one does,
/// because it is read from the same APK.
#[test]
fn adopting_reads_the_apk_rather_than_guessing() {
    let backend = Stub {
        installed: vec![InstalledApp {
            package: "com.example.teapot".into(),
            name: Some("Teapot".into()),
        }],
        apks: vec![fixture("teapot.apk")],
    };

    let app = backend.list_installed().expect("list")[0].clone();
    let (manifest, detail, icon) = adopt::adopt(&backend, &app).expect("adopt");

    assert_eq!(detail, Detail::FromApk);
    assert_eq!(manifest.cookie.package, "com.example.teapot");
    assert_eq!(manifest.cookie.version_name.as_deref(), Some("2.1.0"));
    assert_eq!(manifest.cookie.version_code, Some(4210));
    assert_eq!(manifest.cookie.abis, vec!["arm64-v8a".to_string()]);
    assert_eq!(manifest.cookie.min_sdk, Some(24));
    assert_eq!(
        manifest.cookie.launchable_activity.as_deref(),
        Some("com.example.teapot.MainActivity")
    );

    // The icon came out of the APK, not from a placeholder.
    let icon = icon.expect("an icon");
    assert!(icon.starts_with(&[0x89, b'P', b'N', b'G']));

    // And the signer, which is what makes a later update check possible.
    let signature = manifest.baked.signature.as_ref().expect("a signature");
    assert!(!signature.is_unknown());
    assert_eq!(signature.signers.len(), 1);
}

/// A split set installs as several APKs; the base is the one to read, and
/// the ABIs are the union across all of them.
#[test]
fn a_split_install_is_read_as_one_app() {
    let backend = Stub {
        installed: vec![InstalledApp {
            package: "com.example.teapot".into(),
            name: None,
        }],
        apks: vec![fixture("teapot.apk"), fixture("teapot-split-arm64.apk")],
    };

    let app = backend.list_installed().expect("list")[0].clone();
    let (manifest, detail, _) = adopt::adopt(&backend, &app).expect("adopt");

    assert_eq!(detail, Detail::FromApk);
    // The base APK's metadata, not a split's.
    assert_eq!(manifest.cookie.version_name.as_deref(), Some("2.1.0"));
    assert_eq!(manifest.cookie.abis, vec!["arm64-v8a".to_string()]);
}

/// The APK may be somewhere Cookie cannot read. That is a weaker entry, not
/// a failure, and it has to say which it is.
#[test]
fn an_unreachable_apk_still_produces_an_entry() {
    let backend = Stub {
        installed: vec![InstalledApp {
            package: "com.aurora.store".into(),
            name: Some("Aurora Store".into()),
        }],
        apks: Vec::new(),
    };

    let app = backend.list_installed().expect("list")[0].clone();
    let (manifest, detail, icon) = adopt::adopt(&backend, &app).expect("adopt");

    assert_eq!(detail, Detail::NameOnly);
    assert_eq!(manifest.cookie.package, "com.aurora.store");
    assert_eq!(manifest.cookie.name, "Aurora Store");
    assert!(manifest.cookie.version_name.is_none());
    assert!(manifest.baked.signature.is_none());
    assert!(icon.is_none());
}

/// An adopted entry has no cookie behind it. The record says so rather than
/// looking like something `cookie eat` wrote, because `cookie eat <name>`
/// cannot reinstall from it.
#[test]
fn an_adopted_entry_does_not_pretend_to_be_baked() {
    let backend = Stub {
        installed: vec![InstalledApp {
            package: "com.example.teapot".into(),
            name: None,
        }],
        apks: vec![fixture("teapot.apk")],
    };
    let app = backend.list_installed().expect("list")[0].clone();
    let (manifest, _, _) = adopt::adopt(&backend, &app).expect("adopt");

    assert!(
        manifest.baked.by.contains("adopted"),
        "{}",
        manifest.baked.by
    );
    assert!(
        manifest.baked.files.is_empty(),
        "there are no files to have hashed"
    );
}

/// Reconciliation drives the whole feature, so check it against what a
/// backend really returns rather than only in a unit test.
#[test]
fn the_jar_and_the_runtime_are_compared_both_ways() {
    let backend = Stub {
        installed: vec![
            InstalledApp {
                package: "com.aurora.store".into(),
                name: Some("Aurora Store".into()),
            },
            InstalledApp {
                package: "com.example.teapot".into(),
                name: Some("Teapot".into()),
            },
        ],
        apks: Vec::new(),
    };

    let scratch = cookie::paths::ScratchDir::new("adopt-it").expect("scratch");
    let jar_dir = scratch.path().join("jar");

    // Only the teapot is in the jar.
    let cpk = {
        let info = cookie::apk::inspect(&fixture("teapot.apk")).expect("inspect");
        let sorted = cookie::cpk::pack::sort_apks(vec![info]).expect("sort");
        let out = scratch.path().join("teapot.cpk");
        cookie::cpk::pack::bake(&sorted, None, &out).expect("bake");
        out
    };
    let manifest = cookie::cpk::unpack::read_manifest(&cpk).expect("manifest");
    cookie::jar::put_in(&jar_dir, &manifest, None, Some(&cpk), "stub").expect("jar");

    let jar = cookie::jar::list_in(&jar_dir).expect("list");
    let drift = adopt::reconcile(&jar, &backend.list_installed().expect("installed"));

    assert_eq!(drift.matched, vec!["com.example.teapot".to_string()]);
    assert_eq!(drift.strays.len(), 1);
    assert_eq!(drift.strays[0].package, "com.aurora.store");
    assert!(drift.stale.is_empty());
    assert!(!drift.is_clean());
}
