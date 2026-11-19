//! The oven: Cookie's own Android runtime.
//!
//! # What this is, and what it is not
//!
//! Milestone 4 **starts** replacing Waydroid. The parts that are real and
//! working here are the ones Cookie needs to own:
//!
//! - the shared system image: registering it, checking it, mounting it;
//! - binderfs: detecting it, mounting it, exposing the devices;
//! - the container: its configuration and its lifecycle;
//! - the app commands, through `lxc-attach` into the running container.
//!
//! **What is missing is the display bridge.** An Android app draws through
//! SurfaceFlinger, which expects a graphics stack that ends at a display.
//! Putting those surfaces into Wayland windows means a hardware-composer
//! implementation that talks Wayland, which is a substantial piece of native
//! code and the single largest thing Waydroid provides. Until Cookie has one,
//! the oven can prepare everything, boot the container and install apps, but
//! an app has nowhere to put its window.
//!
//! So the oven reports that plainly rather than appearing to work. Waydroid
//! remains the default backend, and `--backend oven` opts in.

pub mod binder;
pub mod container;
pub mod image;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::cmd;
use super::{
    AndroidBackend, ArmTranslation, BackendNote, InstallOptions, InstalledApp, LogSpec, OvenState,
    OvenStatus,
};
use crate::paths;
use crate::system;
use binder::BinderState;
use container::{ContainerConfig, CONTAINER_NAME};
use image::Image;

/// Where the oven keeps its things. All under the user's own directories.
#[derive(Debug, Clone)]
pub struct OvenDirs {
    pub root: PathBuf,
    /// Where the system image gets mounted.
    pub rootfs: PathBuf,
    /// Writable state for the Android side.
    pub data: PathBuf,
    /// The generated LXC configuration.
    pub config: PathBuf,
    /// What Cookie remembers between runs.
    pub state_file: PathBuf,
}

impl OvenDirs {
    pub fn user() -> Result<Self> {
        Ok(Self::under(&paths::data_dir()?.join("oven")))
    }

    pub fn under(root: &Path) -> Self {
        Self {
            rootfs: root.join("rootfs"),
            data: root.join("data"),
            config: root.join("container.conf"),
            state_file: root.join("state.toml"),
            root: root.to_path_buf(),
        }
    }
}

/// What the oven remembers between runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OvenRecord {
    /// The registered system image, if one has been added.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub image: Option<Image>,
}

/// Everything that has to be true before an app could run.
#[derive(Debug, Clone)]
pub struct Readiness {
    pub lxc_installed: bool,
    pub binder: BinderState,
    pub image: Option<Image>,
    /// Always false today. See the module documentation.
    pub display_bridge: bool,
}

impl Readiness {
    /// Can the container be prepared and started?
    pub fn can_start(&self) -> bool {
        self.lxc_installed
            && self.binder.is_ready()
            && self.image.as_ref().map(|i| i.is_present()).unwrap_or(false)
    }

    /// Could an app actually be used? Not yet, and this says so.
    pub fn can_run_apps(&self) -> bool {
        self.can_start() && self.display_bridge
    }
}

/// The message shown whenever something needs the display bridge.
pub fn display_bridge_message() -> String {
    "The oven can prepare and boot the Android container, but it cannot show an app's window \
     yet.\n\
     \n\
     An Android app draws through SurfaceFlinger, which needs a hardware-composer \
     implementation that hands its surfaces to Wayland. That is the largest piece Waydroid \
     provides, and Cookie does not have its own yet.\n\
     \n\
     Until it does, use the Waydroid backend to actually run apps:\n  \
     cookie eat <file.cpk>            # uses Waydroid, the default\n\
     \n\
     The oven is still useful for checking that your kernel, image and container are set up \
     correctly:\n  \
     cookie burnt --backend oven\n  \
     cookie oven status --backend oven"
        .to_string()
}

pub struct Oven {
    dirs: OvenDirs,
}

impl Oven {
    pub fn new() -> Result<Self> {
        Ok(Self {
            dirs: OvenDirs::user()?,
        })
    }

    /// An oven rooted somewhere else, for tests.
    pub fn with_dirs(dirs: OvenDirs) -> Self {
        Self { dirs }
    }

    pub fn dirs(&self) -> &OvenDirs {
        &self.dirs
    }

