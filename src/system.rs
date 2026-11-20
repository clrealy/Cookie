//! Facts about the host: init system, kernel binder support, Wayland, GPU,
//! CPU architecture.
//!
//! Everything here only *reads*. Nothing in this module needs root, and
//! nothing assumes systemd.

use std::fs;
use std::path::{Path, PathBuf};

/// Which distribution this is, as far as `/etc/os-release` says.
///
/// Used only to order advice: Cookie behaves identically everywhere, but
/// telling an Arch user to run `emerge` first is noise they have to read
/// past, and the command that matters to them should be the one they see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distro {
    Arch,
    Debian,
    Fedora,
    Gentoo,
    Alpine,
    Void,
    OpenSuse,
    Unknown,
}

impl Distro {
    /// How to name it in advice.
    pub fn label(self) -> &'static str {
        match self {
            Distro::Arch => "Arch",
            Distro::Debian => "Debian",
            Distro::Fedora => "Fedora",
            Distro::Gentoo => "Gentoo",
            Distro::Alpine => "Alpine",
            Distro::Void => "Void",
            Distro::OpenSuse => "openSUSE",
            Distro::Unknown => "other",
        }
    }
}

/// What distribution is this?
pub fn distro() -> Distro {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    distro_from_os_release(&text)
}

/// Split out so the matching can be tested against real `os-release` files.
fn distro_from_os_release(text: &str) -> Distro {
    let field = |key: &str| -> Option<String> {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(&format!("{key}=")))
            .map(|value| value.trim().trim_matches('"').to_ascii_lowercase())
    };

    // `ID` is exact; `ID_LIKE` catches the derivatives, of which there are
    // many more than there are bases.
    let id = field("ID").unwrap_or_default();
    let like = field("ID_LIKE").unwrap_or_default();

    for candidate in std::iter::once(id.as_str()).chain(like.split_whitespace()) {
        let found = match candidate {
            "arch" | "archarm" | "manjaro" | "endeavouros" | "cachyos" => Distro::Arch,
            "debian" | "ubuntu" | "linuxmint" | "pop" | "raspbian" => Distro::Debian,
            "fedora" | "rhel" | "centos" | "nobara" => Distro::Fedora,
            "gentoo" => Distro::Gentoo,
            "alpine" => Distro::Alpine,
            "void" => Distro::Void,
            "opensuse" | "opensuse-tumbleweed" | "opensuse-leap" | "suse" => Distro::OpenSuse,
            _ => continue,
        };
        return found;
    }
    Distro::Unknown
}

