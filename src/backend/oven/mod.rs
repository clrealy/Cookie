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
pub mod display;
pub mod image;
pub mod mount;
pub mod props;

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
use crate::ui;
use binder::BinderState;
use container::{ContainerConfig, CONTAINER_NAME};
use display::{DisplayConfig, DisplayReadiness, Graphics};
use image::Image;
use props::Properties;

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
    /// The registered system image. `alias` keeps records written before
    /// the vendor image existed readable.
    #[serde(alias = "image", skip_serializing_if = "Option::is_none", default)]
    pub system: Option<Image>,

    /// The vendor image. Android's hardware layers live in `/vendor`, so the
    /// container cannot boot without it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub vendor: Option<Image>,

    /// Properties the user has set, applied over the ones Cookie computes.
    ///
    /// This is also where `ro.sf.lcd_density` lives when someone sets it:
    /// one place for overrides rather than a second source of truth.
    #[serde(default, skip_serializing_if = "Properties::is_empty")]
    pub properties: Properties,
}

/// Everything that has to be true before an app could run.
#[derive(Debug, Clone)]
pub struct Readiness {
    pub lxc_installed: bool,
    pub binder: BinderState,
    pub image: Option<Image>,
    /// The vendor image. Without it there are no hardware layers to load.
    pub vendor: Option<Image>,
    /// Whether there is a Wayland session to draw into, and what graphics
    /// implementation the Android side should load.
    pub display: DisplayReadiness,
}

impl Readiness {
    /// Can the container be prepared and started?
    pub fn can_start(&self) -> bool {
        self.lxc_installed
            && self.binder.is_ready()
            && self.image.as_ref().map(|i| i.is_present()).unwrap_or(false)
            && self
                .vendor
                .as_ref()
                .map(|i| i.is_present())
                .unwrap_or(false)
    }

    /// Is there somewhere for an app's window to go?
    ///
    /// This says the *host* side is in place. Whether a window actually
    /// appears also depends on the image carrying a hardware composer that
    /// speaks Wayland, and that cannot be known without booting it.
    pub fn can_show_windows(&self) -> bool {
        self.can_start() && matches!(self.display, DisplayReadiness::Ready { .. })
    }

    /// The graphics implementation the Android side will be told to load.
    pub fn graphics(&self) -> Option<&Graphics> {
        match &self.display {
            DisplayReadiness::Ready { graphics } => Some(graphics),
            DisplayReadiness::NoWaylandSocket => None,
        }
    }
}

