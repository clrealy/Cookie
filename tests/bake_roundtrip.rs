//! End-to-end tests over real APK files.
//!
//! The fixtures in `tests/fixtures/` are structurally genuine APKs — real
//! zip layout, real Android binary XML — produced by `make_fixtures.py`,
//! which implements the AXML format independently of Cookie's own reader.
//! That means these tests check the parser against a second implementation
//! rather than against itself.

use std::path::{Path, PathBuf};

use cookie::apk::{self, bundle};
use cookie::cpk::{pack, unpack};
use cookie::paths::ScratchDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn bake_to(scratch: &Path, inputs: &[PathBuf], name: &str) -> PathBuf {
    let resolved = bundle::resolve(inputs).expect("inputs resolve");
    let infos: Vec<_> = resolved
        .apks
        .iter()
        .map(|p| apk::inspect(p).expect("apk inspects"))
        .collect();
    let sorted = pack::sort_apks(infos).expect("apks sort");
    let out = scratch.join(name);
    pack::bake(&sorted, None, &out).expect("bake");
    out
}

#[test]
fn reads_a_real_apk() {
    let info = apk::inspect(&fixture("teapot.apk")).expect("inspect");
    assert_eq!(info.manifest.package, "com.example.teapot");
    assert_eq!(info.manifest.version_name.as_deref(), Some("2.1.0"));
    assert_eq!(info.manifest.version_code, Some(4210));
    assert_eq!(info.manifest.label.as_deref(), Some("Teapot"));
    assert_eq!(info.manifest.min_sdk, Some(24));
    assert_eq!(info.manifest.target_sdk, Some(34));
    assert_eq!(info.abis, vec!["arm64-v8a".to_string()]);
    assert_eq!(info.display_name(), "Teapot");
    assert_eq!(
        info.manifest.launchable_activity.as_deref(),
        Some("com.example.teapot.MainActivity"),
        "a relative android:name should be expanded against the package"
    );
}

