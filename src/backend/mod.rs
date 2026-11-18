//! The Android runtime behind Cookie.
//!
//! Milestone 1 drives Waydroid's CLI. Milestone 4 replaces it with Cookie's
//! own runtime — so everything above this line talks to the trait, never to
//! `waydroid` directly.

pub mod cmd;
pub mod waydroid;

use std::path::PathBuf;

use anyhow::Result;

/// Whether the shared Android runtime is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OvenState {
    /// Ready to install and launch apps.
    Running,
    /// Installed and set up, but not currently running.
    Stopped,
    /// Installed, but the Android image has never been set up.
    NotInitialized,
    /// The backend itself is not on this system.
    NotInstalled,
    /// Present but saying something we do not recognise.
    Unknown(String),
}

impl OvenState {
    pub fn is_running(&self) -> bool {
        matches!(self, OvenState::Running)
    }
}

/// A snapshot of the runtime.
#[derive(Debug, Clone)]
pub struct OvenStatus {
    pub state: OvenState,
    /// The privileged container service, which is separate from the per-user
    /// session and is the part that usually needs root to start.
    pub container_running: Option<bool>,
    pub session_running: Option<bool>,
    pub vendor_type: Option<String>,
    pub ip_address: Option<String>,
    /// Raw backend output, for `--verbose`.
    pub raw: String,
}

/// One app the backend has installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub package: String,
    pub name: Option<String>,
}

/// How an install should behave when the app is already there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// Install over an existing copy, keeping its data.
    ///
    /// Only ever set once the signing keys have been compared: Android ties
    /// an app's data to its signing key, and this is the switch that decides
    /// whether the new build inherits it.
    pub keep_data: bool,
    /// Permit a lower `versionCode` than the installed one.
    pub allow_downgrade: bool,
}

impl InstallOptions {
    /// A first install, with nothing to preserve.
    pub fn fresh() -> Self {
        Self::default()
    }

    /// An update from the same signer.
    pub fn update() -> Self {
        Self {
            keep_data: true,
            allow_downgrade: false,
        }
    }
}

/// What to show for `cookie crumbs`.
#[derive(Debug, Clone)]
pub struct LogSpec {
    pub package: String,
    /// Process id, when we managed to find one. Filtering by pid is exact;
    /// filtering by package name is a best-effort text match.
    pub pid: Option<u32>,
    pub follow: bool,
    /// How many past lines to show before following.
    pub lines: usize,
}

/// The operations Cookie needs from an Android runtime.
pub trait AndroidBackend {
    /// Short name, e.g. `waydroid`.
    fn name(&self) -> &'static str;

    /// Error out if this backend cannot be used at all, with advice.
    fn ensure_available(&self) -> Result<()>;

    fn status(&self) -> Result<OvenStatus>;

    /// Bring the runtime up. Returns once it is usable.
    fn start(&self) -> Result<()>;

    fn stop(&self) -> Result<()>;

    /// Install one app. `apks` is a base APK followed by any splits.
    fn install(&self, apks: &[PathBuf], options: &InstallOptions) -> Result<()>;

    fn uninstall(&self, package: &str) -> Result<()>;

    fn list_installed(&self) -> Result<Vec<InstalledApp>>;

    fn is_installed(&self, package: &str) -> Result<bool> {
        Ok(self
            .list_installed()?
            .iter()
            .any(|app| app.package == package))
    }

    /// Launch an app in its own window.
    fn launch(&self, package: &str) -> Result<()>;

    /// Find a running process id for a package, if it has one.
    fn pid_of(&self, package: &str) -> Result<Option<u32>>;

    /// Stream logs to stdout until the user interrupts, or until the buffer
    /// is exhausted when not following.
    fn logs(&self, spec: &LogSpec) -> Result<()>;

    /// Put the runtime into one-window-per-app mode, which is the whole point
    /// of Cookie. Called before launching.
    fn ensure_multi_window(&self) -> Result<()>;

    /// Whether ARM code can run on this host under the backend.
    /// `None` means we could not tell.
    fn arm_translation(&self) -> Result<Option<ArmTranslation>>;

    /// The Wayland app id, or X11 window class, that this backend gives an
    /// app's windows.
    ///
    /// A desktop launcher puts this in `StartupWMClass` so the taskbar
    /// matches the running window to its menu entry instead of showing a
    /// second, nameless one. `None` means the backend cannot say, and the
    /// key is then left out rather than guessed at.
    fn window_class(&self, package: &str) -> Option<String>;
}

/// How ARM apps run on a non-ARM host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmTranslation {
    pub available: bool,
    /// `libhoudini`, `ndk-translation`, or whatever the backend reports.
    pub implementation: Option<String>,
}

/// Turn an Android package-manager failure into something a person can act
/// on.
///
/// `pm` reports these as bare constants on stdout with a zero exit code, so
/// they otherwise surface as an unexplained wall of capitals.
pub fn explain_install_failure(output: &str) -> Option<&'static str> {
    const EXPLANATIONS: &[(&str, &str)] = &[
        (
            "INSTALL_FAILED_UPDATE_INCOMPATIBLE",
            "Android already has an app with this package id, signed by a different key.\n\
             Remove the installed one first — this deletes its data:\n  \
             cookie toss <name>",
        ),
        (
            "INSTALL_FAILED_VERSION_DOWNGRADE",
            "The installed version is newer than this one. Android does not downgrade an app \
             by default, because the older build may not understand the newer one's data.\n\
             To do it anyway:\n  cookie eat <file.cpk> --allow-downgrade",
        ),
        (
            "INSTALL_FAILED_NO_MATCHING_ABIS",
            "This app's native code is not built for an ABI this machine can run.\n\
             Check what is available:\n  cookie burnt",
        ),
        (
            "INSTALL_FAILED_INSUFFICIENT_STORAGE",
            "The Android image has run out of space.",
        ),
        (
            "INSTALL_FAILED_OLDER_SDK",
            "This app needs a newer Android version than the runtime provides.",
        ),
        (
            "INSTALL_PARSE_FAILED_NO_CERTIFICATES",
            "The APK is not signed, and Android will not install an unsigned app.",
        ),
        (
            "INSTALL_PARSE_FAILED_INCONSISTENT_CERTIFICATES",
            "The APKs in this cookie are not all signed by the same key, so Android will not \
             install them together.",
        ),
        (
            "INSTALL_FAILED_INVALID_APK",
            "Android could not read one of the APKs. The cookie may be damaged; try baking it \
             again from the original APK.",
        ),
        (
            "INSTALL_FAILED_MISSING_SPLIT",
            "This app is split into several APKs and one of them is missing. Bake the base APK \
             together with all of its splits.",
        ),
    ];

    let upper = output.to_ascii_uppercase();
    EXPLANATIONS
        .iter()
        .find(|(code, _)| upper.contains(code))
        .map(|(_, explanation)| *explanation)
}

/// Pick a backend.
///
/// There is one for now. When the oven lands, this is where the choice gets
/// made, and `--backend` on the command line overrides it.
pub fn detect(preference: Option<&str>) -> Result<Box<dyn AndroidBackend>> {
    match preference {
        None | Some("auto") | Some("waydroid") => Ok(Box::new(waydroid::Waydroid::new())),
        Some("oven") => anyhow::bail!(
            "the oven is not built yet, so there is nothing to switch to.\n\
             It arrives in milestone 4; until then Cookie uses Waydroid."
        ),
        Some(other) => anyhow::bail!(
            "unknown backend `{other}`.\n\
             Available backends: waydroid"
        ),
    }
}
