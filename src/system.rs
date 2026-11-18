//! Facts about the host: init system, kernel binder support, Wayland, GPU,
//! CPU architecture.
//!
//! Everything here only *reads*. Nothing in this module needs root, and
//! nothing assumes systemd.

use std::fs;
use std::path::{Path, PathBuf};

/// Which service manager is running, as far as we can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitSystem {
    Systemd,
    OpenRc,
    Runit,
    S6,
    Unknown,
}

impl InitSystem {
    pub fn label(self) -> &'static str {
        match self {
            InitSystem::Systemd => "systemd",
            InitSystem::OpenRc => "OpenRC",
            InitSystem::Runit => "runit",
            InitSystem::S6 => "s6",
            InitSystem::Unknown => "an unrecognised init system",
        }
    }

    /// The command that starts a system service here, for use in advice.
    pub fn start_service_hint(self, service: &str) -> String {
        match self {
            InitSystem::Systemd => format!("sudo systemctl start {service}"),
            InitSystem::OpenRc => format!("sudo rc-service {service} start"),
            InitSystem::Runit => format!("sudo sv up {service}"),
            InitSystem::S6 => format!("sudo s6-rc -u change {service}"),
            InitSystem::Unknown => {
                format!("start the `{service}` service however your system does it")
            }
        }
    }

    /// The command that makes a service start at boot.
    pub fn enable_service_hint(self, service: &str) -> String {
        match self {
            InitSystem::Systemd => format!("sudo systemctl enable --now {service}"),
            InitSystem::OpenRc => format!("sudo rc-update add {service} default"),
            InitSystem::Runit => {
                format!("sudo ln -s /etc/sv/{service} /var/service/")
            }
            InitSystem::S6 => format!("sudo s6-rc-bundle-update add default {service}"),
            InitSystem::Unknown => {
                format!("enable the `{service}` service however your system does it")
            }
        }
    }
}

/// Detect the init system from what is actually on the filesystem.
///
/// Checked in order of how specific the evidence is. We never shell out to
/// `systemctl`, because its absence is exactly what we are testing for.
pub fn init_system() -> InitSystem {
    if Path::new("/run/systemd/system").is_dir() {
        return InitSystem::Systemd;
    }
    if Path::new("/run/openrc").exists() || Path::new("/run/openrc/softlevel").exists() {
        return InitSystem::OpenRc;
    }
    if Path::new("/run/runit").exists() || Path::new("/etc/runit").is_dir() {
        return InitSystem::Runit;
    }
    if Path::new("/run/s6-rc").exists() || Path::new("/run/service").is_dir() {
        return InitSystem::S6;
    }
    // Fall back to what is merely installed.
    if Path::new("/sbin/openrc-run").exists() || Path::new("/usr/bin/openrc-run").exists() {
        return InitSystem::OpenRc;
    }
    if Path::new("/usr/lib/systemd/systemd").exists() {
        return InitSystem::Systemd;
    }
    InitSystem::Unknown
}

/// What the kernel offers for Android IPC.
#[derive(Debug, Clone, Default)]
pub struct BinderSupport {
    /// `binder` appears in `/proc/filesystems`.
    pub binderfs_filesystem: bool,
    /// A binderfs instance is mounted (usually at `/dev/binderfs`).
    pub binderfs_mounted: Option<PathBuf>,
    /// Character devices found: `/dev/binder`, `/dev/hwbinder`, `/dev/vndbinder`.
    pub devices: Vec<PathBuf>,
    /// Kernel config said `CONFIG_ANDROID_BINDER_IPC` is on, when we could
    /// read the config at all.
    pub config_binder_ipc: Option<bool>,
    /// Likewise for `CONFIG_ANDROID_BINDERFS`.
    pub config_binderfs: Option<bool>,
    /// A loadable `binder_linux` module exists even if nothing is loaded yet.
    pub module_available: bool,
}

impl BinderSupport {
    /// Can the kernel actually do Android IPC right now, or after a mount?
    pub fn usable(&self) -> bool {
        self.binderfs_filesystem
            || self.binderfs_mounted.is_some()
            || !self.devices.is_empty()
            || self.module_available
    }

    /// True only when binder is ready to use with no further setup.
    pub fn ready(&self) -> bool {
        self.binderfs_mounted.is_some() || !self.devices.is_empty()
    }
}