#[test]
fn picks_the_highest_density_launcher_icon() {
    let info = apk::inspect(&fixture("teapot.apk")).expect("inspect");
    let icon = info.icon.as_ref().expect("an icon was found");
    assert_eq!(icon.entry, "res/mipmap-xxxhdpi/ic_launcher.png");

    let bytes = apk::read_icon(&info.path, icon).expect("icon reads");
    assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn an_apk_with_no_icon_has_no_candidate() {
    let info = apk::inspect(&fixture("plain.apk")).expect("inspect");
    assert!(info.icon.is_none());
    assert!(info.abis.is_empty(), "no lib/ means no ABI requirement");
    assert_eq!(info.display_name(), "Plain");
}

#[test]
fn reads_a_split_apk() {
    let info = apk::inspect(&fixture("teapot-split-arm64.apk")).expect("inspect");
    assert!(info.manifest.is_split());
    assert_eq!(info.manifest.split.as_deref(), Some("config.arm64_v8a"));
    assert_eq!(info.manifest.package, "com.example.teapot");
}

#[test]
fn a_signature_reads_the_same_way_twice() {
    let first = apk::signing::read(&fixture("teapot.apk")).expect("signature");
    let second = apk::signing::read(&fixture("teapot.apk")).expect("signature");
    assert_eq!(first, second);
    assert!(!first.is_unknown());

    // An unsigned APK reports nothing rather than failing, so callers can
    // warn instead of refusing.
    assert!(apk::signing::read(&fixture("plain.apk"))
        .expect("signature")
        .is_unknown());
}

#[test]
fn bake_then_read_back() {
    let scratch = ScratchDir::new("it-bake").expect("scratch");
    let cpk = bake_to(scratch.path(), &[fixture("teapot.apk")], "teapot.cpk");
    assert!(cpk.is_file());

    let manifest = unpack::read_manifest(&cpk).expect("manifest reads");
    assert_eq!(manifest.cpk_version, 2);
    assert_eq!(manifest.cookie.name, "Teapot");
    assert_eq!(manifest.cookie.package, "com.example.teapot");
    assert_eq!(manifest.version_display(), "2.1.0 (4210)");
    assert_eq!(manifest.cookie.abis, vec!["arm64-v8a".to_string()]);
    assert_eq!(manifest.baked.base_apk, "app/base.apk");
    assert!(manifest.baked.splits.is_empty());
    assert!(manifest.baked.by.starts_with("cookie "));
    assert!(manifest.baked.at.ends_with('Z'));

    let icon = unpack::read_icon(&cpk, &manifest).expect("icon reads");
    assert!(icon
        .expect("an icon")
        .starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn baked_apks_come_back_byte_for_byte() {
    let scratch = ScratchDir::new("it-extract").expect("scratch");
    let cpk = bake_to(scratch.path(), &[fixture("teapot.apk")], "teapot.cpk");
    let manifest = unpack::read_manifest(&cpk).expect("manifest");

    let dest = scratch.path().join("out");
    std::fs::create_dir_all(&dest).expect("mkdir");
    let extracted = unpack::extract_apks(&cpk, &manifest, &dest).expect("extract");

    assert_eq!(extracted.len(), 1);
    let original = std::fs::read(fixture("teapot.apk")).expect("read original");
    let roundtripped = std::fs::read(&extracted[0]).expect("read extracted");
    assert_eq!(
        original, roundtripped,
        "the APK must survive the trip intact"
    );

    // And the recorded digest has to match what came out.
    let digest = apk::file_digest(&extracted[0]).expect("digest");
    assert_eq!(manifest.baked.files.get("app/base.apk"), Some(&digest));
}

#[test]
fn bakes_a_base_with_its_split() {
    let scratch = ScratchDir::new("it-split").expect("scratch");
    let cpk = bake_to(
        scratch.path(),
        &[fixture("teapot.apk"), fixture("teapot-split-arm64.apk")],
        "split.cpk",
    );

    let manifest = unpack::read_manifest(&cpk).expect("manifest");
    assert_eq!(manifest.baked.base_apk, "app/base.apk");
    assert_eq!(manifest.baked.splits.len(), 1);
    assert_eq!(manifest.baked.files.len(), 2);
    assert_eq!(manifest.apk_entries().len(), 2);

    let dest = scratch.path().join("out");
    std::fs::create_dir_all(&dest).expect("mkdir");
    let extracted = unpack::extract_apks(&cpk, &manifest, &dest).expect("extract");
    assert_eq!(extracted.len(), 2);
    assert!(
        extracted[0].to_string_lossy().ends_with("base.apk"),
        "the base APK must come out first so `pm` installs it first"
    );
}

#[test]
fn bakes_an_xapk_bundle() {
    let scratch = ScratchDir::new("it-bundle").expect("scratch");
    let cpk = bake_to(scratch.path(), &[fixture("teapot.xapk")], "bundle.cpk");

    let manifest = unpack::read_manifest(&cpk).expect("manifest");
    assert_eq!(manifest.cookie.package, "com.example.teapot");
    assert_eq!(manifest.baked.splits.len(), 1);
    // The bundle's own manifest.json and icon.png must not come along.
    assert_eq!(manifest.apk_entries().len(), 2);
}

#[test]
fn an_apk_with_no_icon_gets_cookies_own() {
    let scratch = ScratchDir::new("it-fallback").expect("scratch");
    let cpk = bake_to(scratch.path(), &[fixture("plain.apk")], "plain.cpk");

    let manifest = unpack::read_manifest(&cpk).expect("manifest");
    assert_eq!(manifest.cookie.icon.as_deref(), Some("icon.png"));
    let icon = unpack::read_icon(&cpk, &manifest)
        .expect("icon reads")
        .expect("there is always an icon");
    assert!(icon.starts_with(&[0x89, b'P', b'N', b'G']));
    assert!(icon.len() > 1000, "the fallback is Cookie's own icon");
}

#[test]
fn refuses_a_split_from_another_app() {
    let resolved = bundle::resolve(&[fixture("teapot.apk"), fixture("kettle-split.apk")])
        .expect("inputs resolve");
    let infos: Vec<_> = resolved
        .apks
        .iter()
        .map(|p| apk::inspect(p).expect("inspect"))
        .collect();

    let err = pack::sort_apks(infos).unwrap_err().to_string();
    assert!(err.contains("different apps"), "{err}");
    assert!(err.contains("com.example.kettle"), "{err}");
}

#[test]
fn refuses_splits_with_no_base() {
    let info = apk::inspect(&fixture("teapot-split-arm64.apk")).expect("inspect");
    let err = pack::sort_apks(vec![info]).unwrap_err().to_string();
    assert!(err.contains("none of these APKs is a base APK"), "{err}");
}

#[test]
fn a_bundle_cannot_be_mixed_with_loose_apks() {
    let err = bundle::resolve(&[fixture("teapot.xapk"), fixture("plain.apk")])
        .unwrap_err()
        .to_string();
    assert!(err.contains("baked on its own"), "{err}");
}

#[test]
fn a_non_apk_is_rejected_by_name() {
    let err = bundle::resolve(&[fixture("make_fixtures.py")])
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not look like an APK"), "{err}");
}

#[test]
fn a_cpk_is_not_an_apk_and_vice_versa() {
    let scratch = ScratchDir::new("it-mixup").expect("scratch");
    let cpk = bake_to(scratch.path(), &[fixture("teapot.apk")], "teapot.cpk");

    // Handing an APK to the cookie reader should say so plainly.
    let err = unpack::read_manifest(&fixture("teapot.apk"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a cookie"), "{err}");
    assert!(err.contains("cookie bake"), "{err}");

    // And a cookie is not an APK.
    let err = apk::inspect(&cpk).unwrap_err().to_string();
    assert!(err.contains("no AndroidManifest.xml"), "{err}");
}

#[test]
fn baking_twice_gives_the_same_app_metadata() {
    let scratch = ScratchDir::new("it-twice").expect("scratch");
    let first = bake_to(scratch.path(), &[fixture("teapot.apk")], "a.cpk");
    let second = bake_to(scratch.path(), &[fixture("teapot.apk")], "b.cpk");

    let a = unpack::read_manifest(&first).expect("a");
    let b = unpack::read_manifest(&second).expect("b");
    assert_eq!(a.cookie, b.cookie);
    assert_eq!(a.baked.files, b.baked.files);
    assert_eq!(a.baked.signature, b.baked.signature);
}

#[test]
fn an_interrupted_bake_leaves_nothing_behind() {
    let scratch = ScratchDir::new("it-partial").expect("scratch");
    // Baking into a directory that does not exist fails; no .partial file
    // should survive next to the intended output.
    let out = scratch.path().join("sub/dir/teapot.cpk");
    let info = apk::inspect(&fixture("teapot.apk")).expect("inspect");
    let sorted = pack::sort_apks(vec![info]).expect("sort");
    pack::bake(&sorted, None, &out).expect("bake creates parent directories");
    assert!(out.is_file());
    assert!(!scratch.path().join("sub/dir/teapot.cpk.partial").exists());
}