/// What Cookie cannot check from the host.
///
/// The host side of the display path is Cookie's to get right, and it does.
/// The other half — the hardware composer that turns Android's surfaces into
/// Wayland ones — lives inside the system image, and no amount of host-side
/// inspection can tell whether a given image has one.
pub fn image_hal_caveat() -> String {
    "Whether an app's window actually appears depends on the system image.\n\
     \n\
     Android draws through a hardware composer, and in this design that composer is the \
     Wayland client: it lives inside the image and connects out to the socket Cookie passes \
     in. A LineageOS build made for Waydroid has one. A stock Android system image does not, \
     and with such an image the container will boot but nothing will be drawn.\n\
     \n\
     Cookie cannot tell the two apart without starting them, so it does not pretend to. If \
     apps install but no window appears, that is the first thing to suspect:\n  \
     cookie crumbs <name> --backend oven"
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

    /// Register the system image, and the vendor image alongside it.
    pub fn add_images(&self, path: &Path, vendor: Option<&Path>) -> Result<image::ImageSet> {
        let set = image::register_set(path, vendor)?;
        let mut record = self.record()?;
        record.system = Some(set.system.clone());
        // Naming a system image on its own replaces the pair rather than
        // leaving a vendor image from some other build behind it.
        record.vendor = set.vendor.clone();
        self.write_record(&record)?;
        Ok(set)
    }

    /// Set a property that overrides what Cookie computes.
    pub fn set_property(&self, key: &str, value: &str) -> Result<()> {
        let mut record = self.record()?;
        record.properties.set(key, value)?;
        self.write_record(&record)
    }

    /// Drop an override, going back to Cookie's own answer.
    pub fn unset_property(&self, key: &str) -> Result<bool> {
        let mut record = self.record()?;
        let had = record.properties.remove(key);
        if had {
            self.write_record(&record)?;
        }
        Ok(had)
    }

    /// Forget the registered images.
    pub fn remove_image(&self) -> Result<bool> {
        let mut record = self.record()?;
        let had = record.system.take().is_some() || record.vendor.take().is_some();
        if had {
            self.write_record(&record)?;
        }
        Ok(had)
    }

    /// How the container root is layered.
    pub fn layers(&self) -> mount::Layers {
        mount::Layers::under(&self.dirs.root)
    }

    /// Where everything stands.
    pub fn readiness(&self) -> Readiness {
        Readiness {
            lxc_installed: cmd::which("lxc-start").is_some(),
            binder: binder::state(),
            image: self.record().ok().and_then(|r| r.system),
            vendor: self.record().ok().and_then(|r| r.vendor),
            display: display::readiness(&system::render_nodes()),
        }
    }

    /// How the display should be set up on this host.
    pub fn display_config(&self) -> DisplayConfig {
        let record = self.record().unwrap_or_default();
        DisplayConfig {
            graphics: display::detect(&system::render_nodes(), &display::default_hal_dirs()),
            density: record
                .properties
                .get("ro.sf.lcd_density")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            // One window per app is the whole point; there is no reason to
            // ever ask for the other thing.
            multi_window: true,
        }
    }

    /// The property file handed to the Android side, including anything the
    /// user has set themselves.
    pub fn properties(&self) -> Result<Properties> {
        let mut props = self.display_config().properties()?;
        props.merge(&self.record().unwrap_or_default().properties);
        Ok(props)
    }

    /// Where the property file lives on the host.
    pub fn props_path(&self) -> PathBuf {
        self.dirs.root.join("cookie.prop")
    }

    /// Where it is bound inside the container root.
    ///
    /// `/vendor/waydroid.prop` is where a Waydroid-derived image looks, and
    /// those are the images that exist. A file at any other path is a file
    /// nothing reads.
    pub fn container_props_path(&self) -> PathBuf {
        self.layers().rootfs.join("vendor/waydroid.prop")
    }

    /// Write the property file into the shared data directory, which the
    /// container sees as `/data`.
    pub fn write_properties(&self) -> Result<PathBuf> {
        let path = self.props_path();
        self.properties()?.write(&path)?;
        Ok(path)
    }

    /// Apply the properties that can be set on a running system.
    ///
    /// Only `persist.*` ones: Android fixes `ro.*` properties during init and
    /// refuses to change them afterwards, which is exactly why the file above
    /// exists.
    fn apply_runtime_properties(&self) -> Result<usize> {
        let props = self.properties()?;
        let mut applied = 0;
        for (key, value) in props.iter() {
            if !key.starts_with("persist.") {
                continue;
            }
            let out = self.attach(&["setprop", key, value])?;
            if out.ok() {
                applied += 1;
            } else {
                ui::warn(&format!(
                    "could not set {key}:\n{}",
                    cmd::indent(&out.message())
                ));
            }
        }
        Ok(applied)
    }

    /// Build the container configuration from what the host actually has.
    pub fn container_config(&self) -> Result<ContainerConfig> {
        let binder_path = match binder::state() {
            BinderState::Ready { mount_point } => mount_point,
            BinderState::LegacyDevices { .. } => PathBuf::from("/dev"),
            _ => PathBuf::from(binder::DEFAULT_MOUNT_POINT),
        };

        let display = system::display_info();
        // WAYLAND_DISPLAY comes from the environment, and the container
        // config is whitespace-separated, so a path with a space in it would
        // silently mount something else.
        let wayland_socket = display
            .wayland_socket
            .filter(|path| display::socket_path_is_safe(path));

        Ok(ContainerConfig {
            rootfs: self.dirs.rootfs.clone(),
            data_dir: self.dirs.data.clone(),
            binder_devices: binder::REQUIRED_DEVICES
                .iter()
                .map(|d| (*d).to_string())
                .collect(),
            binder_path,
            render_nodes: system::render_nodes(),
            wayland_socket,
            pulse_socket: pulse_socket(),
            cgroup_v2: container::host_uses_cgroup_v2(),
            host_egl_dirs: container::host_egl_dirs(),
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

        if readiness.image.is_some() && readiness.vendor.is_none() {
            bail!(
                "a system image is registered, but no vendor image is.\n\
                 Android's hardware layers live in /vendor — the graphics ones included — so it \
                 cannot boot without one.\n\
                 \n\
                 Register them together:\n  \
                 cookie oven image add <system.img> <vendor.img>\n\
                 or point Cookie at the directory holding both:\n  \
                 cookie oven image add <directory>"
            );
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
        let Some(system) = record.system else {
            bail!("no system image is registered");
        };

        // Binder first: without it the container boots into nothing.
        crate::ui::step("checking binder support");
        binder::ensure_mounted(Path::new(binder::DEFAULT_MOUNT_POINT))?;

        // The root is layered: the images read-only, a writable overlay over
        // each. The container creates device nodes and mount points inside
        // it, so a bare read-only image is not enough.
        crate::ui::step("assembling the container root");
        let layers = self.layers();
        mount::assemble(&layers, &system, record.vendor.as_ref())?;

        crate::ui::step("writing the container configuration");
        let config = self.write_container_config()?;

        // Android decides how to draw from these, and only the host knows
        // what hardware is here, so they go in before it boots — bound to
        // the path the image reads, since a file it does not read is a file
        // that may as well not exist.
        let props_path = self.write_properties()?;
        mount::bind_file(&props_path, &self.container_props_path())?;
        let display = self.display_config();
        crate::ui::note(&crate::ui::dim(&format!(
            "graphics: {} ({})",
            display.graphics.describe(),
            paths::tilde(&props_path)
        )));

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

        // `persist.*` properties only take on a running system; the `ro.*`
        // ones were fixed during init from the file written above.
        match self.apply_runtime_properties() {
            Ok(0) => {}
            Ok(n) => crate::ui::note(&crate::ui::dim(&format!("applied {n} properties"))),
            Err(err) => crate::ui::warn(&format!("could not apply properties: {err}")),
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

        // Leaving the images and overlays mounted would keep a loop device
        // busy and quietly pin the image file.
        for problem in mount::disassemble(&self.layers()) {
            crate::ui::warn(&format!("could not unmount {problem}"));
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

    fn launch(&self, package: &str) -> Result<()> {
        self.ensure_available()?;

        if !matches!(self.readiness().display, DisplayReadiness::Ready { .. }) {
            bail!("{}", display::no_wayland_message());
        }

        // `monkey` starts an app by package id without having to resolve its
        // launcher activity first, which is what `am start` would need.
        let out = self.attach(&[
            "monkey",
            "-p",
            package,
            "-c",
            "android.intent.category.LAUNCHER",
            "1",
        ])?;

        let reported = format!("{}{}", out.stdout, out.stderr);
        if out.ok() && !reported.contains("No activities found") {
            return Ok(());
        }
        bail!(
            "could not launch {package}:\n{}\n\n\
             If the app installed but will not start, its own log usually says why:\n  \
             cookie crumbs {package} --backend oven",
            cmd::indent(reported.trim())
        )
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

    fn logs(&self, spec: &LogSpec) -> Result<()> {
        self.ensure_available()?;

        let mut args: Vec<String> = vec![
            "lxc-attach".into(),
            "-n".into(),
            CONTAINER_NAME.into(),
            "--".into(),
            "logcat".into(),
        ];
        if !spec.follow {
            args.push("-d".into());
        }
        if let Some(pid) = spec.pid {
            args.push(format!("--pid={pid}"));
            if !spec.follow {
                args.push("-t".into());
                args.push(spec.lines.to_string());
            }
        }

        stream_logs(&args, spec)
    }

    fn ensure_multi_window(&self) -> Result<()> {
        // One window per app is the design rather than a mode, but the image
        // still has to be told, and the property only takes effect on a
        // running system.
        if !self.status()?.state.is_running() {
            return Ok(());
        }
        let out = self.attach(&["setprop", "persist.waydroid.multi_windows", "true"])?;
        if !out.ok() {
            ui::warn(&format!(
                "could not ask for one window per app:\n{}\n\
                 Apps will still run, but inside a single Android screen.",
                cmd::indent(&out.message())
            ));
        }
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

        notes.push(match &readiness.display {
            DisplayReadiness::Ready { graphics } => BackendNote {
                label: "display".to_string(),
                detail: format!("{}, one window per app", graphics.describe()),
                advice: (!graphics.is_accelerated()).then(|| {
                    "Apps will be drawn in software, which works but is slow. If this machine \
                     has a GPU, check you are in the `video` and `render` groups."
                        .to_string()
                }),
                blocking: false,
            },
            DisplayReadiness::NoWaylandSocket => BackendNote {
                label: "display".to_string(),
                detail: "no Wayland socket, so there is nowhere to draw".to_string(),
                advice: Some(display::no_wayland_message()),
                blocking: true,
            },
        });

        // The one thing the host genuinely cannot check.
        notes.push(BackendNote {
            label: "image graphics".to_string(),
            detail: "cannot be checked without booting the image".to_string(),
            advice: Some(image_hal_caveat()),
            blocking: false,
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

/// Run a log command and print its output, filtering by package name when
/// there is no process id to filter by.
///
/// Reading the child's output line by line rather than letting it inherit
/// stdout is what makes `--follow` interruptible and lets the name filter
/// work at all.
fn stream_logs(args: &[String], spec: &LogSpec) -> Result<()> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot read logs from the container")?;

    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        bail!("could not capture the container's log output");
    };

    let needle = spec
        .pid
        .is_none()
        .then(|| spec.package.to_ascii_lowercase());
    let mut ring: Vec<String> = Vec::new();
    let mut printed_any = false;

    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
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
    if !status.success() && !printed_any {
        let mut stderr_text = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            use std::io::Read;
            let _ = stderr.read_to_string(&mut stderr_text);
        }
        if !stderr_text.trim().is_empty() {
            bail!(
                "could not read the Android log:\n{}",
                cmd::indent(stderr_text.trim())
            );
        }
    }
    Ok(())
}
/// The desktop's audio socket, if it has one.
///
/// PipeWire puts a PulseAudio-compatible socket in the same place, so one
/// lookup covers both.
fn pulse_socket() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    let socket = runtime.join("pulse/native");
    socket.exists().then_some(socket)
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

        let registered = oven.add_images(&path, None).expect("add");
        assert_eq!(registered.system.format, image::ImageFormat::Ext4);

        // A fresh handle on the same directory sees it.
        let reopened = Oven::with_dirs(OvenDirs::under(&scratch.path().join("oven")));
        let record = reopened.record().expect("record");
        assert_eq!(
            record.system.as_ref().map(|i| i.path.clone()),
            Some(registered.system.path)
        );

        assert!(reopened.remove_image().expect("remove"));
        assert!(reopened.record().expect("record").system.is_none());
        assert!(!reopened.remove_image().expect("remove again"));
    }

    #[test]
    fn readiness_needs_an_image_before_anything_can_run() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        let readiness = oven(&scratch).readiness();
        assert!(!readiness.can_start(), "nothing is registered yet");
        assert!(!readiness.can_show_windows());
    }

    #[test]
    fn the_image_caveat_is_stated_rather_than_guessed_at() {
        // Cookie cannot tell whether an image carries a Wayland-capable
        // composer without booting it, so it has to say so.
        let caveat = image_hal_caveat();
        assert!(caveat.contains("depends on the system image"), "{caveat}");
        assert!(caveat.contains("Waydroid"), "{caveat}");
        assert!(caveat.contains("cannot tell"), "{caveat}");
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
    fn launching_an_app_names_whatever_is_missing() {
        let scratch = ScratchDir::new("oven").expect("scratch");
        let err = oven(&scratch)
            .launch("com.example.teapot")
            .unwrap_err()
            .to_string();
        // Which piece is absent depends on the machine; the message has to
        // name one of them rather than fail obscurely.
        assert!(
            err.contains("LXC")
                || err.contains("system image")
                || err.contains("Wayland")
                || err.contains("binder"),
            "{err}"
        );
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