/// Inspect the kernel's binder support. Read-only, no root.
pub fn binder_support() -> BinderSupport {
    let mut support = BinderSupport::default();

    if let Ok(filesystems) = fs::read_to_string("/proc/filesystems") {
        support.binderfs_filesystem = filesystems
            .lines()
            .any(|line| line.split_whitespace().any(|f| f == "binder"));
    }

    if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
        for line in mounts.lines() {
            let mut fields = line.split_whitespace();
            let _source = fields.next();
            let target = fields.next();
            let fstype = fields.next();
            if fstype == Some("binder") {
                if let Some(target) = target {
                    support.binderfs_mounted = Some(PathBuf::from(unescape_mount(target)));
                    break;
                }
            }
        }
    }

    for device in ["/dev/binder", "/dev/hwbinder", "/dev/vndbinder"] {
        let path = Path::new(device);
        if path.exists() {
            support.devices.push(path.to_path_buf());
        }
    }
    // A mounted binderfs exposes its control node here.
    if let Some(mount) = &support.binderfs_mounted {
        let binder = mount.join("binder");
        if binder.exists() && !support.devices.contains(&binder) {
            support.devices.push(binder);
        }
    }

    let config = kernel_config();
    support.config_binder_ipc = config
        .as_ref()
        .map(|c| config_enabled(c, "CONFIG_ANDROID_BINDER_IPC"));
    support.config_binderfs = config
        .as_ref()
        .map(|c| config_enabled(c, "CONFIG_ANDROID_BINDERFS"));

    support.module_available = binder_module_available();

    support
}

/// `/proc/mounts` escapes spaces and friends as octal.
fn unescape_mount(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let octal = &value[i + 1..i + 4];
            if let Ok(code) = u8::from_str_radix(octal, 8) {
                out.push(code as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Read the running kernel's config, if this system exposes it.
///
/// Gentoo usually has `/proc/config.gz`; Debian-likes ship
/// `/boot/config-$(uname -r)`. Neither is guaranteed, so a `None` here means
/// "cannot tell", never "not supported".
fn kernel_config() -> Option<String> {
    if let Ok(text) = fs::read_to_string("/proc/config") {
        return Some(text);
    }
    if let Ok(text) = read_gzip_text("/proc/config.gz") {
        return Some(text);
    }
    let release = kernel_release()?;
    for candidate in [
        format!("/boot/config-{release}"),
        format!("/lib/modules/{release}/config"),
        format!("/usr/src/linux-{release}/.config"),
    ] {
        if let Ok(text) = fs::read_to_string(&candidate) {
            return Some(text);
        }
    }
    None
}

/// Decompress a gzip file into text without a compression dependency, by
/// asking `zcat`/`gzip` to do it. Returns `Err` when neither is available,
/// which callers treat as "cannot tell".
fn read_gzip_text(path: &str) -> Result<String, ()> {
    if !Path::new(path).exists() {
        return Err(());
    }
    for (program, args) in [("zcat", vec![path]), ("gzip", vec!["-dc", path])] {
        if crate::backend::cmd::which(program).is_none() {
            continue;
        }
        if let Ok(out) = crate::backend::cmd::run(program, &args) {
            if out.ok() && !out.stdout.is_empty() {
                return Ok(out.stdout);
            }
        }
    }
    Err(())
}

fn config_enabled(config: &str, key: &str) -> bool {
    config.lines().any(|line| {
        let line = line.trim();
        line == format!("{key}=y") || line == format!("{key}=m")
    })
}

/// Is a binder module present on disk, ready to be loaded?
fn binder_module_available() -> bool {
    let Some(release) = kernel_release() else {
        return false;
    };
    let roots = [
        PathBuf::from(format!("/lib/modules/{release}")),
        PathBuf::from(format!("/usr/lib/modules/{release}")),
    ];
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        // `modules.dep` lists every module the kernel knows about, which
        // beats walking the whole tree.
        if let Ok(dep) = fs::read_to_string(root.join("modules.dep")) {
            if dep
                .lines()
                .any(|line| line.contains("binder_linux.ko") || line.contains("/binder.ko"))
            {
                return true;
            }
        }
    }
    false
}

/// `uname -r`, read from `/proc/sys/kernel/osrelease` so we need no process.
pub fn kernel_release() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// What we know about the session's display server.
#[derive(Debug, Clone, Default)]
pub struct DisplayInfo {
    pub wayland_display: Option<String>,
    pub wayland_socket: Option<PathBuf>,
    pub x11_display: Option<String>,
    pub session_type: Option<String>,
    pub desktop: Option<String>,
}

impl DisplayInfo {
    pub fn is_wayland(&self) -> bool {
        self.wayland_socket.is_some() || self.session_type.as_deref() == Some("wayland")
    }
}

pub fn display_info() -> DisplayInfo {
    let wayland_display = std::env::var("WAYLAND_DISPLAY")
        .ok()
        .filter(|s| !s.is_empty());
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);

    let wayland_socket = match (&wayland_display, &runtime_dir) {
        (Some(display), Some(dir)) => {
            // An absolute WAYLAND_DISPLAY is used as-is.
            let path = if display.starts_with('/') {
                PathBuf::from(display)
            } else {
                dir.join(display)
            };
            path.exists().then_some(path)
        }
        _ => None,
    };

    DisplayInfo {
        wayland_display,
        wayland_socket,
        x11_display: std::env::var("DISPLAY").ok().filter(|s| !s.is_empty()),
        session_type: std::env::var("XDG_SESSION_TYPE")
            .ok()
            .filter(|s| !s.is_empty()),
        desktop: std::env::var("XDG_CURRENT_DESKTOP")
            .ok()
            .filter(|s| !s.is_empty()),
    }
}

/// Render nodes we can hand to the Android runtime for hardware rendering.
pub fn render_nodes() -> Vec<PathBuf> {
    let mut nodes = Vec::new();
    let Ok(entries) = fs::read_dir("/dev/dri") else {
        return nodes;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("renderD") {
            nodes.push(entry.path());
        }
    }
    nodes.sort();
    nodes
}

/// Host CPU architecture, as Android would name it.
pub fn host_abi() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "x86" => "x86",
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        other => other,
    }
}

