//! Deciding what happens when a cookie meets an app that is already there.
//!
//! The rule Android enforces, and the one that matters: an app may only be
//! replaced in place — keeping its data — by a build signed with the same
//! key. A different key means a different app, however alike the two look,
//! and letting one inherit the other's data would hand over its accounts,
//! messages and tokens.
//!
//! So this module answers one question — *what should happen?* — and answers
//! it without touching the filesystem or the runtime, which is what makes
//! every branch testable.

use crate::apk::signing::Signature;
use crate::cpk::manifest::CpkManifest;
use crate::jar::Cookie;

/// What `cookie eat` should do with an incoming cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing of this package is installed. An ordinary first install.
    Fresh,

    /// Same signer, a newer version. Install over the top; data is kept.
    Update { from: String, to: String },

    /// Same signer, the same version code. Reinstalling is harmless.
    Reinstall { version: String },

    /// Same signer, an older version. Android refuses this unless told to
    /// allow it, and allowing it can leave the app with data it cannot read.
    Downgrade { from: String, to: String },

    /// Different signing keys. This is the one that must not go ahead
    /// quietly.
    SignerMismatch {
        installed: Signature,
        incoming: Signature,
    },

    /// One side's signer could not be read, so nothing can be compared.
    SignerUnknown { reason: UnknownReason },
}

/// Why a signer could not be determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// The installed record predates signature tracking, or was written by
    /// `.cpk` format version 1, whose signature field is not comparable.
    InstalledUnknown,
    /// The incoming cookie carries no readable signature.
    IncomingUnknown,
    /// Neither side has one.
    BothUnknown,
}

impl Decision {
    /// Can this proceed without the user saying anything more?
    pub fn is_safe(&self) -> bool {
        matches!(
            self,
            Decision::Fresh | Decision::Update { .. } | Decision::Reinstall { .. }
        )
    }

    /// Should the app's existing data be kept?
    pub fn keeps_data(&self) -> bool {
        matches!(
            self,
            Decision::Update { .. } | Decision::Reinstall { .. } | Decision::Downgrade { .. }
        )
    }

    /// A one-line summary for `cookie bake`.
    pub fn headline(&self, name: &str) -> String {
        match self {
            Decision::Fresh => format!("{name} is not installed yet"),
            Decision::Update { from, to } => {
                format!("updates the installed {name} from {from} to {to}")
            }
            Decision::Reinstall { version } => {
                format!("reinstalls {name} {version}, which is already installed")
            }
            Decision::Downgrade { from, to } => {
                format!("downgrades the installed {name} from {from} to {to}")
            }
            Decision::SignerMismatch { .. } => {
                format!("is signed by a different key than the installed {name}")
            }
            Decision::SignerUnknown { .. } => {
                format!("cannot be checked against the installed {name}")
            }
        }
    }
}

/// Work out what should happen.
///
/// `installed` is what the jar knows about this package, if anything.
pub fn decide(installed: Option<&Cookie>, incoming: &CpkManifest) -> Decision {
    let Some(installed) = installed else {
        return Decision::Fresh;
    };

    let installed_signature = installed.record.signature.as_ref();
    let incoming_signature = incoming.baked.signature.as_ref();

    // Signatures first: a version comparison between two different apps is
    // meaningless, so there is no point reaching one.
    match (installed_signature, incoming_signature) {
        (Some(old), Some(new)) => match old.same_signer_as(new) {
            Some(false) => {
                return Decision::SignerMismatch {
                    installed: old.clone(),
                    incoming: new.clone(),
                }
            }
            None => {
                return Decision::SignerUnknown {
                    reason: unknown_reason(old.is_unknown(), new.is_unknown()),
                }
            }
            Some(true) => {}
        },
        (old, new) => {
            return Decision::SignerUnknown {
                reason: unknown_reason(old.is_none(), new.is_none()),
            }
        }
    }

    compare_versions(
        installed.manifest.cookie.version_code,
        incoming.cookie.version_code,
        &installed.manifest.version_display(),
        &incoming.version_display(),
    )
}

