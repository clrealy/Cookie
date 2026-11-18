//! `manifest.toml`, the metadata file at the root of every `.cpk`.
//!
//! The full format is documented in `docs/cpk-spec.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The only `.cpk` format version that exists so far.
pub const CPK_FORMAT_VERSION: u32 = 1;

/// Name of the metadata entry inside the archive.
pub const MANIFEST_ENTRY: &str = "manifest.toml";

/// Directory inside the archive holding the APKs.
pub const APP_DIR: &str = "app";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CpkManifest {
    /// Format version of this `.cpk`. Readers refuse anything newer than
    /// [`CPK_FORMAT_VERSION`].
    pub cpk_version: u32,
    pub cookie: CookieMeta,
    pub baked: BakedMeta,
}

/// What the app is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CookieMeta {
    /// Human-facing name, shown in `cookie jar` and the app menu.
    pub name: String,
    /// Android application id, e.g. `org.thoughtcrime.securesms`.
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version_code: Option<u64>,
    /// ABIs the bundled APKs carry native code for. Empty means pure Java or
    /// Kotlin, which runs on any ABI.
    #[serde(default)]
    pub abis: Vec<String>,
    /// Icon path inside the archive, if one was found.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub min_sdk: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub target_sdk: Option<u32>,
    /// Component to launch, when the manifest named one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub launchable_activity: Option<String>,
}

/// How this `.cpk` came to be.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BakedMeta {
    /// Tool and version that produced the archive.
    pub by: String,
    /// When, as ISO 8601 UTC.
    pub at: String,
    /// Archive path of the base APK.
    pub base_apk: String,
    /// Archive paths of any split APKs, sorted.
    #[serde(default)]
    pub splits: Vec<String>,
    /// SHA-256 over the APK's v1 signing certificates, when it has them.
    /// Used to tell an update from a different app with the same package id.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature_sha256: Option<String>,
    /// SHA-256 of every APK in the archive, keyed by archive path.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

/// Why a `.cpk` could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest.toml is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error(
        "this cookie uses .cpk format version {found}, but this Cookie only understands up to {supported}.\n\
         Update Cookie, or re-bake the app from its APK."
    )]
    TooNew { found: u32, supported: u32 },
    #[error("manifest.toml declares cpk_version = 0, which is not a real version")]
    ZeroVersion,
    #[error("manifest.toml has an empty package id")]
    EmptyPackage,
}

impl CpkManifest {
    /// Parse and validate a `manifest.toml`.
    pub fn from_toml(text: &str) -> Result<Self, ManifestError> {
        let manifest: CpkManifest = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.cpk_version == 0 {
            return Err(ManifestError::ZeroVersion);
        }
        if self.cpk_version > CPK_FORMAT_VERSION {
            return Err(ManifestError::TooNew {
                found: self.cpk_version,
                supported: CPK_FORMAT_VERSION,
            });
        }
        if self.cookie.package.trim().is_empty() {
            return Err(ManifestError::EmptyPackage);
        }
        Ok(())
    }

    /// Every APK in the archive, base first.
    pub fn apk_entries(&self) -> Vec<&str> {
        let mut out = vec![self.baked.base_apk.as_str()];
        out.extend(self.baked.splits.iter().map(|s| s.as_str()));
        out
    }

    /// A one-line version summary.
    pub fn version_display(&self) -> String {
        match (&self.cookie.version_name, self.cookie.version_code) {
            (Some(name), Some(code)) => format!("{name} ({code})"),
            (Some(name), None) => name.clone(),
            (None, Some(code)) => code.to_string(),
            (None, None) => "unknown".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CpkManifest {
        CpkManifest {
            cpk_version: CPK_FORMAT_VERSION,
            cookie: CookieMeta {
                name: "Teapot".into(),
                package: "com.example.teapot".into(),
                version_name: Some("2.1.0".into()),
                version_code: Some(4210),
                abis: vec!["arm64-v8a".into()],
                icon: Some("icon.png".into()),
                min_sdk: Some(24),
                target_sdk: Some(34),
                launchable_activity: Some("com.example.teapot.MainActivity".into()),
            },
            baked: BakedMeta {
                by: "cookie 0.1.0".into(),
                at: "2026-11-18T22:36:00Z".into(),
                base_apk: "app/base.apk".into(),
                splits: vec!["app/split_config.arm64_v8a.apk".into()],
                signature_sha256: Some("deadbeef".into()),
                files: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn roundtrips_through_toml() {
        let original = sample();
        let text = original.to_toml().expect("serialise");
        let parsed = CpkManifest::from_toml(&text).expect("parse");
        assert_eq!(original, parsed);
    }

    #[test]
    fn rejects_a_future_format_version() {
        let mut m = sample();
        m.cpk_version = CPK_FORMAT_VERSION + 1;
        let text = toml::to_string_pretty(&m).expect("serialise");
        let err = CpkManifest::from_toml(&text).unwrap_err();
        assert!(matches!(err, ManifestError::TooNew { .. }));
        // The message has to tell the user what to do about it.
        assert!(err.to_string().contains("Update Cookie"));
    }

    #[test]
    fn rejects_version_zero() {
        let mut m = sample();
        m.cpk_version = 0;
        let text = toml::to_string_pretty(&m).expect("serialise");
        assert!(matches!(
            CpkManifest::from_toml(&text).unwrap_err(),
            ManifestError::ZeroVersion
        ));
    }

    #[test]
    fn rejects_an_empty_package() {
        let mut m = sample();
        m.cookie.package = "   ".into();
        let text = toml::to_string_pretty(&m).expect("serialise");
        assert!(matches!(
            CpkManifest::from_toml(&text).unwrap_err(),
            ManifestError::EmptyPackage
        ));
    }

    #[test]
    fn apk_entries_put_the_base_first() {
        let m = sample();
        assert_eq!(
            m.apk_entries(),
            vec!["app/base.apk", "app/split_config.arm64_v8a.apk"]
        );
    }

    #[test]
    fn optional_fields_are_omitted_not_nulled() {
        let mut m = sample();
        m.cookie.icon = None;
        m.cookie.version_name = None;
        let text = m.to_toml().expect("serialise");
        assert!(!text.contains("icon"), "{text}");
        assert!(!text.contains("version_name"), "{text}");
        // And it still round-trips.
        assert_eq!(CpkManifest::from_toml(&text).expect("parse"), m);
    }

    #[test]
    fn garbage_toml_is_an_error() {
        assert!(CpkManifest::from_toml("this is not toml {{{").is_err());
        assert!(CpkManifest::from_toml("").is_err());
    }
}