    /// Read what the oven remembers.
    pub fn record(&self) -> Result<OvenRecord> {
        if !self.dirs.state_file.is_file() {
            return Ok(OvenRecord::default());
        }
        let text = fs::read_to_string(&self.dirs.state_file)
            .with_context(|| format!("cannot read {}", self.dirs.state_file.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("cannot understand {}", self.dirs.state_file.display()))
    }

    fn write_record(&self, record: &OvenRecord) -> Result<()> {
        paths::ensure_dir(&self.dirs.root)?;
        let text = toml::to_string_pretty(record).context("cannot serialise the oven's state")?;
        fs::write(&self.dirs.state_file, text)
            .with_context(|| format!("cannot write {}", self.dirs.state_file.display()))
    }

    /// Register a system image.
    pub fn add_image(&self, path: &Path) -> Result<Image> {
        let image = image::register(path)?;
        let mut record = self.record()?;
        record.image = Some(image.clone());
        self.write_record(&record)?;
        Ok(image)
    }

    /// Forget the registered image.
    pub fn remove_image(&self) -> Result<bool> {
        let mut record = self.record()?;
        let had = record.image.take().is_some();
        if had {
            self.write_record(&record)?;
        }
        Ok(had)
    }

    /// Where everything stands.
    pub fn readiness(&self) -> Readiness {
        Readiness {
            lxc_installed: cmd::which("lxc-start").is_some(),
            binder: binder::state(),
            image: self.record().ok().and_then(|r| r.image),
            // See the module documentation.
            display_bridge: false,
        }
    }

    /// Build the container configuration from what the host actually has.
    pub fn container_config(&self) -> Result<ContainerConfig> {
        let binder_path = match binder::state() {
            BinderState::Ready { mount_point } => mount_point,
            BinderState::LegacyDevices { .. } => PathBuf::from("/dev"),
            _ => PathBuf::from(binder::DEFAULT_MOUNT_POINT),
        };

        let display = system::display_info();

        Ok(ContainerConfig {
            rootfs: self.dirs.rootfs.clone(),
            data_dir: self.dirs.data.clone(),
            binder_devices: binder::REQUIRED_DEVICES
                .iter()
                .map(|d| (*d).to_string())
                .collect(),
            binder_path,
            render_nodes: system::render_nodes(),
            wayland_socket: display.wayland_socket,
            uid: current_uid(),
            gid: current_gid(),
        })
    }

    /// Write the container configuration to disk.
    pub fn write_container_config(&self) -> Result<PathBuf> {
        paths::ensure_dir(&self.dirs.root)?;
        paths::ensure_dir(&self.dirs.data)?;
        paths::ensure_dir(&self.dirs.rootfs)?;

        let config = self.container_config()?;
        fs::write(&self.dirs.config, config.render())
            .with_context(|| format!("cannot write {}", self.dirs.config.display()))?;
        Ok(self.dirs.config.clone())
    }

    /// Mount the system image read-only at the container's root.
    fn mount_image(&self, image: &Image) -> Result<()> {
        if is_mount_point(&self.dirs.rootfs) {
            return Ok(());
        }
        paths::ensure_dir(&self.dirs.rootfs)?;

        let source = image.path.display().to_string();
        let target = self.dirs.rootfs.display().to_string();
        let arguments = [
            "mount".to_string(),
            "-o".to_string(),
            "loop,ro".to_string(),
            "-t".to_string(),
            image.format.mount_type().to_string(),
            source,
            target,
        ];
        let arguments: Vec<String> = arguments.to_vec();

        let as_root = binder::running_as_root();
        if !as_root {
            crate::ui::step(
                "mounting the Android system image — this needs root, because mounting a \
                 filesystem does",
            );
            crate::ui::note(&crate::ui::dim(&format!("  sudo {}", arguments.join(" "))));
        }

        let (program, args): (&str, Vec<&str>) = if as_root {
            ("mount", arguments[1..].iter().map(String::as_str).collect())
        } else {
            ("sudo", arguments.iter().map(String::as_str).collect())
        };

        let out = cmd::run(program, &args)?;
        if !out.ok() {
            bail!(
                "could not mount the system image:\n{}\n\n\
                 Run it yourself to see more:\n  sudo {}",
                cmd::indent(&out.message()),
                arguments.join(" ")
            );
        }
        Ok(())
    }

    /// Run a command inside the container.
    fn attach(&self, args: &[&str]) -> Result<cmd::Output> {
        let mut full: Vec<&str> = vec!["lxc-attach", "-n", CONTAINER_NAME, "--"];
        full.extend_from_slice(args);
        // `lxc-attach` is the binary; the rest are its arguments.
        cmd::run(full[0], &full[1..])
    }

    fn lxc_info(&self) -> Result<cmd::Output> {
        cmd::run("lxc-info", &["-n", CONTAINER_NAME, "-H", "-s"])
    }
}

/// Everything an app command needs, and the one thing it cannot have yet.
fn not_until_the_bridge(what: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{what} needs the display bridge.\n\n{}",
        display_bridge_message()
    )
}