fn unknown_reason(installed_missing: bool, incoming_missing: bool) -> UnknownReason {
    match (installed_missing, incoming_missing) {
        (true, true) => UnknownReason::BothUnknown,
        (true, false) => UnknownReason::InstalledUnknown,
        _ => UnknownReason::IncomingUnknown,
    }
}

/// Same signer, so it comes down to the version code.
fn compare_versions(
    installed_code: Option<u64>,
    incoming_code: Option<u64>,
    installed_display: &str,
    incoming_display: &str,
) -> Decision {
    match (installed_code, incoming_code) {
        (Some(old), Some(new)) if new > old => Decision::Update {
            from: installed_display.to_string(),
            to: incoming_display.to_string(),
        },
        (Some(old), Some(new)) if new < old => Decision::Downgrade {
            from: installed_display.to_string(),
            to: incoming_display.to_string(),
        },
        (Some(_), Some(_)) => Decision::Reinstall {
            version: incoming_display.to_string(),
        },
        // Without version codes on both sides there is nothing to order by.
        // The signer matches, so installing over the top is safe; Android
        // enforces the real rule and says so if it disagrees.
        _ => Decision::Reinstall {
            version: incoming_display.to_string(),
        },
    }
}

/// The full explanation shown when a signer does not match.
///
/// Worth spelling out, because the safe-looking option is the wrong one: the
/// two apps share a package id, so to Android they are the same app, and
/// replacing one with the other would hand its data to a different author.
pub fn signer_mismatch_message(
    name: &str,
    package: &str,
    installed: &Signature,
    incoming: &Signature,
) -> String {
    format!(
        "{name} is already installed, but this cookie is signed by a different key.\n\
         \n\
         installed  {}  ({})\n\
         this one   {}  ({})\n\
         \n\
         Both claim the package id {package}, so Android treats them as the same app — but \
         they were built by different people, or with different keys. Replacing one with the \
         other would hand the installed app's data, including any accounts it is signed in to, \
         to a build that did not create it.\n\
         \n\
         Cookie will not do that quietly. If you are sure this is the app you want:\n  \
         cookie toss {name}            # remove the installed one and its data\n  \
         cookie eat <file.cpk>         # then install this one fresh\n\
         \n\
         Or, in one step, which also deletes the app's data:\n  \
         cookie eat <file.cpk> --replace",
        installed.short(),
        installed.scheme_list(),
        incoming.short(),
        incoming.scheme_list(),
    )
}

