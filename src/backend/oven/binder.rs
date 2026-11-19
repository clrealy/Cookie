//! binderfs: the kernel side of Android IPC.
//!
//! Android processes talk to each other through the binder driver, which has
//! been in the mainline Linux kernel for years. **Cookie never builds or
//! loads a kernel module.** If the kernel was not built with binder support,
//! the only honest answer is which options to turn on, and that is what this
//! gives.
//!
//! Mounting binderfs does need root. Cookie asks for it explicitly, says what
//! for, and runs exactly one command — it does not keep a privileged helper
//! around.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::backend::cmd;
use crate::system::{self, BinderSupport};

/// Where Cookie mounts binderfs when it has to mount it itself.
///
/// The same path Waydroid and the kernel documentation use, so a system that
/// already has one mounted is found rather than duplicated.
pub const DEFAULT_MOUNT_POINT: &str = "/dev/binderfs";

/// Binder devices an Android container expects to find.
pub const REQUIRED_DEVICES: &[&str] = &["binder", "hwbinder", "vndbinder"];

/// What the kernel offers, and what still has to happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinderState {
    /// Mounted and ready.
    Ready { mount_point: PathBuf },
    /// The kernel has binderfs, but nothing is mounted yet.
    NeedsMount,
    /// A binder module exists but has not been loaded.
    NeedsModule,
    /// Legacy, pre-binderfs devices. Usable, nothing to mount.
    LegacyDevices { devices: Vec<PathBuf> },
    /// The kernel cannot do this at all.
    Unsupported,
}

impl BinderState {
    pub fn is_ready(&self) -> bool {
        matches!(
            self,
            BinderState::Ready { .. } | BinderState::LegacyDevices { .. }
        )
    }
}

/// Work out where things stand. Read-only; needs no privileges.
pub fn state() -> BinderState {
    let support = system::binder_support();
    state_from(&support)
}

/// Split out so the mapping can be tested without a particular kernel.
fn state_from(support: &BinderSupport) -> BinderState {
    if let Some(mount_point) = &support.binderfs_mounted {
        return BinderState::Ready {
            mount_point: mount_point.clone(),
        };
    }
    if !support.devices.is_empty() {
        return BinderState::LegacyDevices {
            devices: support.devices.clone(),
        };
    }
    if support.binderfs_filesystem {
        return BinderState::NeedsMount;
    }
    if support.module_available {
        return BinderState::NeedsModule;
    }
    BinderState::Unsupported
}

/// The advice Cookie gives when a kernel cannot do Android IPC.
///
/// Deliberately specific: "enable binder" helps nobody, and this is the one
/// thing Cookie categorically will not do for the user.
pub fn unsupported_message() -> String {
    let release = system::kernel_release().unwrap_or_else(|| "your kernel".to_string());
    format!(
        "This kernel ({release}) has no Android binder support, and Cookie will not build or \
         load a kernel module for you.\n\
         \n\
         Enable these and rebuild:\n  \
         CONFIG_ANDROID_BINDER_IPC=y   (or =m)\n  \
         CONFIG_ANDROID_BINDERFS=y     (or =m)\n\
         \n\
         On Gentoo they are under:\n  \
         Device Drivers -> Android -> Android Drivers\n\
         \n\
         Some distributions ship binder as a separate module package instead, such as \
         `binder_linux-dkms`."
    )
}

/// The command that mounts binderfs.
///
/// Returned rather than run, so the caller can show it before asking for the
/// privilege to run it. A user is entitled to see what root is being used for.
pub fn mount_command(mount_point: &Path) -> Vec<String> {
    vec![
        "mount".to_string(),
        "-t".to_string(),
        "binder".to_string(),
        "binder".to_string(),
        mount_point.display().to_string(),
    ]
}

/// Are we already root?
pub fn running_as_root() -> bool {
    // Reading the status file avoids pulling in libc for one number.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .and_then(|values| values.split_whitespace().next().map(str::to_string))
        })
        .map(|uid| uid == "0")
        .unwrap_or(false)
}

/// Mount binderfs, asking for root only if we do not already have it.
///
/// Returns the mount point. Mounting an already-mounted binderfs is a no-op.
pub fn ensure_mounted(mount_point: &Path) -> Result<PathBuf> {
    match state() {
        BinderState::Ready { mount_point } => return Ok(mount_point),
        BinderState::LegacyDevices { .. } => {
            // Nothing to mount: the devices are already there.
            return Ok(PathBuf::from("/dev"));
        }
        BinderState::Unsupported => bail!("{}", unsupported_message()),
        BinderState::NeedsModule => bail!(
            "A binder module is installed but not loaded. Cookie does not load kernel modules \
             for you. Load it with:\n  \
             sudo modprobe binder_linux\n\
             To load it at every boot, add `binder_linux` to your module list — on OpenRC that \
             is /etc/modules-load.d/binder.conf.\n\
             Then: cookie oven start"
        ),
        BinderState::NeedsMount => {}
    }

    if !mount_point.is_dir() {
        create_mount_point(mount_point)?;
    }

    let command = mount_command(mount_point);
    let as_root = running_as_root();

    let (program, arguments): (&str, Vec<&str>) = if as_root {
        ("mount", command[1..].iter().map(String::as_str).collect())
    } else {
        crate::ui::step(&format!(
            "mounting binderfs at {} — this needs root, because mounting a filesystem does",
            mount_point.display()
        ));
        crate::ui::note(&crate::ui::dim(&format!("  sudo {}", command.join(" "))));
        ("sudo", command.iter().map(String::as_str).collect())
    };

    let out = cmd::run(program, &arguments)
        .with_context(|| format!("cannot run `{}`", command.join(" ")))?;

    if !out.ok() {
        bail!(
            "could not mount binderfs at {}:\n{}\n\n\
             Run it yourself to see more:\n  sudo {}",
            mount_point.display(),
            cmd::indent(&out.message()),
            command.join(" ")
        );
    }

    // Trust the kernel's own view rather than the exit code.
    match state() {
        BinderState::Ready { mount_point } => Ok(mount_point),
        _ => bail!(
            "the mount command succeeded but no binderfs appeared at {}.\n\
             Check /proc/mounts to see what happened.",
            mount_point.display()
        ),
    }
}

