//! The Waydroid backend.
//!
//! Everything here goes through the `waydroid` command-line tool. Cookie adds
//! no kernel modules and no privileged daemons of its own; where root is
//! genuinely needed — starting Waydroid's container service, initialising its
//! Android image — Cookie says so and hands the user the exact command for
//! their init system rather than running `sudo` behind their back.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::cmd;
use super::{
    AndroidBackend, ArmTranslation, InstallOptions, InstalledApp, LogSpec, OvenState, OvenStatus,
};
use crate::system;
use crate::ui;

/// Name of Waydroid's privileged container service, the same on every distro.
const CONTAINER_SERVICE: &str = "waydroid-container";

/// How long to wait for a session to come up before giving up.
const SESSION_TIMEOUT: Duration = Duration::from_secs(45);

/// The Android property that gives each app its own window.
const MULTI_WINDOW_PROP: &str = "persist.waydroid.multi_windows";

pub struct Waydroid {
    program: String,
}

impl Default for Waydroid {
    fn default() -> Self {
        Self::new()
    }
}

impl Waydroid {
    pub fn new() -> Self {
        Self {
            program: "waydroid".to_string(),
        }
    }

    fn binary(&self) -> Option<PathBuf> {
        cmd::which(&self.program)
    }

    /// Run a `waydroid` subcommand, capturing output.
    fn waydroid(&self, args: &[&str]) -> Result<cmd::Output> {
        cmd::run(&self.program, args)
    }

    /// Run a `waydroid` subcommand and fail loudly on a non-zero exit.
    fn waydroid_checked(&self, args: &[&str]) -> Result<cmd::Output> {
        let out = self.waydroid(args)?;
        if out.ok() {
            return Ok(out);
        }
        let message = out.message();
        if looks_like_permission_problem(&message) {
            bail!(
                "`waydroid {}` was refused:\n{}\n\n\
                 Waydroid wants root for this one. Try:\n  \
                 sudo waydroid {}",
                args.join(" "),
                cmd::indent(&message),
                args.join(" ")
            );
        }
        bail!(
            "`waydroid {}` failed:\n{}",
            args.join(" "),
            cmd::indent(&message)
        )
    }

    /// Where Waydroid exposes the Android `/sdcard` on the host.
    ///
    /// Used to hand split APKs to `pm` inside the container, since `pm` can
    /// only read paths the container can see.
    fn shared_storage(&self) -> Result<PathBuf> {
        let base =
            dirs::data_dir().context("cannot work out your data directory (is $HOME set?)")?;
        Ok(base.join("waydroid/data/media/0"))
    }

    /// Run a command inside the Android container.
    fn shell(&self, args: &[&str]) -> Result<cmd::Output> {
        let mut full: Vec<&str> = vec!["shell", "--"];
        full.extend_from_slice(args);
        cmd::run(&self.program, &full)
    }

    /// Install a base APK on its own, which Waydroid handles directly.
    fn install_single(&self, apk: &Path) -> Result<()> {
        let path = apk.to_string_lossy().into_owned();
        let out = self.waydroid(&["app", "install", &path])?;
        if out.ok() && !mentions_failure(&out.stdout) {
            return Ok(());
        }
        let message = if out.message().is_empty() {
            out.stdout.trim().to_string()
        } else {
            out.message()
        };
        match super::explain_install_failure(&message) {
            Some(explanation) => bail!(
                "Waydroid could not install {}:\n{}\n\n{explanation}",
                apk.display(),
                cmd::indent(&message)
            ),
            None => bail!(
                "Waydroid could not install {}:\n{}\n\n\
                 Common causes:\n  \
                 - the app needs an ABI this system cannot run (check `cookie burnt`)\n  \
                 - a different build of the same app is already installed\n  \
                 - the APK is damaged",
                apk.display(),
                cmd::indent(&message)
            ),
        }
    }

