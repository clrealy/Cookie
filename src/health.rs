//! `cookie burnt` — the health check.
//!
//! Every check is read-only and says, in plain words, what to do when it
//! fails. Nothing here runs `sudo`; where root is genuinely required, the
//! check prints the command for the user's own init system and leaves the
//! decision to them.

use anyhow::Result;

use crate::backend::{AndroidBackend, OvenState};
use crate::jar;
use crate::system::{self, InitSystem};
use crate::ui::{self, Level};

/// One line of the report.
#[derive(Debug, Clone)]
pub struct Check {
    pub level: Level,
    pub label: String,
    pub detail: String,
    /// What to do about it, when something is wrong.
    pub fix: Option<String>,
}

impl Check {
    fn ok(label: &str, detail: impl Into<String>) -> Self {
        Self {
            level: Level::Ok,
            label: label.to_string(),
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(label: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            level: Level::Warn,
            label: label.to_string(),
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(label: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            level: Level::Fail,
            label: label.to_string(),
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// The whole report.
#[derive(Debug, Default)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn failures(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.level == Level::Fail)
            .count()
    }

    pub fn warnings(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.level == Level::Warn)
            .count()
    }

    pub fn is_healthy(&self) -> bool {
        self.failures() == 0
    }
}

/// Run every check.
pub fn run(backend: &dyn AndroidBackend) -> Result<Report> {
    let checks = vec![
        check_kernel_binder(),
        check_binderfs_mount(),
        check_display(),
        check_gpu(),
        check_backend(backend),
        check_oven(backend),
        check_arm_translation(backend),
    ];

    Ok(Report { checks })
}

/// Does this kernel have the Android IPC driver at all?
///
/// Cookie never builds or ships a kernel module. If binder is missing, the
/// only honest answer is which config options to turn on.
fn check_kernel_binder() -> Check {
    let support = system::binder_support();
    let release = system::kernel_release().unwrap_or_else(|| "unknown".to_string());

    let kernel_fix = format!(
        "Your kernel ({release}) needs Android binder support. Cookie will not build or load \
         a kernel module for you.\n\
         Enable these and rebuild your kernel:\n  \
         CONFIG_ANDROID_BINDER_IPC=y   (or =m)\n  \
         CONFIG_ANDROID_BINDERFS=y     (or =m)\n\
         On Gentoo:\n  \
         Device Drivers -> Android -> Android Drivers\n  \
         sudo make menuconfig && sudo make && sudo make modules_install && sudo make install\n\
         Some distributions ship it as a separate package instead, such as \
         `binder_linux-dkms` on Debian or `binder_linux-dkms` from the AUR."
    );

    if support.ready() {
        let how = match (&support.binderfs_mounted, support.devices.first()) {
            (Some(mount), _) => format!("binderfs mounted at {}", mount.display()),
            (None, Some(device)) => format!("{} present", device.display()),
            (None, None) => "available".to_string(),
        };
        return Check::ok("kernel binder support", how);
    }

    if support.binderfs_filesystem {
        return Check::warn(
            "kernel binder support",
            "the kernel knows binderfs, but nothing is mounted",
            "The backend mounts binderfs when it starts. Try:\n  cookie oven start",
        );
    }

    if support.module_available {
        return Check::warn(
            "kernel binder support",
            "a binder module is installed but not loaded",
            "Load it now:\n  \
             sudo modprobe binder_linux\n\
             To load it at every boot, add `binder_linux` to your module list \
             (on OpenRC that is /etc/modules-load.d/binder.conf).",
        );
    }

    match (support.config_binder_ipc, support.config_binderfs) {
        (Some(false), _) | (_, Some(false)) => Check::fail(
            "kernel binder support",
            "your kernel config has it switched off",
            kernel_fix,
        ),
        (None, None) => Check::fail(
            "kernel binder support",
            "no binder devices, and your kernel config is not readable",
            format!(
                "{kernel_fix}\n\
                 Cookie could not read your kernel config, so it is guessing. To let it check, \
                 enable CONFIG_IKCONFIG_PROC (which gives /proc/config.gz)."
            ),
        ),
        _ => Check::fail(
            "kernel binder support",
            "no binder devices found",
            kernel_fix,
        ),
    }
}

/// Is binderfs actually mounted where the runtime expects it?
fn check_binderfs_mount() -> Check {
    let support = system::binder_support();
    match &support.binderfs_mounted {
        Some(mount) => Check::ok("binderfs mount", mount.display().to_string()),
        None if !support.devices.is_empty() => Check::ok(
            "binderfs mount",
            "not needed: legacy binder devices are present",
        ),
        None if support.binderfs_filesystem => Check::warn(
            "binderfs mount",
            "not mounted yet",
            "The backend mounts it on start, so this usually sorts itself out:\n  \
             cookie oven start\n\
             To mount it by hand:\n  \
             sudo mkdir -p /dev/binderfs && sudo mount -t binder binder /dev/binderfs",
        ),
        None => Check::warn(
            "binderfs mount",
            "nothing to mount until the kernel supports binder",
            "Fix the kernel binder check above first.",
        ),
    }
}

/// Is there a Wayland session for app windows to appear in?
fn check_display() -> Check {
    let display = system::display_info();
    let desktop = display.desktop.clone().unwrap_or_default();

    if let Some(socket) = &display.wayland_socket {
        let detail = if desktop.is_empty() {
            format!("Wayland at {}", socket.display())
        } else {
            format!("Wayland at {} on {desktop}", socket.display())
        };
        return Check::ok("display server", detail);
    }

    if display.wayland_display.is_some() {
        return Check::warn(
            "display server",
            "WAYLAND_DISPLAY is set but its socket is missing",
            "Check $XDG_RUNTIME_DIR is set and points at your session's runtime directory.",
        );
    }

    if display.x11_display.is_some() {
        return Check::fail(
            "display server",
            "this is an X11 session",
            "The Android runtime draws through Wayland, so app windows need a Wayland session.\n\
             On KDE Plasma, log out and pick \"Plasma (Wayland)\" at the login screen.\n\
             On X11 you can still run it inside a nested compositor such as `cage` or `weston`.",
        );
    }

    Check::fail(
        "display server",
        "no graphical session detected",
        "Run `cookie burnt` from inside your desktop session, not over a bare SSH connection.",
    )
}

/// Is there a GPU render node for hardware acceleration?
fn check_gpu() -> Check {
    let nodes = system::render_nodes();
    if nodes.is_empty() {
        return Check::warn(
            "gpu",
            "no render node under /dev/dri",
            "Apps will fall back to software rendering, which is slow but works.\n\
             If you have a GPU, make sure its driver is loaded and that you are in the \
             `video` or `render` group:\n  \
             sudo usermod -aG video,render $USER\n\
             Then log out and back in.",
        );
    }

    let readable = nodes
        .iter()
        .any(|node| std::fs::OpenOptions::new().read(true).open(node).is_ok());

    let names: Vec<String> = nodes
        .iter()
        .map(|n| {
            n.file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();

    if readable {
        Check::ok("gpu", format!("render node {}", names.join(", ")))
    } else {
        Check::warn(
            "gpu",
            format!("{} present but not readable by you", names.join(", ")),
            "Add yourself to the render group so the Android runtime can use the GPU:\n  \
             sudo usermod -aG video,render $USER\n\
             Then log out and back in.",
        )
    }
}

/// Is the backend installed at all?
fn check_backend(backend: &dyn AndroidBackend) -> Check {
    match backend.ensure_available() {
        Ok(()) => Check::ok("backend", format!("{} is installed", backend.name())),
        Err(err) => Check::fail(
            "backend",
            format!("{} is not available", backend.name()),
            err.to_string(),
        ),
    }
}

/// Is the shared Android runtime up?
fn check_oven(backend: &dyn AndroidBackend) -> Check {
    let status = match backend.status() {
        Ok(status) => status,
        Err(err) => {
            return Check::warn(
                "oven",
                "could not be queried",
                format!("{err}\nTry: cookie oven status"),
            )
        }
    };

    let init = system::init_system();
    match status.state {
        OvenState::Running => {
            let detail = match &status.ip_address {
                Some(ip) => format!("running at {ip}"),
                None => "running".to_string(),
            };
            Check::ok("oven", detail)
        }
        OvenState::Stopped => {
            if status.container_running == Some(false) {
                Check::warn(
                    "oven",
                    "stopped, and its privileged container service is not running",
                    format!(
                        "The container service needs root, so start it yourself:\n  {}\n\
                         To start it at boot:\n  {}\n\
                         Then: cookie oven start",
                        init.start_service_hint("waydroid-container"),
                        init.enable_service_hint("waydroid-container")
                    ),
                )
            } else {
                Check::warn("oven", "stopped", "Start it with:\n  cookie oven start")
            }
        }
        OvenState::NotInitialized => Check::fail(
            "oven",
            "the Android image has never been set up",
            "This downloads a system image and needs root, so run it once yourself:\n  \
             sudo waydroid init\n\
             Add `-s GAPPS` if you want Google apps. Then: cookie oven start",
        ),
        OvenState::NotInstalled => Check::fail(
            "oven",
            "no backend installed",
            "See the backend check above.",
        ),
        OvenState::Unknown(ref what) => Check::warn(
            "oven",
            format!("in an unfamiliar state: {what}"),
            "Have a look at the raw status:\n  cookie oven status --verbose",
        ),
    }
}

/// Can this host run ARM apps?
fn check_arm_translation(backend: &dyn AndroidBackend) -> Check {
    let host = system::host_abi();

    if system::host_is_arm() {
        return Check::ok(
            "arm translation",
            format!("not needed: this host is {host}"),
        );
    }

    match backend.arm_translation() {
        Ok(Some(translation)) if translation.available => Check::ok(
            "arm translation",
            match translation.implementation {
                Some(name) => format!("{name} on {host}"),
                None => format!("available on {host}"),
            },
        ),
        Ok(Some(_)) => Check::warn(
            "arm translation",
            format!("none installed, and this host is {host}"),
            "Apps built only for ARM will not start. Apps with x86_64 code run fine.\n\
             To add translation, install libhoudini or ndk-translation into the Android image. \
             The usual route is the community `waydroid_script` helper:\n  \
             https://github.com/casualsnek/waydroid_script\n\
             Cookie will not download it for you.",
        ),
        Ok(None) => Check::warn(
            "arm translation",
            "could not be determined",
            "Start the oven and check again:\n  cookie oven start && cookie burnt",
        ),
        Err(err) => Check::warn(
            "arm translation",
            "could not be determined",
            err.to_string(),
        ),
    }
}

/// Print a report.
pub fn print(report: &Report, verbose: bool) {
    ui::heading("Checking the oven");
    for check in &report.checks {
        ui::status_line(check.level, &check.label, &check.detail);
        if let Some(fix) = &check.fix {
            ui::fix(fix);
        } else if verbose {
            ui::note(&ui::dim("looks good"));
        }
    }

    println!();
    let failures = report.failures();
    let warnings = report.warnings();

    if failures == 0 && warnings == 0 {
        ui::success("Everything checks out. Go bake something.");
    } else if failures == 0 {
        println!(
            "{} {}",
            ui::yellow("!"),
            format_args!(
                "{warnings} thing{} worth a look, but nothing is broken.",
                plural(warnings)
            )
        );
    } else {
        println!(
            "{} {}",
            ui::red("✘"),
            format_args!(
                "{failures} thing{} need{} fixing before apps will run.",
                plural(failures),
                if failures == 1 { "s" } else { "" }
            )
        );
    }

    print_troubleshooting();
}

/// What to do when an app installs but will not start.
///
/// The logs are almost always the answer, so point straight at them, using a
/// real installed app's name when there is one.
fn print_troubleshooting() {
    let example = jar::list()
        .ok()
        .and_then(|cookies| cookies.into_iter().next())
        .map(|c| c.name().to_string())
        .unwrap_or_else(|| "<name>".to_string());

    ui::heading("If an app will not launch");
    ui::note(&format!(
        "Read its logs — the Android side almost always says why:\n  \
         cookie crumbs {example}\n  \
         cookie crumbs {example} --follow     # watch it live while you try again\n\n\
         Then check the app's ABIs against this host:\n  \
         cookie jar"
    ));
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Used by `cookie oven status` to describe the init system in one line.
pub fn init_summary() -> String {
    let init = system::init_system();
    match init {
        InitSystem::Unknown => "init system: not recognised".to_string(),
        other => format!("init system: {}", other.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_counts_levels() {
        let report = Report {
            checks: vec![
                Check::ok("a", "fine"),
                Check::warn("b", "hmm", "do something"),
                Check::fail("c", "broken", "fix it"),
                Check::fail("d", "broken", "fix it"),
            ],
        };
        assert_eq!(report.failures(), 2);
        assert_eq!(report.warnings(), 1);
        assert!(!report.is_healthy());
    }

    #[test]
    fn a_clean_report_is_healthy() {
        let report = Report {
            checks: vec![Check::ok("a", "fine")],
        };
        assert!(report.is_healthy());
        assert_eq!(report.warnings(), 0);
    }

    #[test]
    fn every_failing_check_carries_a_fix() {
        let report = Report {
            checks: vec![
                check_kernel_binder(),
                check_binderfs_mount(),
                check_display(),
                check_gpu(),
            ],
        };
        for check in &report.checks {
            if check.level == Level::Fail || check.level == Level::Warn {
                assert!(
                    check.fix.is_some(),
                    "check `{}` has no fix text",
                    check.label
                );
            }
        }
    }

    #[test]
    fn kernel_advice_names_the_config_options() {
        let check = check_kernel_binder();
        if let Some(fix) = &check.fix {
            assert!(fix.contains("CONFIG_ANDROID_BINDER_IPC"), "{fix}");
            assert!(fix.contains("CONFIG_ANDROID_BINDERFS"), "{fix}");
        }
    }

    #[test]
    fn plurals_read_correctly() {
        assert_eq!(plural(1), "");
        assert_eq!(plural(0), "s");
        assert_eq!(plural(2), "s");
    }
}