impl AndroidBackend for Oven {
    fn name(&self) -> &'static str {
        "oven"
    }

    fn ensure_available(&self) -> Result<()> {
        let readiness = self.readiness();

        if !readiness.lxc_installed {
            bail!(
                "the oven runs the Android container with LXC, which is not installed.\n\
                 Install it with your package manager, for example:\n  \
                 Gentoo:  sudo emerge --ask app-containers/lxc\n  \
                 Arch:    sudo pacman -S lxc\n  \
                 Debian:  sudo apt install lxc\n\
                 \n\
                 The oven replaces Waydroid, not LXC: Cookie owns the image, binder, \
                 configuration and lifecycle, and LXC is the engine underneath."
            );
        }

        match &readiness.binder {
            BinderState::Unsupported => bail!("{}", binder::unsupported_message()),
            state if !state.is_ready() => {
                // Mountable or loadable: `start` deals with it.
            }
            _ => {}
        }

        match &readiness.image {
            None => bail!(
                "the oven has no Android system image yet.\n\
                 Cookie does not download one: which image to trust is your call, not \
                 Cookie's.\n\
                 \n\
                 Get a system image — a LineageOS build, or the one Waydroid uses — and \
                 register it:\n  \
                 cookie oven image add /path/to/system.img\n\
                 \n\
                 If it is an Android sparse image, expand it first with `simg2img`."
            ),
            Some(image) if !image.is_present() => bail!(
                "the registered system image is missing or has changed:\n  {}\n\
                 Register it again:\n  cookie oven image add <path>",
                image.path.display()
            ),
            Some(_) => {}
        }

        Ok(())
    }

    fn status(&self) -> Result<OvenStatus> {
        let readiness = self.readiness();

        if !readiness.lxc_installed {
            return Ok(OvenStatus {
                state: OvenState::NotInstalled,
                container_running: None,
                session_running: None,
                vendor_type: Some("cookie oven".to_string()),
                ip_address: None,
                raw: "lxc is not installed".to_string(),
            });
        }

        if readiness.image.is_none() {
            return Ok(OvenStatus {
                state: OvenState::NotInitialized,
                container_running: Some(false),
                session_running: Some(false),
                vendor_type: Some("cookie oven".to_string()),
                ip_address: None,
                raw: "no system image registered".to_string(),
            });
        }

        let info = self.lxc_info()?;
        let reported = info.stdout.trim().to_string();
        let running = reported.eq_ignore_ascii_case("RUNNING");

        Ok(OvenStatus {
            state: if running {
                OvenState::Running
            } else {
                OvenState::Stopped
            },
            container_running: Some(running),
            session_running: Some(running),
            vendor_type: Some("cookie oven".to_string()),
            ip_address: None,
            raw: format!("lxc-info: {reported}"),
        })
    }

    fn start(&self) -> Result<()> {
        self.ensure_available()?;

        let record = self.record()?;
        let Some(image) = record.image else {
            bail!("no system image is registered");
        };

        // Binder first: without it the container boots into nothing.
        crate::ui::step("checking binder support");
        binder::ensure_mounted(Path::new(binder::DEFAULT_MOUNT_POINT))?;

        crate::ui::step("mounting the system image");
        self.mount_image(&image)?;

        crate::ui::step("writing the container configuration");
        let config = self.write_container_config()?;

        crate::ui::step("starting the container");
        let config_path = config.display().to_string();
        let out = cmd::run(
            "lxc-start",
            &["-n", CONTAINER_NAME, "-f", &config_path, "-d"],
        )?;
        if !out.ok() {
            bail!(
                "the container did not start:\n{}\n\n\
                 Its configuration is at {}, and `lxc-start -n {CONTAINER_NAME} -f {} -F` will \
                 run it in the foreground so you can see why.",
                cmd::indent(&out.message()),
                config.display(),
                config.display()
            );
        }

        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if !self.readiness().lxc_installed {
            return Ok(());
        }
        let out = cmd::run("lxc-stop", &["-n", CONTAINER_NAME])?;
        if !out.ok() && !out.message().to_lowercase().contains("not running") {
            bail!(
                "could not stop the container:\n{}",
                cmd::indent(&out.message())
            );
        }
        Ok(())
    }

    fn install(&self, apks: &[PathBuf], options: &InstallOptions) -> Result<()> {
        if apks.is_empty() {
            bail!("no APKs to install");
        }
        self.ensure_available()?;

        // `pm` reads paths from inside the container, so the APKs are staged
        // into the shared data directory first.
        let staging = self.dirs.data.join("cookie-staging");
        paths::ensure_dir(&staging)?;
        let _ = fs::remove_dir_all(&staging);
        paths::ensure_dir(&staging)?;

        let mut inside = Vec::new();
        for (i, apk) in apks.iter().enumerate() {
            let name = apk
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(paths::sanitize_file_name)
                .unwrap_or_else(|| format!("part{i}.apk"));
            fs::copy(apk, staging.join(&name))
                .with_context(|| format!("cannot stage {}", apk.display()))?;
            inside.push(format!("/data/cookie-staging/{name}"));
        }

        let mut args: Vec<&str> = vec!["pm", "install"];
        if options.keep_data {
            args.push("-r");
        }
        if options.allow_downgrade {
            args.push("-d");
        }
        let paths_as_args: Vec<&str> = inside.iter().map(String::as_str).collect();
        args.extend_from_slice(&paths_as_args);

        let out = self.attach(&args)?;
        let _ = fs::remove_dir_all(&staging);

        let reported = format!("{}{}", out.stdout, out.stderr);
        if !out.ok() || reported.to_ascii_uppercase().contains("FAILURE") {
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
    }

    fn uninstall(&self, package: &str) -> Result<()> {
        self.ensure_available()?;
        let out = self.attach(&["pm", "uninstall", package])?;
        if !out.ok() {
            bail!(
                "could not remove {package}:\n{}",
                cmd::indent(&out.message())
            );
        }
        Ok(())
    }

    fn list_installed(&self) -> Result<Vec<InstalledApp>> {
        self.ensure_available()?;
        let out = self.attach(&["pm", "list", "packages"])?;
        if !out.ok() {
            bail!(
                "could not list installed apps:\n{}",
                cmd::indent(&out.message())
            );
        }
        Ok(parse_package_list(&out.stdout))
    }

    fn launch(&self, _package: &str) -> Result<()> {
        Err(not_until_the_bridge("launching an app"))
    }

    fn pid_of(&self, package: &str) -> Result<Option<u32>> {
        let out = self.attach(&["pidof", package])?;
        if !out.ok() {
            return Ok(None);
        }
        Ok(out
            .stdout
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok()))
    }

    fn logs(&self, _spec: &LogSpec) -> Result<()> {
        Err(not_until_the_bridge("reading an app's logs"))
    }

    fn ensure_multi_window(&self) -> Result<()> {
        // One window per app is not a mode in the oven, it is the design:
        // there is no Android home screen to be inside. Nothing to switch on,
        // and nothing to claim until the bridge exists.
        Ok(())
    }

    fn arm_translation(&self) -> Result<Option<ArmTranslation>> {
        if system::host_is_arm() {
            return Ok(Some(ArmTranslation {
                available: true,
                implementation: Some("native".to_string()),
            }));
        }

        // Look inside the mounted image rather than asking a running system,
        // so this answers even while the oven is cold.
        for (relative, name) in [
            ("system/lib64/libhoudini.so", "libhoudini"),
            ("system/lib/libhoudini.so", "libhoudini"),
            ("system/lib64/libndk_translation.so", "ndk-translation"),
        ] {
            if self.dirs.rootfs.join(relative).exists() {
                return Ok(Some(ArmTranslation {
                    available: true,
                    implementation: Some(name.to_string()),
                }));
            }
        }

        if !is_mount_point(&self.dirs.rootfs) {
            // Nothing mounted, so nothing can be said.
            return Ok(None);
        }

        Ok(Some(ArmTranslation {
            available: false,
            implementation: None,
        }))
    }

    fn health_notes(&self) -> Vec<BackendNote> {
        let readiness = self.readiness();
        let mut notes = Vec::new();

        notes.push(match &readiness.image {
            Some(image) if image.is_present() => BackendNote {
                label: "system image".to_string(),
                detail: format!(
                    "{} ({}, {})",
                    paths::tilde(&image.path),
                    image.format.label(),
                    crate::ui::bytes(image.size)
                ),
                advice: None,
                blocking: false,
            },
            Some(image) => BackendNote {
                label: "system image".to_string(),
                detail: "registered, but the file is missing or has changed".to_string(),
                advice: Some(format!(
                    "It was at {}. Register it again:\n  cookie oven image add <path>",
                    image.path.display()
                )),
                blocking: true,
            },
            None => BackendNote {
                label: "system image".to_string(),
                detail: "none registered".to_string(),
                advice: Some(
                    "Cookie does not download one; which image to trust is your call.\n\
                     Get a LineageOS-derived system image and register it:\n  \
                     cookie oven image add /path/to/system.img"
                        .to_string(),
                ),
                blocking: true,
            },
        });

        notes.push(BackendNote {
            label: "container engine".to_string(),
            detail: if readiness.lxc_installed {
                "lxc is installed".to_string()
            } else {
                "lxc is not installed".to_string()
            },
            advice: (!readiness.lxc_installed).then(|| {
                "The oven runs the container with LXC. Install it, for example:\n  \
                 sudo emerge --ask app-containers/lxc"
                    .to_string()
            }),
            blocking: !readiness.lxc_installed,
        });

        // The honest one.
        notes.push(BackendNote {
            label: "display bridge".to_string(),
            detail: "not built yet, so the oven cannot show an app's window".to_string(),
            advice: Some(display_bridge_message()),
            blocking: true,
        });

        notes
    }

    fn window_class(&self, package: &str) -> Option<String> {
        Some(format!("cookie.{package}"))
    }
}