    /// Install a base APK plus splits, using a `pm` install session.
    ///
    /// Waydroid's own `app install` takes a single file, so split sets go
    /// through `pm install-create` / `install-write` / `install-commit`
    /// inside the container. The APKs are staged in Waydroid's shared storage
    /// first, because `pm` reads paths from the container's point of view.
    fn install_split_set(&self, apks: &[PathBuf], options: &InstallOptions) -> Result<()> {
        let storage = self.shared_storage()?;
        if !storage.is_dir() {
            bail!(
                "cannot find Waydroid's shared storage at {}.\n\
                 It appears once the Android image has been initialised and started at least once.\n\
                 Try: cookie oven start",
                storage.display()
            );
        }

        let staging = storage.join("Cookie").join("staging");
        crate::paths::ensure_dir(&staging)?;
        // Clear anything a previous run left behind.
        if let Ok(entries) = std::fs::read_dir(&staging) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }

        let mut staged: Vec<(String, u64, String)> = Vec::new();
        for (i, apk) in apks.iter().enumerate() {
            let file_name = apk
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| {
                    n.replace(
                        |c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_',
                        "_",
                    )
                })
                .unwrap_or_else(|| format!("part{i}.apk"));
            let target = staging.join(&file_name);
            std::fs::copy(apk, &target).with_context(|| {
                format!(
                    "cannot stage {} into Waydroid's shared storage",
                    apk.display()
                )
            })?;
            let size = std::fs::metadata(&target)
                .map(|m| m.len())
                .with_context(|| format!("cannot stat {}", target.display()))?;
            // A split name has to be unique within the session.
            let split_name = if i == 0 {
                "base".to_string()
            } else {
                format!("split{i}")
            };
            staged.push((
                format!("/sdcard/Cookie/staging/{file_name}"),
                size,
                split_name,
            ));
        }

        let result = self.run_install_session(&staged, options);

        // Always clean up the staged copies, success or not.
        for (_, _, _) in &staged {}
        let _ = std::fs::remove_dir_all(&staging);

        result
    }

    /// Drive one `pm` install session to completion.
    fn run_install_session(
        &self,
        staged: &[(String, u64, String)],
        options: &InstallOptions,
    ) -> Result<()> {
        let total: u64 = staged.iter().map(|(_, size, _)| size).sum();
        let total_str = total.to_string();

        let mut create: Vec<&str> = vec!["pm", "install-create"];
        if options.keep_data {
            // -r replaces the existing app and keeps its data.
            create.push("-r");
        }
        if options.allow_downgrade {
            create.push("-d");
        }
        create.push("-S");
        create.push(&total_str);

        let created = self.shell(&create)?;
        if !created.ok() {
            let message = created.message();
            if looks_like_permission_problem(&message) {
                bail!(
                    "could not talk to the Android package manager:\n{}\n\n\
                     `waydroid shell` needs more privilege on this system. Try:\n  \
                     sudo waydroid shell -- pm install-create\n\
                     If that works, re-run the install with sudo.",
                    cmd::indent(&message)
                );
            }
            bail!(
                "could not start a package install session:\n{}",
                cmd::indent(&message)
            );
        }

        let session_id = parse_session_id(&created.stdout).ok_or_else(|| {
            anyhow::anyhow!(
                "the Android package manager did not report a session id.\nIt said:\n{}",
                cmd::indent(created.stdout.trim())
            )
        })?;

        let commit = (|| -> Result<()> {
            for (container_path, size, split_name) in staged {
                let size_str = size.to_string();
                let out = self.shell(&[
                    "pm",
                    "install-write",
                    "-S",
                    &size_str,
                    &session_id,
                    split_name,
                    container_path,
                ])?;
                if !out.ok() || mentions_failure(&out.stdout) {
                    bail!(
                        "could not add {split_name} to the install session:\n{}",
                        cmd::indent(&out.message())
                    );
                }
            }

            let out = self.shell(&["pm", "install-commit", &session_id])?;
            if !out.ok() || mentions_failure(&out.stdout) {
                let reported = format!("{}{}", out.stdout, out.stderr);
                match super::explain_install_failure(&reported) {
                    Some(explanation) => bail!(
                        "the install did not go through:\n{}\n\n{explanation}",
                        cmd::indent(reported.trim())
                    ),
                    None => bail!(
                        "the install did not go through:\n{}",
                        cmd::indent(reported.trim())
                    ),
                }
            }
            Ok(())
        })();

        if commit.is_err() {
            // Leave no half-built session behind.
            let _ = self.shell(&["pm", "install-abandon", &session_id]);
        }
        commit
    }

    /// Advice for getting the privileged container service running.
    fn container_advice(&self) -> String {
        let init = system::init_system();
        format!(
            "Waydroid's container service is not running. It is the privileged half of \
             Waydroid and needs root to start, which is why Cookie will not start it for you.\n\
             On {}:\n  {}\n\
             To have it start at boot:\n  {}",
            init.label(),
            init.start_service_hint(CONTAINER_SERVICE),
            init.enable_service_hint(CONTAINER_SERVICE)
        )
    }

    /// Is the privileged container service up?
    ///
    /// Asked without `systemctl`, so this works the same on OpenRC.
    fn container_running(&self, status_output: &str) -> Option<bool> {
        for line in status_output.lines() {
            if let Some(value) = field(line, "Container") {
                return Some(value.eq_ignore_ascii_case("RUNNING"));
            }
        }
        // Older Waydroid does not report the container separately. Fall back
        // to looking for its socket, which exists only while it runs.
        let socket = Path::new("/run/waydroid-lxc");
        if socket.exists() {
            return Some(true);
        }
        None
    }
}