/// Render per-distribution install advice, with this machine's first.
///
/// Every option is still shown — detection can be wrong, and someone reading
/// over a shoulder may be on something else — but the one that applies here
/// is the one at the top.
pub fn install_advice(options: &[(Distro, String)]) -> String {
    let here = distro();
    let width = options
        .iter()
        .map(|(d, _)| d.label().len())
        .max()
        .unwrap_or(0)
        + 1;

    let mut ordered: Vec<&(Distro, String)> = options.iter().collect();
    ordered.sort_by_key(|(d, _)| *d != here);

    ordered
        .iter()
        .map(|(d, command)| {
            format!(
                "  {:<width$} {command}",
                format!("{}:", d.label()),
                width = width
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Advice for installing something, honest about whether this machine can.
///
/// Where the package manager can be asked and says no, saying "run pacman -S
/// waydroid" is advice that fails — on Arch, Waydroid is not in the official
/// repositories. So the answer is checked rather than assumed, and where it
/// cannot be checked the upstream instructions carry the weight instead of a
/// package name baked into this binary years ago.
pub fn install_guidance(package: &str, options: &[(Distro, String)], upstream: &str) -> String {
    let here = distro();

    if package_in_repositories(package) == Some(false) {
        // Carefully worded. A package manager that does not list something
        // may be missing it, or may just have a stale index — this container
        // reports `lxc` as absent on Debian purely because `apt update` has
        // never run. Stating what was observed is true either way; stating
        // "it is not packaged for Debian" would not be.
        let user_repository = if here == Distro::Arch {
            "\nOn Arch, anything outside the official repositories lives in the AUR, reached \
             with a helper such as `yay` or `paru`."
        } else {
            ""
        };
        return format!(
            "Your package manager does not currently list `{package}`. Either it is not \
             packaged for {}, or its package index needs refreshing.{user_repository}\n\
             The project's own instructions cover every distribution:\n  {upstream}",
            here.label()
        );
    }

    format!(
        "Install it with your package manager, for example:\n{}\n\
         If it is not in your repositories, the project's own instructions cover every \
         distribution:\n  {upstream}",
        install_advice(options)
    )
}

/// Ask this distribution's package manager whether it has a package.
///
/// Cookie runs on the user's machine, so it does not have to carry a baked-in
/// table of which distribution packages what — a table that is wrong the day
/// something moves between repositories. It asks.
///
/// `None` means the question could not be put: no package manager found, or
/// one whose query is too slow to run inside an error message. Callers then
/// give advice that does not depend on the answer.
pub fn package_in_repositories(name: &str) -> Option<bool> {
    use crate::backend::cmd;

    // Only managers whose metadata lookup is local and fast. Portage's
    // search walks the tree and can take many seconds, which is far too long
    // to spend building an error message, so Gentoo is deliberately absent.
    let (program, args): (&str, Vec<&str>) = match distro() {
        Distro::Arch => ("pacman", vec!["-Si", name]),
        Distro::Debian => ("apt-cache", vec!["show", name]),
        Distro::Fedora => ("dnf", vec!["--cacheonly", "info", name]),
        Distro::Alpine => ("apk", vec!["search", "-e", name]),
        Distro::Void => ("xbps-query", vec!["-R", name]),
        Distro::OpenSuse => ("zypper", vec!["--non-interactive", "info", name]),
        Distro::Gentoo | Distro::Unknown => return None,
    };

    cmd::which(program)?;

    let out = cmd::run(program, &args).ok()?;
    // `apk search -e` exits zero either way, so an empty result means no.
    if program == "apk" {
        return Some(!out.stdout.trim().is_empty());
    }
    Some(out.ok())
}

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
        let su = crate::privilege::name();
        match self {
            InitSystem::Systemd => format!("{su} systemctl start {service}"),
            InitSystem::OpenRc => format!("{su} rc-service {service} start"),
            InitSystem::Runit => format!("{su} sv up {service}"),
            InitSystem::S6 => format!("{su} s6-rc -u change {service}"),
            InitSystem::Unknown => {
                format!("start the `{service}` service however your system does it")
            }
        }
    }

    /// The command that makes a service start at boot.
    pub fn enable_service_hint(self, service: &str) -> String {
        let su = crate::privilege::name();
        match self {
            InitSystem::Systemd => format!("{su} systemctl enable --now {service}"),
            InitSystem::OpenRc => format!("{su} rc-update add {service} default"),
            InitSystem::Runit => {
                format!("{su} ln -s /etc/sv/{service} /var/service/")
            }
            InitSystem::S6 => format!("{su} s6-rc-bundle-update add default {service}"),
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
        let su = crate::privilege::name();
        assert_eq!(
            InitSystem::OpenRc.start_service_hint("waydroid-container"),
            format!("{su} rc-service waydroid-container start")
        );
        assert_eq!(
            InitSystem::OpenRc.enable_service_hint("waydroid-container"),
            format!("{su} rc-update add waydroid-container default")
        );
        assert!(InitSystem::Unknown
            .start_service_hint("x")
            .contains("however your system does it"));
    }

    /// The hints have to name a program the user actually has. On a system
    /// with doas and no sudo, advice saying "sudo" is no advice at all.
    #[test]
    fn init_hints_name_whichever_privilege_command_is_here() {
        let su = crate::privilege::name();
        assert!(["doas", "sudo"].contains(&su));

        for hint in [
            InitSystem::Systemd.start_service_hint("x"),
            InitSystem::OpenRc.start_service_hint("x"),
            InitSystem::Runit.start_service_hint("x"),
            InitSystem::Systemd.enable_service_hint("x"),
            InitSystem::OpenRc.enable_service_hint("x"),
        ] {
            assert!(hint.starts_with(su), "{hint}");
            // And no leftover placeholder.
            assert!(!hint.contains("{su}"), "{hint}");
        }
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

#[cfg(test)]
mod distro_tests {
    use super::*;

    #[test]
    fn os_release_id_is_matched() {
        assert_eq!(distro_from_os_release("ID=arch\n"), Distro::Arch);
        assert_eq!(distro_from_os_release("ID=gentoo\n"), Distro::Gentoo);
        assert_eq!(distro_from_os_release("ID=\"fedora\"\n"), Distro::Fedora);
        assert_eq!(distro_from_os_release("ID=void\n"), Distro::Void);
    }

    /// There are far more derivatives than bases, and they all set ID_LIKE.
    #[test]
    fn derivatives_are_matched_through_id_like() {
        assert_eq!(
            distro_from_os_release("ID=endeavouros\nID_LIKE=arch\n"),
            Distro::Arch
        );
        assert_eq!(
            distro_from_os_release("ID=ubuntu\nID_LIKE=debian\n"),
            Distro::Debian
        );
        assert_eq!(
            distro_from_os_release("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"),
            Distro::Debian
        );
        assert_eq!(
            distro_from_os_release("ID=nobara\nID_LIKE=\"fedora\"\n"),
            Distro::Fedora
        );
    }

    #[test]
    fn an_exact_id_beats_a_vaguer_id_like() {
        // CachyOS is arch-based; the ID should win over anything in ID_LIKE.
        assert_eq!(
            distro_from_os_release("ID=cachyos\nID_LIKE=arch\n"),
            Distro::Arch
        );
    }

    #[test]
    fn something_unrecognised_is_unknown_rather_than_a_guess() {
        assert_eq!(distro_from_os_release("ID=plan9\n"), Distro::Unknown);
        assert_eq!(distro_from_os_release(""), Distro::Unknown);
        assert_eq!(distro_from_os_release("garbage"), Distro::Unknown);
    }

    #[test]
    fn advice_puts_this_machines_distribution_first() {
        let options = vec![
            (Distro::Gentoo, "emerge --ask x".to_string()),
            (Distro::Arch, "pacman -S x".to_string()),
            (Distro::Debian, "apt install x".to_string()),
        ];
        let rendered = install_advice(&options);

        // Every option is still shown — detection can be wrong.
        assert!(rendered.contains("emerge"), "{rendered}");
        assert!(rendered.contains("pacman"), "{rendered}");
        assert!(rendered.contains("apt install"), "{rendered}");

        // And the one for this machine, if it is in the list, leads.
        let here = distro();
        if options.iter().any(|(d, _)| *d == here) {
            let first = rendered.lines().next().unwrap_or_default();
            assert!(
                first.contains(here.label()),
                "{} should lead:\n{rendered}",
                here.label()
            );
        }
    }

    #[test]
    fn advice_lines_up_in_a_column() {
        let rendered = install_advice(&[
            (Distro::Arch, "a".to_string()),
            (Distro::OpenSuse, "b".to_string()),
        ]);
        let starts: Vec<usize> = rendered
            .lines()
            .map(|l| l.rfind(' ').unwrap_or(0))
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0], starts[1], "commands must align:\n{rendered}");
    }

    #[test]
    fn asking_the_package_manager_never_panics_here() {
        // Whatever this machine is, the question must be safe to put.
        let _ = package_in_repositories("definitely-not-a-real-package-xyz");
    }
}

#[cfg(test)]
mod guidance_tests {
    use super::*;

    /// A package manager with a stale index reports things as absent that
    /// are not. The wording has to survive that, so it says what was seen
    /// rather than making a claim about the distribution.
    #[test]
    fn absence_is_reported_as_an_observation_not_a_fact() {
        let guidance = install_guidance(
            "definitely-not-a-real-package-xyz",
            &[(Distro::Arch, "pacman -S x".to_string())],
            "https://example.invalid/install",
        );

        if guidance.contains("does not currently list") {
            assert!(
                guidance.contains("index needs refreshing"),
                "a stale index is the other explanation, and must be offered:\n{guidance}"
            );
            assert!(
                !guidance.contains("is not in"),
                "that would be a claim about the distribution:\n{guidance}"
            );
        }
        // Either way, upstream is named.
        assert!(
            guidance.contains("https://example.invalid/install"),
            "{guidance}"
        );
    }

    /// The AUR exists on Arch and nowhere else, so mentioning it anywhere
    /// else is noise at best and wrong at worst.
    #[test]
    fn the_aur_is_mentioned_only_on_arch() {
        let guidance = install_guidance(
            "definitely-not-a-real-package-xyz",
            &[(Distro::Arch, "pacman -S x".to_string())],
            "https://example.invalid/install",
        );
        if distro() != Distro::Arch {
            assert!(
                !guidance.contains("AUR"),
                "the AUR has nothing to do with {}:\n{guidance}",
                distro().label()
            );
        }
    }

    #[test]
    fn guidance_never_carries_a_placeholder() {
        let guidance = install_guidance(
            "bash",
            &[
                (Distro::Arch, "pacman -S bash".to_string()),
                (Distro::Debian, "apt install bash".to_string()),
            ],
            "https://example.invalid/install",
        );
        assert!(!guidance.contains("{}"), "{guidance}");
        assert!(!guidance.contains("{su}"), "{guidance}");
    }
}
