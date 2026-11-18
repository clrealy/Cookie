//! Turns AXML events from an `AndroidManifest.xml` into the handful of facts
//! Cookie actually needs.

use super::axml::{self, Event, Value};

/// What we learn from one APK's manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestInfo {
    /// Application id, e.g. `com.example.teapot`.
    pub package: String,
    /// Present only on split APKs; identifies which split this is.
    pub split: Option<String>,
    pub version_code: Option<u64>,
    pub version_name: Option<String>,
    /// `android:label` on `<application>`, but only when it was a literal
    /// string. Labels that point at a string resource need `resources.arsc`,
    /// which we do not parse, so they come back as `None`.
    pub label: Option<String>,
    /// Resource id behind `android:icon`, when there was one.
    pub icon_resource: Option<u32>,
    pub min_sdk: Option<u32>,
    pub target_sdk: Option<u32>,
    /// Fully-qualified component of the first MAIN/LAUNCHER activity.
    pub launchable_activity: Option<String>,
    /// ABI-specific native code requirement declared via `<uses-native-library>`
    /// is not modelled; ABIs come from the zip layout instead.
    pub extract_native_libs: Option<bool>,
}

impl ManifestInfo {
    /// True when this APK is a split rather than a base APK.
    pub fn is_split(&self) -> bool {
        self.split.is_some()
    }