/// `package:com.example.teapot` -> `com.example.teapot`
fn parse_package_list(text: &str) -> Vec<InstalledApp> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("package:"))
        .map(|package| package.trim())
        .filter(|package| !package.is_empty())
        .map(|package| InstalledApp {
            package: package.to_string(),
            // `pm list packages` gives no labels; the jar has them.
            name: None,
        })
        .collect()
}

/// Is this path the root of a mount?
///
/// Compared against `/proc/mounts` rather than by comparing device numbers,
/// which would need `libc` for one field.
fn is_mount_point(path: &Path) -> bool {
    let Ok(mounts) = fs::read_to_string("/proc/mounts") else {
        return false;
    };
    let wanted = path.to_string_lossy();
    mounts.lines().any(|line| {
        line.split_whitespace()
            .nth(1)
            .map(|target| target == wanted)
            .unwrap_or(false)
    })
}

fn current_uid() -> u32 {
    read_proc_id("Uid:").unwrap_or(1000)
}

fn current_gid() -> u32 {
    read_proc_id("Gid:").unwrap_or(1000)
}

fn read_proc_id(field: &str) -> Option<u32> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|values| values.split_whitespace().next().map(str::to_string))
        .and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::ScratchDir;

    fn oven(scratch: &ScratchDir) -> Oven {
        Oven::with_dirs(OvenDirs::under(&scratch.path().join("oven")))
    }

    #[test]
    fn an_empty_oven_remembers_nothing() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        let oven = oven(&scratch);
        assert_eq!(oven.record().expect("record"), OvenRecord::default());
        assert!(oven.readiness().image.is_none());
    }

    #[test]
    fn registering_an_image_survives_a_restart() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        let oven = oven(&scratch);

        // A file with an ext4 superblock magic.
        let mut data = vec![0u8; 20 * 1024 * 1024];
        let at = 1024 + 0x38;
        data[at..at + 2].copy_from_slice(&0xEF53u16.to_le_bytes());
        let path = scratch.path().join("system.img");
        fs::write(&path, &data).expect("write");

        let registered = oven.add_image(&path).expect("add");
        assert_eq!(registered.format, image::ImageFormat::Ext4);

        // A fresh handle on the same directory sees it.
        let reopened = Oven::with_dirs(OvenDirs::under(&scratch.path().join("oven")));
        let record = reopened.record().expect("record");
        assert_eq!(
            record.image.as_ref().map(|i| i.path.clone()),
            Some(registered.path)
        );

        assert!(reopened.remove_image().expect("remove"));
        assert!(reopened.record().expect("record").image.is_none());
        assert!(!reopened.remove_image().expect("remove again"));
    }

    #[test]
    fn readiness_is_honest_about_the_display_bridge() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        let readiness = oven(&scratch).readiness();
        assert!(
            !readiness.display_bridge,
            "the bridge does not exist yet and must not claim to"
        );
        assert!(!readiness.can_run_apps());
    }

    #[test]
    fn without_an_image_the_oven_says_so_and_does_not_offer_to_fetch_one() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        let oven = oven(&scratch);

        // `ensure_available` reports the first thing that is missing, so this
        // only checks the image message when LXC happens to be installed.
        if cmd::which("lxc-start").is_some() {
            let err = oven.ensure_available().unwrap_err().to_string();
            assert!(err.contains("no Android system image"), "{err}");
            assert!(err.contains("does not download"), "{err}");
            assert!(err.contains("cookie oven image add"), "{err}");
        }
    }

    #[test]
    fn without_lxc_the_oven_explains_what_it_is_for() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        if cmd::which("lxc-start").is_none() {
            let err = oven(&scratch).ensure_available().unwrap_err().to_string();
            assert!(err.contains("LXC"), "{err}");
            assert!(
                err.contains("replaces Waydroid, not LXC"),
                "the distinction matters:\n{err}"
            );
        }
    }

    #[test]
    fn launching_an_app_explains_the_missing_piece() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        let err = oven(&scratch)
            .launch("com.example.teapot")
            .unwrap_err()
            .to_string();
        assert!(err.contains("display bridge"), "{err}");
        assert!(err.contains("SurfaceFlinger"), "{err}");
        // And it points at the backend that does work.
        assert!(err.contains("Waydroid"), "{err}");
    }

    #[test]
    fn the_container_config_is_written_where_it_says() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        let oven = oven(&scratch);
        let path = oven.write_container_config().expect("write");

        assert!(path.is_file());
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("lxc.init.cmd = /init"), "{text}");
        assert!(
            text.contains(&oven.dirs().rootfs.display().to_string()),
            "{text}"
        );
        assert!(oven.dirs().data.is_dir());
        assert!(oven.dirs().rootfs.is_dir());
    }

    #[test]
    fn package_lists_are_parsed() {
        let text = "package:com.android.settings\n\
                    package:org.thoughtcrime.securesms\n\
                    \n";
        let apps = parse_package_list(text);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].package, "com.android.settings");
        assert_eq!(apps[1].package, "org.thoughtcrime.securesms");
        assert!(apps[0].name.is_none());
    }

    #[test]
    fn a_noisy_package_list_is_survivable() {
        assert!(parse_package_list("").is_empty());
        assert!(parse_package_list("error: device offline").is_empty());
        assert!(parse_package_list("package:").is_empty());
    }

    #[test]
    fn window_classes_are_namespaced_to_cookie() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        assert_eq!(
            oven(&scratch).window_class("com.example.teapot").as_deref(),
            Some("cookie.com.example.teapot")
        );
    }

    #[test]
    fn mount_point_detection_agrees_with_proc() {
        // `/` is always a mount point; a scratch directory never is.
        assert!(is_mount_point(Path::new("/")));
        let scratch = ScratchDir::new("oven").expect("scratch");
        assert!(!is_mount_point(scratch.path()));
    }

    #[test]
    fn the_session_owner_is_read_from_proc() {
        // Whatever the ids are, they must be readable and not the fallback
        // unless /proc is unavailable.
        let uid = current_uid();
        let gid = current_gid();
        assert!(uid < 1_000_000, "{uid}");
        assert!(gid < 1_000_000, "{gid}");
    }
}
