//! Update decisions, end to end over genuinely signed APKs.
//!
//! The fixtures carry real X.509 certificates, a real PKCS#7 blob for v1 jar
//! signing, and a real APK Signing Block for v2 and v3. `signatures.json`
//! holds the certificate digests as computed by Python's `hashlib`, so these
//! tests check Cookie's reader against an independent implementation rather
//! than against itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cookie::apk::{self, signing::Scheme};
use cookie::cpk::{pack, unpack};
use cookie::jar;
use cookie::paths::ScratchDir;
use cookie::update::{self, Decision, UnknownReason};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The digests `make_fixtures.py` recorded, parsed without a JSON dependency:
/// the file is two flat string fields.
fn expected_digests() -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(fixture("signatures.json")).expect("signatures.json");
    let mut found = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim().trim_end_matches(',');
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().trim_matches('"');
            let value = value.trim().trim_matches('"');
            if !key.is_empty() && value.len() == 64 {
                found.insert(key.to_string(), value.to_string());
            }
        }
    }
    assert_eq!(found.len(), 2, "expected two digests in signatures.json");
    found
}

fn bake(scratch: &Path, apk_name: &str, out_name: &str) -> PathBuf {
    let info = apk::inspect(&fixture(apk_name)).expect("inspect");
    let sorted = pack::sort_apks(vec![info]).expect("sort");
    let out = scratch.join(out_name);
    pack::bake(&sorted, None, &out).expect("bake");
    out
}

/// Bake a cookie and record it in a scratch jar, as `cookie eat` would.
fn install(scratch: &Path, jar_dir: &Path, apk_name: &str, out_name: &str) -> jar::Cookie {
    let cpk = bake(scratch, apk_name, out_name);
    let manifest = unpack::read_manifest(&cpk).expect("manifest");
    jar::put_in(jar_dir, &manifest, None, Some(&cpk), "waydroid").expect("jar");
    jar::get_in(jar_dir, &manifest.cookie.package)
        .expect("read back")
        .expect("present")
}

// ---------------------------------------------------------------------------
// Reading signatures
// ---------------------------------------------------------------------------

#[test]
fn cookie_reports_the_same_certificate_digest_as_python_does() {
    let expected = expected_digests();
    let teapot = expected.get("teapot").expect("teapot digest");
    let other = expected.get("other").expect("other digest");

    let signature = apk::signing::read(&fixture("teapot.apk")).expect("signature");
    assert_eq!(signature.signers, vec![teapot.clone()]);

    let signature = apk::signing::read(&fixture("teapot-resigned.apk")).expect("signature");
    assert_eq!(signature.signers, vec![other.clone()]);

    assert_ne!(teapot, other, "the fixtures must use different keys");
}

#[test]
fn every_scheme_yields_the_same_certificate() {
    let expected = expected_digests();
    let teapot = expected.get("teapot").expect("teapot digest");

    // Signed under v1, v2 and v3 at once.
    let all = apk::signing::read(&fixture("teapot.apk")).expect("signature");
    assert_eq!(all.schemes, vec![Scheme::V1, Scheme::V2, Scheme::V3]);
    assert_eq!(all.signers, vec![teapot.clone()]);

    // The APK Signing Block on its own.
    let v2_only = apk::signing::read(&fixture("teapot-v2only.apk")).expect("signature");
    assert_eq!(v2_only.schemes, vec![Scheme::V2]);
    assert_eq!(v2_only.signers, vec![teapot.clone()]);

    // The PKCS#7 blob on its own.
    let v1_only = apk::signing::read(&fixture("teapot-v1only.apk")).expect("signature");
    assert_eq!(v1_only.schemes, vec![Scheme::V1]);
    assert_eq!(v1_only.signers, vec![teapot.clone()]);

    // Which means an app that adds or drops a scheme between releases is
    // still recognised as the same app.
    assert_eq!(all.same_signer_as(&v2_only), Some(true));
    assert_eq!(v1_only.same_signer_as(&v2_only), Some(true));
}

#[test]
fn an_unsigned_apk_reports_nothing_rather_than_failing() {
    let signature = apk::signing::read(&fixture("plain.apk")).expect("signature");
    assert!(signature.is_unknown());
    assert_eq!(signature.scheme_list(), "none found");
}