    /// A human-facing version string, falling back to the version code.
    pub fn version_display(&self) -> String {
        match (&self.version_name, self.version_code) {
            (Some(name), Some(code)) => format!("{name} ({code})"),
            (Some(name), None) => name.clone(),
            (None, Some(code)) => code.to_string(),
            (None, None) => "unknown".to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("could not read AndroidManifest.xml: {0}")]
    Axml(#[from] axml::AxmlError),
    #[error("AndroidManifest.xml has no <manifest> element")]
    NoManifestElement,
    #[error("AndroidManifest.xml declares no package name")]
    NoPackage,
}

/// Read an `AndroidManifest.xml` blob.
pub fn parse(data: &[u8]) -> Result<ManifestInfo, ManifestError> {
    let events = axml::parse(data)?;
    let mut info = ManifestInfo::default();
    let mut saw_manifest = false;

    // State for finding the launcher activity.
    let mut current_component: Option<String> = None;
    let mut in_intent_filter = false;
    let mut filter_has_main = false;
    let mut filter_has_launcher = false;

    for event in &events {
        match event {
            Event::Start { name, attrs } => match name.as_str() {
                "manifest" => {
                    saw_manifest = true;
                    for a in attrs {
                        if !a.is_android_or_bare() {
                            continue;
                        }
                        match a.name.as_str() {
                            "package" => {
                                if let Some(s) = a.value.as_str() {
                                    info.package = s.to_string();
                                }
                            }
                            "split" => {
                                if let Some(s) = a.value.as_str() {
                                    if !s.is_empty() {
                                        info.split = Some(s.to_string());
                                    }
                                }
                            }
                            "versionCode" => {
                                info.version_code = a.value.as_u32().map(u64::from);
                            }
                            "versionName" => {
                                info.version_name = a.value.as_str().map(str::to_string);
                            }
                            _ => {}
                        }
                    }
                }
                "uses-sdk" => {
                    for a in attrs {
                        match a.name.as_str() {
                            "minSdkVersion" => info.min_sdk = a.value.as_u32(),
                            "targetSdkVersion" => info.target_sdk = a.value.as_u32(),
                            _ => {}
                        }
                    }
                }
                "application" => {
                    for a in attrs {
                        match a.name.as_str() {
                            "label" => {
                                // Only literal labels are usable here.
                                if let Some(s) = a.value.as_str() {
                                    if !s.is_empty() {
                                        info.label = Some(s.to_string());
                                    }
                                }
                            }
                            "icon" | "roundIcon" => {
                                if info.icon_resource.is_none() {
                                    info.icon_resource = a.value.as_reference();
                                }
                            }
                            "extractNativeLibs" => {
                                if let Value::Bool(b) = a.value {
                                    info.extract_native_libs = Some(b);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "activity" | "activity-alias" => {
                    current_component = attrs
                        .iter()
                        .find(|a| a.name == "name")
                        .and_then(|a| a.value.as_str())
                        .map(str::to_string);
                }
                "intent-filter" => {
                    in_intent_filter = true;
                    filter_has_main = false;
                    filter_has_launcher = false;
                }
                "action" if in_intent_filter => {
                    if attrs
                        .iter()
                        .any(|a| a.value.as_str() == Some("android.intent.action.MAIN"))
                    {
                        filter_has_main = true;
                    }
                }
                "category"
                    if in_intent_filter
                        && attrs.iter().any(|a| {
                            a.value.as_str() == Some("android.intent.category.LAUNCHER")
                        }) =>
                {
                    filter_has_launcher = true;
                }
                _ => {}
            },
            Event::End { name } => match name.as_str() {
                "intent-filter" => {
                    if filter_has_main && filter_has_launcher && info.launchable_activity.is_none()
                    {
                        if let Some(component) = &current_component {
                            info.launchable_activity = Some(component.clone());
                        }
                    }
                    in_intent_filter = false;
                }
                "activity" | "activity-alias" => current_component = None,
                _ => {}
            },
        }
    }

    if !saw_manifest {
        return Err(ManifestError::NoManifestElement);
    }
    if info.package.is_empty() {
        return Err(ManifestError::NoPackage);
    }

    // `android:name=".MainActivity"` is shorthand for package + name.
    if let Some(activity) = info.launchable_activity.take() {
        info.launchable_activity = Some(qualify(&info.package, &activity));
    }

    Ok(info)
}

/// Expand a relative component name against its package.
fn qualify(package: &str, component: &str) -> String {
    if let Some(rest) = component.strip_prefix('.') {
        format!("{package}.{rest}")
    } else if component.contains('.') {
        component.to_string()
    } else {
        format!("{package}.{component}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk::axml_fixture;

    #[test]
    fn reads_a_base_manifest() {
        let info = parse(&axml_fixture::manifest_axml()).expect("fixture parses");
        assert_eq!(info.package, "com.example.teapot");
        assert_eq!(info.version_code, Some(4210));
        assert_eq!(info.version_name.as_deref(), Some("2.1.0"));
        assert_eq!(info.label.as_deref(), Some("Teapot"));
        assert_eq!(info.min_sdk, Some(24));
        assert_eq!(info.target_sdk, Some(34));
        assert_eq!(info.icon_resource, Some(0x7f08_0000));
        assert_eq!(
            info.launchable_activity.as_deref(),
            Some("com.example.teapot.MainActivity")
        );
        assert!(!info.is_split());
    }

    #[test]
    fn reads_a_split_manifest() {
        let info =
            parse(&axml_fixture::split_manifest_axml("config.arm64_v8a")).expect("fixture parses");
        assert_eq!(info.package, "com.example.teapot");
        assert_eq!(info.split.as_deref(), Some("config.arm64_v8a"));
        assert!(info.is_split());
        assert_eq!(info.launchable_activity, None);
    }

    #[test]
    fn version_display_degrades_gracefully() {
        let mut info = ManifestInfo {
            package: "a.b".into(),
            ..Default::default()
        };
        assert_eq!(info.version_display(), "unknown");
        info.version_code = Some(7);
        assert_eq!(info.version_display(), "7");
        info.version_name = Some("1.2".into());
        assert_eq!(info.version_display(), "1.2 (7)");
    }

    #[test]
    fn qualify_handles_all_three_shapes() {
        assert_eq!(qualify("a.b", ".Main"), "a.b.Main");
        assert_eq!(qualify("a.b", "c.d.Main"), "c.d.Main");
        assert_eq!(qualify("a.b", "Main"), "a.b.Main");
    }

    #[test]
    fn garbage_input_is_an_error_not_a_panic() {
        assert!(parse(b"definitely not axml").is_err());
        assert!(parse(&[0xFF; 64]).is_err());
    }
}