/// What to say when the comparison could not be made.
pub fn signer_unknown_message(name: &str, reason: UnknownReason) -> String {
    let detail = match reason {
        UnknownReason::InstalledUnknown => format!(
            "Cookie has no record of who signed the installed {name}. That happens when it \
             was installed by an older Cookie, or by something other than Cookie."
        ),
        UnknownReason::IncomingUnknown => {
            "No signature could be read from this cookie's APK. It may be unsigned, or signed \
             in a way Cookie does not recognise."
                .to_string()
        }
        UnknownReason::BothUnknown => {
            format!("Neither the installed {name} nor this cookie has a signature Cookie can read.")
        }
    };

    format!(
        "{detail}\n\
         Cookie cannot tell whether this is a genuine update, so it is letting Android decide. \
         If the keys do not match, the install will fail with a clear error and nothing will \
         be changed."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk::signing::Scheme;
    use crate::cpk::manifest::{BakedMeta, CookieMeta, CPK_FORMAT_VERSION};
    use crate::jar::InstalledRecord;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn signature(signers: &[&str]) -> Signature {
        Signature {
            signers: signers.iter().map(|s| s.to_string()).collect(),
            schemes: vec![Scheme::V2, Scheme::V3],
        }
    }

    fn manifest(
        version_code: Option<u64>,
        version_name: &str,
        sig: Option<Signature>,
    ) -> CpkManifest {
        CpkManifest {
            cpk_version: CPK_FORMAT_VERSION,
            cookie: CookieMeta {
                name: "Teapot".into(),
                package: "com.example.teapot".into(),
                version_name: Some(version_name.into()),
                version_code,
                abis: Vec::new(),
                icon: None,
                min_sdk: None,
                target_sdk: None,
                launchable_activity: None,
            },
            baked: BakedMeta {
                by: "cookie test".into(),
                at: "2026-11-18T00:00:00Z".into(),
                base_apk: "app/base.apk".into(),
                splits: Vec::new(),
                signature: sig,
                legacy_signature_sha256: None,
                files: BTreeMap::new(),
            },
        }
    }

    fn installed(version_code: Option<u64>, version_name: &str, sig: Option<Signature>) -> Cookie {
        let manifest = manifest(version_code, version_name, sig.clone());
        Cookie {
            package: manifest.cookie.package.clone(),
            record: InstalledRecord {
                installed_at: 0,
                backend: "waydroid".into(),
                source_cpk: None,
                icon: None,
                signature: sig,
            },
            manifest,
            dir: PathBuf::from("/nowhere"),
        }
    }

    #[test]
    fn nothing_installed_is_a_fresh_install() {
        let incoming = manifest(Some(1), "1.0", Some(signature(&["aa"])));
        assert_eq!(decide(None, &incoming), Decision::Fresh);
        assert!(Decision::Fresh.is_safe());
        assert!(!Decision::Fresh.keeps_data());
    }

    #[test]
    fn a_newer_version_from_the_same_key_is_an_update() {
        let old = installed(Some(4210), "2.1.0", Some(signature(&["aa"])));
        let new = manifest(Some(4300), "2.2.0", Some(signature(&["aa"])));

        let decision = decide(Some(&old), &new);
        assert_eq!(
            decision,
            Decision::Update {
                from: "2.1.0 (4210)".into(),
                to: "2.2.0 (4300)".into()
            }
        );
        assert!(decision.is_safe());
        assert!(decision.keeps_data(), "an update must keep the app's data");
    }

    #[test]
    fn the_same_version_is_a_reinstall() {
        let old = installed(Some(4210), "2.1.0", Some(signature(&["aa"])));
        let new = manifest(Some(4210), "2.1.0", Some(signature(&["aa"])));

        let decision = decide(Some(&old), &new);
        assert_eq!(
            decision,
            Decision::Reinstall {
                version: "2.1.0 (4210)".into()
            }
        );
        assert!(decision.is_safe());
        assert!(decision.keeps_data());
    }

    #[test]
    fn an_older_version_is_a_downgrade_and_is_not_automatic() {
        let old = installed(Some(4300), "2.2.0", Some(signature(&["aa"])));
        let new = manifest(Some(4210), "2.1.0", Some(signature(&["aa"])));

        let decision = decide(Some(&old), &new);
        assert_eq!(
            decision,
            Decision::Downgrade {
                from: "2.2.0 (4300)".into(),
                to: "2.1.0 (4210)".into()
            }
        );
        assert!(!decision.is_safe(), "a downgrade needs the user to say so");
    }

    #[test]
    fn a_different_key_is_a_mismatch_whatever_the_versions() {
        let old = installed(Some(4210), "2.1.0", Some(signature(&["aa"])));
        let new = manifest(Some(4300), "2.2.0", Some(signature(&["bb"])));

        let decision = decide(Some(&old), &new);
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

    /// The version comparison must never be reached when the keys differ: a
    /// higher version number from a different author is not an update.
    #[test]
    fn a_newer_version_does_not_excuse_a_different_key() {
        let old = installed(Some(1), "1.0", Some(signature(&["aa"])));
        let new = manifest(Some(999_999), "99.0", Some(signature(&["bb"])));
        assert!(matches!(
            decide(Some(&old), &new),
            Decision::SignerMismatch { .. }
        ));
    }

    #[test]
    fn sharing_one_key_among_several_counts_as_the_same_signer() {
        let old = installed(Some(1), "1.0", Some(signature(&["aa", "bb"])));
        let new = manifest(Some(2), "2.0", Some(signature(&["bb"])));
        assert!(matches!(decide(Some(&old), &new), Decision::Update { .. }));
    }

    #[test]
    fn a_missing_installed_signature_is_unknown_not_a_mismatch() {
        let old = installed(Some(1), "1.0", None);
        let new = manifest(Some(2), "2.0", Some(signature(&["aa"])));
        assert_eq!(
            decide(Some(&old), &new),
            Decision::SignerUnknown {
                reason: UnknownReason::InstalledUnknown
            }
        );
    }

    #[test]
    fn a_missing_incoming_signature_is_unknown() {
        let old = installed(Some(1), "1.0", Some(signature(&["aa"])));
        let new = manifest(Some(2), "2.0", None);
        assert_eq!(
            decide(Some(&old), &new),
            Decision::SignerUnknown {
                reason: UnknownReason::IncomingUnknown
            }
        );
    }

    #[test]
    fn neither_side_having_one_is_reported_as_both() {
        let old = installed(Some(1), "1.0", None);
        let new = manifest(Some(2), "2.0", None);
        assert_eq!(
            decide(Some(&old), &new),
            Decision::SignerUnknown {
                reason: UnknownReason::BothUnknown
            }
        );
    }

    #[test]
    fn an_empty_signature_is_unknown_rather_than_different() {
        let old = installed(Some(1), "1.0", Some(Signature::default()));
        let new = manifest(Some(2), "2.0", Some(signature(&["aa"])));
        assert!(matches!(
            decide(Some(&old), &new),
            Decision::SignerUnknown { .. }
        ));
    }

    #[test]
    fn missing_version_codes_fall_back_to_a_reinstall() {
        let old = installed(None, "unknown", Some(signature(&["aa"])));
        let new = manifest(None, "unknown", Some(signature(&["aa"])));
        assert!(matches!(
            decide(Some(&old), &new),
            Decision::Reinstall { .. }
        ));
    }

    #[test]
    fn the_mismatch_message_says_what_is_at_stake_and_what_to_do() {
        let message = signer_mismatch_message(
            "Teapot",
            "com.example.teapot",
            &signature(&["aaaaaaaaaaaaaaaaaaaa"]),
            &signature(&["bbbbbbbbbbbbbbbbbbbb"]),
        );
        assert!(message.contains("different key"), "{message}");
        assert!(message.contains("com.example.teapot"), "{message}");
        assert!(message.contains("cookie toss"), "{message}");
        assert!(message.contains("--replace"), "{message}");
        // Both digests must appear, so the user can check them.
        assert!(message.contains("aaaaaaaaaaaaaaaa"), "{message}");
        assert!(message.contains("bbbbbbbbbbbbbbbb"), "{message}");
    }

    #[test]
    fn the_unknown_message_explains_each_case() {
        for reason in [
            UnknownReason::InstalledUnknown,
            UnknownReason::IncomingUnknown,
            UnknownReason::BothUnknown,
        ] {
            let message = signer_unknown_message("Teapot", reason);
            assert!(message.contains("Android"), "{reason:?}: {message}");
            assert!(!message.is_empty());
        }
    }

    #[test]
    fn headlines_name_the_app() {
        let decisions = [
            Decision::Fresh,
            Decision::Update {
                from: "1".into(),
                to: "2".into(),
            },
            Decision::Reinstall {
                version: "1".into(),
            },
            Decision::Downgrade {
                from: "2".into(),
                to: "1".into(),
            },
            Decision::SignerMismatch {
                installed: signature(&["aa"]),
                incoming: signature(&["bb"]),
            },
            Decision::SignerUnknown {
                reason: UnknownReason::BothUnknown,
            },
        ];
        for decision in decisions {
            assert!(
                decision.headline("Teapot").contains("Teapot"),
                "{decision:?}"
            );
        }
    }
}