#[test]
fn a_baked_cookie_carries_the_signature() {
    let scratch = ScratchDir::new("update").expect("scratch");
    let cpk = bake(scratch.path(), "teapot.apk", "teapot.cpk");
    let manifest = unpack::read_manifest(&cpk).expect("manifest");

    let signature = manifest.baked.signature.as_ref().expect("a signature");
    assert_eq!(
        signature.signers,
        vec![expected_digests()["teapot"].clone()]
    );
    assert_eq!(manifest.cpk_version, 2);
}

#[test]
fn a_cookie_from_an_unsigned_apk_has_no_signature() {
    let scratch = ScratchDir::new("update").expect("scratch");
    let cpk = bake(scratch.path(), "plain.apk", "plain.cpk");
    let manifest = unpack::read_manifest(&cpk).expect("manifest");
    assert!(manifest.baked.signature.is_none());
}

// ---------------------------------------------------------------------------
// Decisions
// ---------------------------------------------------------------------------

#[test]
fn a_newer_build_from_the_same_key_updates_and_keeps_data() {
    let scratch = ScratchDir::new("update").expect("scratch");
    let jar_dir = scratch.path().join("jar");

    let installed = install(scratch.path(), &jar_dir, "teapot.apk", "v1.cpk");
    let incoming =
        unpack::read_manifest(&bake(scratch.path(), "teapot-v2.apk", "v2.cpk")).expect("manifest");

    let decision = update::decide(Some(&installed), &incoming);
    assert_eq!(
        decision,
        Decision::Update {
            from: "2.1.0 (4210)".into(),
            to: "2.2.0 (4300)".into()
        }
    );
    assert!(decision.is_safe());
    assert!(decision.keeps_data());
}

#[test]
fn the_same_build_again_is_a_reinstall() {
    let scratch = ScratchDir::new("update").expect("scratch");
    let jar_dir = scratch.path().join("jar");

    let installed = install(scratch.path(), &jar_dir, "teapot.apk", "v1.cpk");
    let incoming =
        unpack::read_manifest(&bake(scratch.path(), "teapot.apk", "again.cpk")).expect("manifest");

    assert!(matches!(
        update::decide(Some(&installed), &incoming),
        Decision::Reinstall { .. }
    ));
}

#[test]
fn an_older_build_is_a_downgrade_and_needs_asking_for() {
    let scratch = ScratchDir::new("update").expect("scratch");
    let jar_dir = scratch.path().join("jar");

    let installed = install(scratch.path(), &jar_dir, "teapot.apk", "v1.cpk");
    let incoming = unpack::read_manifest(&bake(scratch.path(), "teapot-old.apk", "old.cpk"))
        .expect("manifest");

    let decision = update::decide(Some(&installed), &incoming);
    assert_eq!(
        decision,
        Decision::Downgrade {
            from: "2.1.0 (4210)".into(),
            to: "2.0.0 (4100)".into()
        }
    );
    assert!(!decision.is_safe());
}

/// The one that matters: same package id, same app name, higher version, but
/// built with someone else's key. It must never be treated as an update.
#[test]
fn a_different_key_is_refused_however_convincing_it_looks() {
    let scratch = ScratchDir::new("update").expect("scratch");
    let jar_dir = scratch.path().join("jar");

    let installed = install(scratch.path(), &jar_dir, "teapot.apk", "v1.cpk");
    let incoming =
        unpack::read_manifest(&bake(scratch.path(), "teapot-resigned.apk", "resigned.cpk"))
            .expect("manifest");

    // Everything about it says "update" except the key.
    assert_eq!(incoming.cookie.package, installed.manifest.cookie.package);
    assert_eq!(incoming.cookie.name, installed.manifest.cookie.name);
    assert!(incoming.cookie.version_code > installed.manifest.cookie.version_code);

    let decision = update::decide(Some(&installed), &incoming);
    assert!(
        matches!(decision, Decision::SignerMismatch { .. }),
        "{decision:?}"
    );
    assert!(!decision.is_safe());
    assert!(
        !decision.keeps_data(),
        "a different app must never inherit the installed one's data"
    );
}