/// Does the host run ARM code natively?
pub fn host_is_arm() -> bool {
    matches!(std::env::consts::ARCH, "aarch64" | "arm")
}

/// Which ABIs a set of APKs needs that this host cannot run natively.
pub fn foreign_abis(abis: &[String]) -> Vec<String> {
    if host_is_arm() {
        // An arm64 host runs both arm64 and 32-bit ARM.
        return Vec::new();
    }
    abis.iter()
        .filter(|abi| abi.starts_with("arm"))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_point_unescaping() {
        assert_eq!(unescape_mount("/dev/binderfs"), "/dev/binderfs");
        assert_eq!(unescape_mount("/mnt/my\\040disk"), "/mnt/my disk");
    }

    #[test]
    fn config_detection_handles_y_m_and_not_set() {
        let config = "CONFIG_ANDROID_BINDER_IPC=y\n\
                      CONFIG_ANDROID_BINDERFS=m\n\
                      # CONFIG_ANDROID_DEBUG is not set\n";
        assert!(config_enabled(config, "CONFIG_ANDROID_BINDER_IPC"));
        assert!(config_enabled(config, "CONFIG_ANDROID_BINDERFS"));
        assert!(!config_enabled(config, "CONFIG_ANDROID_DEBUG"));
        assert!(!config_enabled(config, "CONFIG_NOT_MENTIONED"));
    }

    #[test]
    fn init_hints_never_assume_systemd() {
        assert_eq!(
            InitSystem::OpenRc.start_service_hint("waydroid-container"),
            "sudo rc-service waydroid-container start"
        );
        assert_eq!(
            InitSystem::OpenRc.enable_service_hint("waydroid-container"),
            "sudo rc-update add waydroid-container default"
        );
        assert!(InitSystem::Unknown
            .start_service_hint("x")
            .contains("however your system does it"));
    }

    #[test]
    fn foreign_abis_only_matter_off_arm() {
        let abis = vec!["arm64-v8a".to_string(), "x86_64".to_string()];
        let foreign = foreign_abis(&abis);
        if host_is_arm() {
            assert!(foreign.is_empty());
        } else {
            assert_eq!(foreign, vec!["arm64-v8a".to_string()]);
        }
    }

    #[test]
    fn detection_never_panics_on_this_host() {
        let _ = init_system();
        let _ = binder_support();
        let _ = display_info();
        let _ = render_nodes();
        let _ = host_abi();
    }
}