fn create_mount_point(mount_point: &Path) -> Result<()> {
    if running_as_root() {
        std::fs::create_dir_all(mount_point)
            .with_context(|| format!("cannot create {}", mount_point.display()))?;
        return Ok(());
    }
    let path = mount_point.display().to_string();
    let out = cmd::run("sudo", &["mkdir", "-p", &path])
        .with_context(|| format!("cannot create {path}"))?;
    if !out.ok() {
        bail!(
            "could not create the mount point {path}:\n{}",
            cmd::indent(&out.message())
        );
    }
    Ok(())
}

/// Which of the devices an Android container needs are actually present.
pub fn devices_present(mount_point: &Path) -> Vec<String> {
    REQUIRED_DEVICES
        .iter()
        .filter(|name| mount_point.join(name).exists())
        .map(|name| (*name).to_string())
        .collect()
}

/// Which are missing.
pub fn devices_missing(mount_point: &Path) -> Vec<String> {
    let present = devices_present(mount_point);
    REQUIRED_DEVICES
        .iter()
        .filter(|name| !present.iter().any(|p| p == *name))
        .map(|name| (*name).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::ScratchDir;

    fn support(
        mounted: Option<&str>,
        devices: &[&str],
        filesystem: bool,
        module: bool,
    ) -> BinderSupport {
        BinderSupport {
            binderfs_filesystem: filesystem,
            binderfs_mounted: mounted.map(PathBuf::from),
            devices: devices.iter().map(PathBuf::from).collect(),
            config_binder_ipc: None,
            config_binderfs: None,
            module_available: module,
        }
    }

    #[test]
    fn a_mounted_binderfs_is_ready() {
        let state = state_from(&support(Some("/dev/binderfs"), &[], true, true));
        assert_eq!(
            state,
            BinderState::Ready {
                mount_point: PathBuf::from("/dev/binderfs")
            }
        );
        assert!(state.is_ready());
    }

    #[test]
    fn legacy_devices_count_as_ready() {
        let state = state_from(&support(None, &["/dev/binder"], false, false));
        assert!(state.is_ready(), "{state:?}");
        assert!(matches!(state, BinderState::LegacyDevices { .. }));
    }

    #[test]
    fn a_known_filesystem_with_nothing_mounted_needs_mounting() {
        let state = state_from(&support(None, &[], true, false));
        assert_eq!(state, BinderState::NeedsMount);
        assert!(!state.is_ready());
    }

    #[test]
    fn an_unloaded_module_is_distinguished_from_no_support() {
        assert_eq!(
            state_from(&support(None, &[], false, true)),
            BinderState::NeedsModule
        );
        assert_eq!(
            state_from(&support(None, &[], false, false)),
            BinderState::Unsupported
        );
    }

    #[test]
    fn the_unsupported_message_names_both_config_options() {
        let message = unsupported_message();
        assert!(message.contains("CONFIG_ANDROID_BINDER_IPC"), "{message}");
        assert!(message.contains("CONFIG_ANDROID_BINDERFS"), "{message}");
        assert!(
            message.contains("will not build or load a kernel module"),
            "{message}"
        );
    }

    #[test]
    fn the_mount_command_is_what_a_person_would_type() {
        assert_eq!(
            mount_command(Path::new("/dev/binderfs")),
            vec!["mount", "-t", "binder", "binder", "/dev/binderfs"]
        );
    }

    #[test]
    fn missing_devices_are_listed() {
        let scratch = ScratchDir::new("binder").expect("scratch");
        let mount_point = scratch.path();

        assert_eq!(devices_present(mount_point), Vec::<String>::new());
        assert_eq!(devices_missing(mount_point).len(), REQUIRED_DEVICES.len());

        std::fs::write(mount_point.join("binder"), b"").expect("write");
        assert_eq!(devices_present(mount_point), vec!["binder".to_string()]);
        assert_eq!(
            devices_missing(mount_point),
            vec!["hwbinder".to_string(), "vndbinder".to_string()]
        );
    }

    #[test]
    fn root_detection_matches_the_environment() {
        // Whatever the answer, it must not panic and must agree with /proc.
        let detected = running_as_root();
        let from_proc = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("Uid:"))
                    .and_then(|v| v.split_whitespace().next().map(str::to_string))
            });
        if let Some(uid) = from_proc {
            assert_eq!(detected, uid == "0");
        }
    }

    #[test]
    fn mounting_on_a_kernel_without_binder_explains_rather_than_tries() {
        // This container has no binder support, so the real call must refuse
        // with advice instead of shelling out to mount.
        if matches!(state(), BinderState::Unsupported) {
            let err = ensure_mounted(Path::new("/dev/binderfs"))
                .unwrap_err()
                .to_string();
            assert!(err.contains("CONFIG_ANDROID_BINDER_IPC"), "{err}");
        }
    }
}