impl AndroidBackend for Waydroid {
    fn name(&self) -> &'static str {
        "waydroid"
    }

    fn ensure_available(&self) -> Result<()> {
        if self.binary().is_some() {
            return Ok(());
        }
        bail!(
            "Waydroid is not installed, and milestone 1 of Cookie runs on top of it.\n\
             Install it with your package manager, for example:\n  \
             Gentoo:  sudo emerge --ask app-emulation/waydroid\n  \
             Arch:    sudo pacman -S waydroid\n  \
             Debian:  sudo apt install waydroid\n\
             Then set up its Android image once:\n  \
             sudo waydroid init\n\
             Check how things stand with:\n  cookie burnt"
        )
    }

    fn status(&self) -> Result<OvenStatus> {
        if self.binary().is_none() {
            return Ok(OvenStatus {
                state: OvenState::NotInstalled,
                container_running: None,
                session_running: None,
                vendor_type: None,
                ip_address: None,
                raw: String::new(),
            });
        }

        let out = self.waydroid(&["status"])?;
        let text = format!("{}{}", out.stdout, out.stderr);

        let session = text
            .lines()
            .find_map(|line| field(line, "Session"))
            .map(|v| v.to_string());
        let container = self.container_running(&text);
        let vendor_type = text
            .lines()
            .find_map(|line| field(line, "Vendor type"))
            .map(|v| v.to_string());
        let ip_address = text
            .lines()
            .find_map(|line| field(line, "IP address"))
            .map(|v| v.to_string());

        let lower = text.to_ascii_lowercase();
        let state = if lower.contains("not initialized") || lower.contains("not initialised") {
            OvenState::NotInitialized
        } else {
            match session.as_deref() {
                Some(s) if s.eq_ignore_ascii_case("RUNNING") => OvenState::Running,
                Some(s) if s.eq_ignore_ascii_case("STOPPED") => OvenState::Stopped,
                Some(other) => OvenState::Unknown(other.to_string()),
                None if out.ok() => OvenState::Stopped,
                None => OvenState::Unknown(out.message()),
            }
        };

        Ok(OvenStatus {
            state,
            container_running: container,
            session_running: session.map(|s| s.eq_ignore_ascii_case("RUNNING")),
            vendor_type,
            ip_address,
            raw: text,
        })
    }

    fn start(&self) -> Result<()> {
        self.ensure_available()?;
        let status = self.status()?;

        if status.state.is_running() {
            return Ok(());
        }
        if status.state == OvenState::NotInitialized {
            bail!(
                "Waydroid's Android image has not been set up yet. This downloads the system \
                 image and needs root, so run it yourself once:\n  \
                 sudo waydroid init\n\
                 Add `-s GAPPS` if you want Google apps. Then: cookie oven start"
            );
        }
        if status.container_running == Some(false) {
            bail!("{}", self.container_advice());
        }

        // `waydroid session start` stays in the foreground for as long as the
        // session lives, so it gets detached and its output parked in a log.
        let log_dir = crate::paths::cache_dir()?.join("logs");
        crate::paths::ensure_dir(&log_dir)?;
        let log_path = log_dir.join("oven.log");
        let log = std::fs::File::create(&log_path)
            .with_context(|| format!("cannot write {}", log_path.display()))?;
        let log_err = log
            .try_clone()
            .with_context(|| format!("cannot write {}", log_path.display()))?;

        Command::new(&self.program)
            .args(["session", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .with_context(|| {
                format!(
                    "cannot start a Waydroid session.\n\
                     Its output would have gone to {}",
                    log_path.display()
                )
            })?;

        // Wait for the session to actually come up.
        let deadline = Instant::now() + SESSION_TIMEOUT;
        let mut waited = false;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
            if self.status()?.state.is_running() {
                return Ok(());
            }
            if !waited {
                ui::step("waiting for the oven to warm up");
                waited = true;
            }
        }

        let tail = std::fs::read_to_string(&log_path).unwrap_or_default();
        let tail: Vec<&str> = tail.lines().rev().take(12).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!(
            "the oven did not come up within {} seconds.\n\
             Last lines from {}:\n{}\n\n\
             If the container service is not running, start it with:\n  {}",
            SESSION_TIMEOUT.as_secs(),
            log_path.display(),
            cmd::indent(&tail.join("\n")),
            system::init_system().start_service_hint(CONTAINER_SERVICE)
        )
    }

    fn stop(&self) -> Result<()> {
        self.ensure_available()?;
        if !self.status()?.state.is_running() {
            return Ok(());
        }
        self.waydroid_checked(&["session", "stop"])?;
        Ok(())
    }

    fn install(&self, apks: &[PathBuf], options: &InstallOptions) -> Result<()> {
        self.ensure_available()?;
        match apks {
            [] => bail!("no APKs to install"),
            // `waydroid app install` is the best-trodden path, but it takes
            // no flags, so anything needing one goes through `pm` directly.
            [single] if *options == InstallOptions::fresh() => self.install_single(single),
            many => self.install_split_set(many, options),
        }
    }

    fn uninstall(&self, package: &str) -> Result<()> {
        self.ensure_available()?;
        let out = self.waydroid(&["app", "remove", package])?;
        if out.ok() && !mentions_failure(&out.stdout) {
            return Ok(());
        }
        bail!(
            "Waydroid could not remove {package}:\n{}",
            cmd::indent(&out.message())
        )
    }

    fn list_installed(&self) -> Result<Vec<InstalledApp>> {
        self.ensure_available()?;
        let out = self.waydroid(&["app", "list"])?;
        if !out.ok() {
            bail!(
                "could not list the apps Waydroid has installed:\n{}",
                cmd::indent(&out.message())
            );
        }
        Ok(parse_app_list(&out.stdout))
    }

    fn launch(&self, package: &str) -> Result<()> {
        self.ensure_available()?;
        let out = self.waydroid(&["app", "launch", package])?;
        if out.ok() && !mentions_failure(&out.stdout) {
            return Ok(());
        }
        bail!(
            "Waydroid could not launch {package}:\n{}\n\n\
             See what the app itself said with:\n  cookie crumbs {package}",
            cmd::indent(&out.message())
        )
    }

    fn pid_of(&self, package: &str) -> Result<Option<u32>> {
        let out = self.shell(&["pidof", package])?;
        if !out.ok() {
            // `pidof` exits non-zero when nothing matches; that is not an
            // error, it just means the app is not running.
            return Ok(None);
        }
        Ok(out
            .stdout
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<u32>().ok()))
    }

    fn logs(&self, spec: &LogSpec) -> Result<()> {
        self.ensure_available()?;

        let mut args: Vec<String> = vec!["shell".into(), "--".into(), "logcat".into()];
        if !spec.follow {
            args.push("-d".into());
        }
        // Filter in the container when we have a pid: that is exact, and it
        // keeps the whole log off the pipe.
        if let Some(pid) = spec.pid {
            args.push(format!("--pid={pid}"));
            if !spec.follow {
                args.push("-t".into());
                args.push(spec.lines.to_string());
            }
        }

        let mut child = Command::new(&self.program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot read logs from Waydroid")?;

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill();
                bail!("could not capture Waydroid's log output");
            }
        };

        // With no pid we fall back to matching the package name in the text.
        // It catches the common cases — the tag, the process name, stack
        // traces — without needing the app to be running first.
        let needle = spec
            .pid
            .is_none()
            .then(|| spec.package.to_ascii_lowercase());
        let mut ring: Vec<String> = Vec::new();
        let mut printed_any = false;

        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                // A broken pipe here means the user walked away; that is fine.
                Err(_) => break,
            };
            let keep = match &needle {
                None => true,
                Some(needle) => line.to_ascii_lowercase().contains(needle),
            };
            if !keep {
                continue;
            }
            if spec.follow || spec.pid.is_some() {
                println!("{line}");
                printed_any = true;
            } else {
                // Not following: keep only the last `lines` matches.
                ring.push(line);
                if ring.len() > spec.lines {
                    ring.remove(0);
                }
            }
        }

        for line in &ring {
            println!("{line}");
            printed_any = true;
        }

        let status = child.wait().context("waiting for logcat to finish")?;
        let mut stderr_text = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            use std::io::Read;
            let _ = stderr.read_to_string(&mut stderr_text);
        }

        if !status.success() && !printed_any {
            let message = stderr_text.trim();
            if looks_like_permission_problem(message) {
                bail!(
                    "could not read the Android log:\n{}\n\n\
                     `waydroid shell` needs more privilege here. Try:\n  \
                     sudo cookie crumbs {}",
                    cmd::indent(message),
                    spec.package
                );
            }
            if !message.is_empty() {
                bail!("could not read the Android log:\n{}", cmd::indent(message));
            }
        }

        Ok(())
    }

    fn ensure_multi_window(&self) -> Result<()> {
        let current = self.waydroid(&["prop", "get", MULTI_WINDOW_PROP])?;
        if current.ok() && current.stdout.trim().eq_ignore_ascii_case("true") {
            return Ok(());
        }
        let out = self.waydroid(&["prop", "set", MULTI_WINDOW_PROP, "true"])?;
        if out.ok() {
            return Ok(());
        }
        // Not fatal: the app still runs, it just shares the Android screen.
        ui::warn(&format!(
            "could not turn on one-window-per-app mode:\n{}\n\
             The app will still run, but inside Waydroid's full screen.\n\
             You can set it by hand with:\n  waydroid prop set {MULTI_WINDOW_PROP} true",
            cmd::indent(&out.message())
        ));
        Ok(())
    }

    fn window_class(&self, package: &str) -> Option<String> {
        // In multi-window mode Waydroid gives each app's surface an app id of
        // `waydroid.<package>`, which is what its own generated launchers use.
        Some(format!("waydroid.{package}"))
    }

    fn arm_translation(&self) -> Result<Option<ArmTranslation>> {
        if system::host_is_arm() {
            return Ok(Some(ArmTranslation {
                available: true,
                implementation: Some("native".to_string()),
            }));
        }
        if self.binary().is_none() {
            return Ok(None);
        }

        // `ro.dalvik.vm.isa.arm` names the translator the image is set up to
        // use; it is only set once a translation layer is installed.
        let out = self.waydroid(&["prop", "get", "ro.dalvik.vm.isa.arm"])?;
        if out.ok() {
            let value = out.stdout.trim();
            if !value.is_empty() {
                return Ok(Some(ArmTranslation {
                    available: true,
                    implementation: Some(value.to_string()),
                }));
            }
        }

        // Fall back to looking for the libraries on disk, which works even
        // when the session is stopped.
        for (path, name) in [
            (
                "/var/lib/waydroid/overlay/system/lib64/libhoudini.so",
                "libhoudini",
            ),
            (
                "/var/lib/waydroid/overlay/system/lib/libhoudini.so",
                "libhoudini",
            ),
            (
                "/var/lib/waydroid/overlay/system/lib64/libndk_translation.so",
                "ndk-translation",
            ),
        ] {
            if Path::new(path).exists() {
                return Ok(Some(ArmTranslation {
                    available: true,
                    implementation: Some(name.to_string()),
                }));
            }
        }

        Ok(Some(ArmTranslation {
            available: false,
            implementation: None,
        }))
    }
}

