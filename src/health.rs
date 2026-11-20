//! `cookie burnt` — the health check.
//!
//! Every check is read-only and says, in plain words, what to do when it
//! fails. Nothing here runs `sudo`; where root is genuinely required, the
//! check prints the command for the user's own init system and leaves the
//! decision to them.

use anyhow::Result;

use crate::backend::{AndroidBackend, OvenState};
use crate::desktop::{self, DesktopDirs};
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
        check_desktop_integration(),
    ];

    // Whatever else this particular backend wants reported.
    let checks = checks
        .into_iter()
        .chain(backend.health_notes().into_iter().map(|note| Check {
            level: match (note.blocking, note.advice.is_some()) {
                (true, _) => Level::Fail,
                (false, true) => Level::Warn,
                (false, false) => Level::Ok,
            },
            label: note.label,
            detail: note.detail,
            fix: note.advice,
        }))
        .collect();

    Ok(Report { checks })
}

/// Does this kernel have the Android IPC driver at all?
///
/// Cookie never builds or ships a kernel module. If binder is missing, the
/// only honest answer is which config options to turn on.
fn check_kernel_binder() -> Check {
    kernel_binder_check(
        &system::binder_support(),
        &system::kernel_release().unwrap_or_else(|| "unknown".to_string()),
    )
}