#[test]
fn the_same_key_under_a_different_scheme_still_updates() {
    let scratch = ScratchDir::new("update").expect("scratch");
    let jar_dir = scratch.path().join("jar");

    // Installed with v1+v2+v3; the update ships only v2.
    let installed = install(scratch.path(), &jar_dir, "teapot.apk", "v1.cpk");
    let incoming = unpack::read_manifest(&bake(scratch.path(), "teapot-v2only.apk", "v2only.cpk"))
        .expect("manifest");

    assert!(matches!(
        update::decide(Some(&installed), &incoming),
        Decision::Update { .. }
    ));
}

#[test]
fn an_unsigned_incoming_cookie_is_unknown_not_a_mismatch() {
    let scratch = ScratchDir::new("update").expect("scratch");
    let jar_dir = scratch.path().join("jar");

    let installed = install(scratch.path(), &jar_dir, "teapot.apk", "v1.cpk");
    let mut incoming =
        unpack::read_manifest(&bake(scratch.path(), "teapot-v2.apk", "v2.cpk")).expect("manifest");
    incoming.baked.signature = None;

    assert_eq!(
        update::decide(Some(&installed), &incoming),
        Decision::SignerUnknown {
            reason: UnknownReason::IncomingUnknown
        }
    );
}

#[test]
fn nothing_installed_is_always_a_fresh_install() {
    let scratch = ScratchDir::new("update").expect("scratch");
    let incoming = unpack::read_manifest(&bake(scratch.path(), "teapot-resigned.apk", "r.cpk"))
        .expect("manifest");
    assert_eq!(update::decide(None, &incoming), Decision::Fresh);
}

// ---------------------------------------------------------------------------
// Format compatibility
// ---------------------------------------------------------------------------

/// A version 1 cookie must still open, but its old `signature_sha256` hashed
/// the certificate *files*, whose bytes vary between builds with the same
/// key. It is therefore not a signer identity, and must be read as unknown
/// rather than compared.
#[test]
fn a_version_one_cookie_has_no_usable_signer() {
    let toml = r#"
cpk_version = 1

[cookie]
name = "Teapot"
package = "com.example.teapot"
version_name = "2.1.0"
version_code = 4210

[baked]
by = "cookie 0.1.0"
at = "2026-11-18T00:00:00Z"
base_apk = "app/base.apk"
signature_sha256 = "ea185bf3dc83d0e9a71af5914bc81f22712a9b1900af579cd6be44ec3193a192"
"#;

    let manifest = cookie::cpk::manifest::CpkManifest::from_toml(toml).expect("v1 parses");
    assert_eq!(manifest.cpk_version, 1);
    assert!(
        manifest.baked.signature.is_none(),
        "the old field must not be mistaken for a signer identity"
    );
    assert!(manifest.baked.legacy_signature_sha256.is_some());

    // And it is never written back out.
    let rendered = manifest.to_toml().expect("serialise");
    assert!(!rendered.contains("signature_sha256"), "{rendered}");
}

#[test]
fn a_cookie_from_the_future_is_refused_with_advice() {
    let toml = r#"
cpk_version = 99

[cookie]
name = "Teapot"
package = "com.example.teapot"

[baked]
by = "cookie 99.0"
at = "2026-11-18T00:00:00Z"
base_apk = "app/base.apk"
"#;
    let err = cookie::cpk::manifest::CpkManifest::from_toml(toml)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Update Cookie"), "{err}");
}

// ---------------------------------------------------------------------------
// Android's own failures
// ---------------------------------------------------------------------------

#[test]
fn install_failures_are_explained() {
    use cookie::backend::explain_install_failure;

    let incompatible =
        explain_install_failure("Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE]").expect("known");
    assert!(incompatible.contains("different key"), "{incompatible}");
    assert!(incompatible.contains("cookie toss"), "{incompatible}");

    let downgrade =
        explain_install_failure("Failure [INSTALL_FAILED_VERSION_DOWNGRADE]").expect("known");
    assert!(downgrade.contains("--allow-downgrade"), "{downgrade}");

    let abis = explain_install_failure("Failure [INSTALL_FAILED_NO_MATCHING_ABIS]").expect("known");
    assert!(abis.contains("cookie burnt"), "{abis}");

    assert_eq!(explain_install_failure("Success"), None);
    assert_eq!(explain_install_failure(""), None);
}