/// Pull the value out of a `Key:\tValue` status line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (name, value) = line.split_once(':')?;
    if name.trim().eq_ignore_ascii_case(key) {
        Some(value.trim())
    } else {
        None
    }
}

/// `Success: created install session [1234567]` -> `1234567`
fn parse_session_id(text: &str) -> Option<String> {
    let start = text.find('[')?;
    let end = text[start..].find(']')? + start;
    let id = text[start + 1..end].trim();
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        Some(id.to_string())
    } else {
        None
    }
}

/// `pm` and Waydroid both report failures on stdout with a zero exit code.
fn mentions_failure(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("failure")
        || lower.contains("install_failed")
        || lower.contains("error:")
        || lower.contains("not found")
}

fn looks_like_permission_problem(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("permission denied")
        || lower.contains("operation not permitted")
        || lower.contains("must be root")
        || lower.contains("are you root")
}

/// Parse `waydroid app list`, which prints blocks like:
///
/// ```text
/// Name: Settings
/// packageName: com.android.settings
/// categories: android.intent.category.LAUNCHER
/// ```
fn parse_app_list(text: &str) -> Vec<InstalledApp> {
    let mut apps = Vec::new();
    let mut name: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = field(line, "Name") {
            name = Some(value.to_string());
        } else if let Some(value) = field(line, "packageName") {
            if !value.is_empty() {
                apps.push(InstalledApp {
                    package: value.to_string(),
                    name: name.take().filter(|n| !n.is_empty()),
                });
            }
        }
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_status_fields() {
        let text = "Session:\tRUNNING\nContainer:\tRUNNING\nVendor type:\tMAINLINE\nIP address:\t192.168.240.112\n";
        assert_eq!(
            text.lines().find_map(|l| field(l, "Session")),
            Some("RUNNING")
        );
        assert_eq!(
            text.lines().find_map(|l| field(l, "Vendor type")),
            Some("MAINLINE")
        );
        assert_eq!(
            text.lines().find_map(|l| field(l, "IP address")),
            Some("192.168.240.112")
        );
        assert_eq!(text.lines().find_map(|l| field(l, "Nothing")), None);
    }

    #[test]
    fn parses_an_app_list() {
        let text = "Name: Settings\npackageName: com.android.settings\ncategories: android.intent.category.LAUNCHER\n\n\
                    Name: Signal\npackageName: org.thoughtcrime.securesms\n";
        let apps = parse_app_list(text);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].package, "com.android.settings");
        assert_eq!(apps[0].name.as_deref(), Some("Settings"));
        assert_eq!(apps[1].package, "org.thoughtcrime.securesms");
        assert_eq!(apps[1].name.as_deref(), Some("Signal"));
    }

    #[test]
    fn app_list_survives_a_missing_name() {
        let apps = parse_app_list("packageName: com.example.bare\n");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, None);
    }

    #[test]
    fn empty_app_list_is_empty() {
        assert!(parse_app_list("").is_empty());
        assert!(parse_app_list("\n\n  \n").is_empty());
    }

    #[test]
    fn extracts_an_install_session_id() {
        assert_eq!(
            parse_session_id("Success: created install session [874512399]").as_deref(),
            Some("874512399")
        );
        assert_eq!(
            parse_session_id("Success: created install session").as_deref(),
            None
        );
        assert_eq!(parse_session_id("[not-a-number]").as_deref(), None);
        assert_eq!(parse_session_id("").as_deref(), None);
    }

    #[test]
    fn spots_failures_reported_on_stdout() {
        assert!(mentions_failure(
            "Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE]"
        ));
        assert!(mentions_failure("Error: package not found"));
        assert!(!mentions_failure("Success"));
        assert!(!mentions_failure(""));
    }

    #[test]
    fn spots_permission_problems() {
        assert!(looks_like_permission_problem("bash: Permission denied"));
        assert!(looks_like_permission_problem("You must be root"));
        assert!(!looks_like_permission_problem("No such file or directory"));
    }
}