/// The advice for a given kernel, as a pure function of what it offers.
///
/// Split out from the host lookup deliberately. A test that reads the real
/// kernel passes or fails depending on the machine it runs on, and one that
/// did exactly that got through CI here and then failed on a Gentoo box whose
/// kernel *does* have binder — asserting advice that only applies to a kernel
/// that does not. Every branch is now reachable from a test.
pub fn kernel_binder_check(support: &system::BinderSupport, release: &str) -> Check {
    let su = crate::privilege::name();

    let kernel_fix = format!(
        "Your kernel ({release}) needs Android binder support. Cookie will not build or load \
         a kernel module for you.\n\
         Enable these and rebuild your kernel:\n  \
         CONFIG_ANDROID_BINDER_IPC=y   (or =m)\n  \
         CONFIG_ANDROID_BINDERFS=y     (or =m)\n\
         On Gentoo:\n  \
         Device Drivers -> Android -> Android Drivers\n  \
         {su} make menuconfig && {su} make && {su} make modules_install && {su} make install\n\
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
            format!(
                "Load it now:\n  \
                 {su} modprobe binder_linux\n\
                 To load it at every boot, add `binder_linux` to your module list \
                 (on OpenRC that is /etc/modules-load.d/binder.conf)."
            ),
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
    binderfs_mount_check(&system::binder_support())
}

/// As above: a pure function of what the kernel offers, so every branch is
/// reachable from a test rather than only on a machine in that state.
pub fn binderfs_mount_check(support: &system::BinderSupport) -> Check {
    let su = crate::privilege::name();
    match &support.binderfs_mounted {
        Some(mount) => Check::ok("binderfs mount", mount.display().to_string()),
        None if !support.devices.is_empty() => Check::ok(
            "binderfs mount",
            "not needed: legacy binder devices are present",
        ),
        None if support.binderfs_filesystem => Check::warn(
            "binderfs mount",
            "not mounted yet",
            format!(
                "The backend mounts it on start, so this usually sorts itself out:\n  \
                 cookie oven start\n\
                 To mount it by hand:\n  \
                 {su} mkdir -p /dev/binderfs && {su} mount -t binder binder /dev/binderfs"
            ),
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
    let su = crate::privilege::name();
    let nodes = system::render_nodes();
    if nodes.is_empty() {
        return Check::warn(
            "gpu",
            "no render node under /dev/dri",
            format!(
                "Apps will fall back to software rendering, which is slow but works.\n\
                 If you have a GPU, make sure its driver is loaded and that you are in the \
                 `video` or `render` group:\n  \
                 {su} usermod -aG video,render $USER\n\
                 Then log out and back in."
            ),
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
            format!(
                "Add yourself to the render group so the Android runtime can use the GPU:\n  \
                 {su} usermod -aG video,render $USER\n\
                 Then log out and back in."
            ),
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
    let su = crate::privilege::name();
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
            format!(
                "This downloads a system image and needs root, so run it once yourself:\n  \
                 {su} waydroid init\n\
                 Add `-s GAPPS` if you want Google apps. Then: cookie oven start"
            ),
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

/// Is the desktop set up to open cookies?
///
/// Never a failure: everything still works from a terminal without it.
fn check_desktop_integration() -> Check {
    let dirs = match DesktopDirs::user() {
        Ok(dirs) => dirs,
        Err(err) => {
            return Check::warn(
                "desktop integration",
                "could not be checked",
                err.to_string(),
            )
        }
    };

    let status = desktop::status(&dirs);
    let menu = format!(
        "{} app{} in your menu",
        status.launchers,
        if status.launchers == 1 { "" } else { "s" }
    );

    if status.is_ready() {
        return Check::ok("desktop integration", menu);
    }

    let missing = [
        (
            !status.mime_registered,
            "the .cpk file type is not registered",
        ),
        (
            !status.handler_installed,
            "Cookie is not installed as a file handler",
        ),
        (
            status.mime_registered && status.handler_installed && !status.is_default_handler,
            "Cookie is not the default application for .cpk files",
        ),
    ]
    .iter()
    .filter(|(missing, _)| *missing)
    .map(|(_, what)| *what)
    .collect::<Vec<_>>()
    .join(", ");

    Check::warn(
        "desktop integration",
        missing,
        "Double-clicking a cookie will not work until this is set up. \
         Everything still works from a terminal.\n\
         Set it up with:\n  cookie kitchen setup",
    )
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

    /// Every check that is not `Ok` must say what to do about it.
    ///
    /// Driven through the pure binder checks rather than the host's own
    /// kernel, so it covers the branches this machine is not in.
    #[test]
    fn every_failing_check_carries_a_fix() {
        let mut checks = vec![check_display(), check_gpu(), check_desktop_integration()];
        for shape in [
            support(Some("/dev/binderfs"), &[], true, false, Some(true)),
            support(None, &["/dev/binder"], false, false, None),
            support(None, &[], true, false, Some(true)),
            support(None, &[], false, true, Some(true)),
            support(None, &[], false, false, Some(false)),
            support(None, &[], false, false, None),
        ] {
            checks.push(kernel_binder_check(&shape, "6.12.0"));
            checks.push(binderfs_mount_check(&shape));
        }

        for check in &checks {
            if check.level == Level::Fail || check.level == Level::Warn {
                assert!(
                    check.fix.is_some(),
                    "check `{}` has no fix text",
                    check.label
                );
            }
        }
    }

    /// Build a `BinderSupport` describing a particular kind of kernel.
    fn support(
        mounted: Option<&str>,
        devices: &[&str],
        filesystem: bool,
        module: bool,
        config: Option<bool>,
    ) -> system::BinderSupport {
        system::BinderSupport {
            binderfs_filesystem: filesystem,
            binderfs_mounted: mounted.map(std::path::PathBuf::from),
            devices: devices.iter().map(std::path::PathBuf::from).collect(),
            config_binder_ipc: config,
            config_binderfs: config,
            module_available: module,
        }
    }

    /// Regression test for a failure found on real hardware.
    ///
    /// This used to call `check_kernel_binder()`, which reads the running
    /// kernel — so it asserted whatever advice *this* machine happened to
    /// produce. It passed on a machine with no binder at all and failed on a
    /// Gentoo box whose kernel has it, because there the advice correctly
    /// says to mount binderfs rather than to rebuild the kernel.
    ///
    /// The config options belong in the advice only when the kernel actually
    /// lacks support, so that is what is asserted, branch by branch.
    #[test]
    fn the_config_options_are_named_only_when_the_kernel_lacks_support() {
        // No binder of any kind: the only useful answer is which options to
        // turn on.
        for config in [Some(false), None] {
            let check = kernel_binder_check(&support(None, &[], false, false, config), "6.12.0");
            assert_eq!(check.level, Level::Fail);
            let fix = check.fix.as_deref().unwrap_or_default();
            assert!(fix.contains("CONFIG_ANDROID_BINDER_IPC"), "{fix}");
            assert!(fix.contains("CONFIG_ANDROID_BINDERFS"), "{fix}");
        }

        // The kernel has binderfs but nothing is mounted — which is the case
        // that broke this test. Telling the user to rebuild their kernel here
        // would be wrong.
        let check = kernel_binder_check(&support(None, &[], true, false, Some(true)), "6.12.0");
        assert_eq!(check.level, Level::Warn);
        let fix = check.fix.as_deref().unwrap_or_default();
        assert!(
            !fix.contains("CONFIG_ANDROID_BINDER_IPC"),
            "the kernel already has it; this advice is wrong:\n{fix}"
        );
        assert!(fix.contains("cookie oven start"), "{fix}");
    }

    #[test]
    fn a_ready_kernel_is_reported_as_ready() {
        let mounted = kernel_binder_check(
            &support(Some("/dev/binderfs"), &[], true, false, Some(true)),
            "6.12.0",
        );
        assert_eq!(mounted.level, Level::Ok);
        assert!(mounted.detail.contains("/dev/binderfs"), "{mounted:?}");

        let legacy = kernel_binder_check(
            &support(None, &["/dev/binder"], false, false, None),
            "6.12.0",
        );
        assert_eq!(legacy.level, Level::Ok);
    }

    #[test]
    fn an_unloaded_module_is_told_to_be_loaded_not_rebuilt() {
        let check = kernel_binder_check(&support(None, &[], false, true, Some(true)), "6.12.0");
        assert_eq!(check.level, Level::Warn);
        let fix = check.fix.as_deref().unwrap_or_default();
        assert!(fix.contains("modprobe binder_linux"), "{fix}");
        assert!(!fix.contains("CONFIG_ANDROID_BINDER_IPC"), "{fix}");
    }

    /// Every branch of both binder checks, including the ones this machine is
    /// not in. The placeholder guard only covers code that actually runs, and
    /// three unsubstituted `{su}` placeholders reached users by sitting in
    /// branches no test ever entered.
    #[test]
    fn no_binder_advice_carries_an_unsubstituted_placeholder() {
        let shapes = [
            support(Some("/dev/binderfs"), &[], true, false, Some(true)),
            support(None, &["/dev/binder"], false, false, None),
            support(None, &[], true, false, Some(true)),
            support(None, &[], false, true, Some(true)),
            support(None, &[], false, false, Some(false)),
            support(None, &[], false, false, None),
        ];

        for shape in &shapes {
            for check in [
                kernel_binder_check(shape, "6.12.0"),
                binderfs_mount_check(shape),
            ] {
                assert!(
                    !check.detail.contains("{su}") && !check.detail.contains("{}"),
                    "`{}` detail: {}",
                    check.label,
                    check.detail
                );
                if let Some(fix) = &check.fix {
                    assert!(
                        !fix.contains("{su}") && !fix.contains("{}"),
                        "`{}` fix:\n{fix}",
                        check.label
                    );
                }
            }
        }
    }

    #[test]
    fn plurals_read_correctly() {
        assert_eq!(plural(1), "");
        assert_eq!(plural(0), "s");
        assert_eq!(plural(2), "s");
    }
}
