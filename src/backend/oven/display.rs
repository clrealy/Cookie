//! Getting app windows onto the desktop.
//!
//! # Where the Wayland client actually lives
//!
//! Not here. An Android app draws through SurfaceFlinger, which composites
//! through a gralloc allocator and a hardware composer — and in a
//! Waydroid-style setup **those HALs are inside the Android image**, compiled
//! into it, and they are the Wayland client. There is no host-side compositor
//! or proxy anywhere in the picture.
//!
//! That makes the host's job specific and finite:
//!
//! 1. Put the desktop's Wayland socket where the in-image HAL looks for it,
//!    which is `/run/xdg/wayland-0` inside the container.
//! 2. Pass the GPU through, so the HAL has something to allocate buffers on.
//! 3. Tell the HAL *which* implementation to load, by writing Android
//!    properties. Only the host knows what hardware is present, so only the
//!    host can decide this.
//! 4. Ask for one window per app rather than a single Android screen.
//!
//! All four are host-side, and all four are what this module does. What
//! Cookie cannot supply is the HAL itself: that comes with the image.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::props::Properties;

/// Where the container sees the desktop's Wayland socket.
///
/// Not an arbitrary choice: it is the path a Waydroid-derived image's HAL
/// looks in, so an image built for Waydroid works unmodified.
pub const CONTAINER_RUNTIME_DIR: &str = "/run/xdg";
pub const CONTAINER_WAYLAND_DISPLAY: &str = "wayland-0";

/// Which graphics implementation the Android side should load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Graphics {
    /// The host is itself Android-based and carries its own HALs, as on a
    /// Halium device. Rare on a desktop, but cheap to detect and wrong to
    /// override when present.
    HostHal { name: String },
    /// Mesa, allocating through GBM on a DRI render node. This is the normal
    /// desktop case.
    Gbm { device: PathBuf },
    /// No usable GPU: software rendering. Slow, but it draws.
    Software,
}

impl Graphics {
    /// The value for `ro.hardware.gralloc`.
    pub fn gralloc(&self) -> String {
        match self {
            Graphics::HostHal { name } => name.clone(),
            Graphics::Gbm { .. } => "gbm".to_string(),
            Graphics::Software => "default".to_string(),
        }
    }

    /// The value for `ro.hardware.egl`.
    pub fn egl(&self) -> String {
        match self {
            Graphics::HostHal { name } => name.clone(),
            Graphics::Gbm { .. } => "mesa".to_string(),
            Graphics::Software => "swiftshader".to_string(),
        }
    }

    /// Will apps be drawn by the GPU?
    pub fn is_accelerated(&self) -> bool {
        !matches!(self, Graphics::Software)
    }

    pub fn describe(&self) -> String {
        match self {
            Graphics::HostHal { name } => format!("the host's own {name} HAL"),
            Graphics::Gbm { device } => {
                format!("mesa on {}", device.display())
            }
            Graphics::Software => "software rendering (no GPU found)".to_string(),
        }
    }
}

/// Work out which graphics implementation suits this host.
///
/// Inputs are passed in rather than looked up so the decision can be tested
/// against every combination without needing the hardware.
pub fn detect(render_nodes: &[PathBuf], hal_dirs: &[PathBuf]) -> Graphics {
    // A host that already has Android gralloc HALs knows better than we do.
    if let Some(name) = find_host_hal(hal_dirs) {
        return Graphics::HostHal { name };
    }

    // The ordinary desktop case: a DRI render node, allocated through GBM.
    if let Some(device) = render_nodes.first() {
        return Graphics::Gbm {
            device: device.clone(),
        };
    }

    Graphics::Software
}

/// Look for `gralloc.<name>.so` in the host's library directories.
fn find_host_hal(hal_dirs: &[PathBuf]) -> Option<String> {
    for dir in hal_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_str()?;
                let middle = name.strip_prefix("gralloc.")?.strip_suffix(".so")?;
                // `gralloc.default.so` is the no-op implementation; finding it
                // tells us nothing.
                (!middle.is_empty() && middle != "default").then(|| middle.to_string())
            })
            .collect();
        names.sort();
        if let Some(name) = names.into_iter().next() {
            return Some(name);
        }
    }
    None
}

/// The library directories worth looking in for host HALs.
pub fn default_hal_dirs() -> Vec<PathBuf> {
    [
        "/system/lib64/hw",
        "/system/lib/hw",
        "/vendor/lib64/hw",
        "/vendor/lib/hw",
    ]
    .iter()
    .map(PathBuf::from)
    .filter(|p| p.is_dir())
    .collect()
}

/// How the display should behave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayConfig {
    pub graphics: Graphics,
    /// `ro.sf.lcd_density`. Zero means "do not set it", letting Android pick.
    pub density: u32,
    /// One window per app, rather than a single Android screen.
    pub multi_window: bool,
}

impl DisplayConfig {
    /// The properties the Android side needs to draw the way we want.
    pub fn properties(&self) -> Result<Properties> {
        let mut props = Properties::new();

        props.set_if_present("ro.hardware.gralloc", &self.graphics.gralloc())?;
        props.set_if_present("ro.hardware.egl", &self.graphics.egl())?;

        if let Graphics::Gbm { device } = &self.graphics {
            // The HAL opens this node directly to allocate buffers.
            props.set("gralloc.gbm.device", &device.display().to_string())?;
        }

        // OpenGL ES 3.2, encoded as Android writes it: major in the high 16
        // bits, minor in the low 16.
        props.set("ro.opengles.version", "196610")?;

        if self.density > 0 {
            props.set("ro.sf.lcd_density", &self.density.to_string())?;
        }

        // One window per app is the entire point of Cookie, and the property
        // that asks for it is read by the *image*. A Waydroid-derived image —
        // which is what anyone actually has — reads the `waydroid`-namespaced
        // name, so that is the one that does the work. The cookie-namespaced
        // one is set alongside it for an image built against Cookie later;
        // an image that does not know a property simply ignores it.
        let value = if self.multi_window { "true" } else { "false" };
        props.set("persist.waydroid.multi_windows", value)?;
        props.set("persist.cookie.multi_windows", value)?;

        Ok(props)
    }
}

/// Where the desktop's Wayland socket is on the host.
pub fn host_wayland_socket() -> Option<PathBuf> {
    crate::system::display_info().wayland_socket
}

/// The container-side path for the Wayland socket.
pub fn container_wayland_socket() -> String {
    format!("{CONTAINER_RUNTIME_DIR}/{CONTAINER_WAYLAND_DISPLAY}")
}

/// What is missing before an app could appear in a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayReadiness {
    Ready {
        graphics: Graphics,
    },
    /// There is no Wayland session to draw into.
    NoWaylandSocket,
}

pub fn readiness(render_nodes: &[PathBuf]) -> DisplayReadiness {
    match host_wayland_socket() {
        Some(_) => DisplayReadiness::Ready {
            graphics: detect(render_nodes, &default_hal_dirs()),
        },
        None => DisplayReadiness::NoWaylandSocket,
    }
}

/// What to tell someone whose session cannot be drawn into.
pub fn no_wayland_message() -> String {
    "Cookie could not find your desktop's Wayland socket, so there is nowhere to put an app's \
     window.\n\
     The Android side draws by connecting to that socket from inside the container, so it has \
     to exist before the oven starts.\n\
     \n\
     Check you are in a Wayland session rather than X11:\n  \
     echo $WAYLAND_DISPLAY $XDG_RUNTIME_DIR\n\
     On KDE Plasma, log out and choose \"Plasma (Wayland)\" at the login screen."
        .to_string()
}

/// Is this path safe to bind-mount as the Wayland socket?
///
/// `WAYLAND_DISPLAY` comes from the environment, and the container config is
/// a file whose lines are whitespace-separated, so a path containing a space
/// or a newline would silently change what gets mounted.
pub fn socket_path_is_safe(path: &Path) -> bool {
    let text = path.to_string_lossy();
    !text.is_empty()
        && !text.contains(char::is_whitespace)
        && !text.contains('\n')
        && path.is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::ScratchDir;

    fn nodes(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_render_node_means_mesa_on_gbm() {
        let graphics = detect(&nodes(&["/dev/dri/renderD128"]), &[]);
        assert_eq!(
            graphics,
            Graphics::Gbm {
                device: PathBuf::from("/dev/dri/renderD128")
            }
        );
        assert_eq!(graphics.gralloc(), "gbm");
        assert_eq!(graphics.egl(), "mesa");
        assert!(graphics.is_accelerated());
    }

    #[test]
    fn the_first_render_node_is_the_one_used() {
        let graphics = detect(&nodes(&["/dev/dri/renderD128", "/dev/dri/renderD129"]), &[]);
        assert_eq!(
            graphics,
            Graphics::Gbm {
                device: PathBuf::from("/dev/dri/renderD128")
            }
        );
    }

    #[test]
    fn no_gpu_falls_back_to_software_rather_than_failing() {
        let graphics = detect(&[], &[]);
        assert_eq!(graphics, Graphics::Software);
        assert_eq!(graphics.gralloc(), "default");
        assert_eq!(graphics.egl(), "swiftshader");
        assert!(!graphics.is_accelerated());
        assert!(graphics.describe().contains("no GPU"));
    }

    #[test]
    fn a_host_hal_wins_over_our_own_choice() {
        let scratch = ScratchDir::new("display").expect("scratch");
        let hw = scratch.path().join("hw");
        std::fs::create_dir_all(&hw).expect("mkdir");
        std::fs::write(hw.join("gralloc.msm8974.so"), b"").expect("write");

        // Even with a render node present, the host's own HAL is used.
        let graphics = detect(&nodes(&["/dev/dri/renderD128"]), &[hw]);
        assert_eq!(
            graphics,
            Graphics::HostHal {
                name: "msm8974".to_string()
            }
        );
        assert_eq!(graphics.gralloc(), "msm8974");
        assert_eq!(graphics.egl(), "msm8974");
    }

    #[test]
    fn the_no_op_hal_is_not_mistaken_for_a_real_one() {
        let scratch = ScratchDir::new("display").expect("scratch");
        let hw = scratch.path().join("hw");
        std::fs::create_dir_all(&hw).expect("mkdir");
        // gralloc.default.so is the stub; finding it tells us nothing.
        std::fs::write(hw.join("gralloc.default.so"), b"").expect("write");

        assert_eq!(detect(&[], &[hw]), Graphics::Software);
    }

    #[test]
    fn unrelated_libraries_are_ignored() {
        let scratch = ScratchDir::new("display").expect("scratch");
        let hw = scratch.path().join("hw");
        std::fs::create_dir_all(&hw).expect("mkdir");
        for name in [
            "libgralloc.so",
            "gralloc.so",
            "hwcomposer.gbm.so",
            "notes.txt",
        ] {
            std::fs::write(hw.join(name), b"").expect("write");
        }
        assert_eq!(detect(&[], &[hw]), Graphics::Software);
    }

    #[test]
    fn a_missing_hal_directory_is_skipped_not_fatal() {
        assert_eq!(detect(&[], &nodes(&["/nope/not/here"])), Graphics::Software);
    }

    #[test]
    fn the_properties_tell_android_what_to_load() {
        let config = DisplayConfig {
            graphics: Graphics::Gbm {
                device: PathBuf::from("/dev/dri/renderD128"),
            },
            density: 240,
            multi_window: true,
        };
        let props = config.properties().expect("properties");

        assert_eq!(props.get("ro.hardware.gralloc"), Some("gbm"));
        assert_eq!(props.get("ro.hardware.egl"), Some("mesa"));
        assert_eq!(props.get("gralloc.gbm.device"), Some("/dev/dri/renderD128"));
        assert_eq!(props.get("ro.sf.lcd_density"), Some("240"));
        assert_eq!(props.get("ro.opengles.version"), Some("196610"));
    }

    /// The image is what reads this, and the images people actually have are
    /// Waydroid's, so the waydroid-namespaced property is the one that works.
    #[test]
    fn multi_window_is_set_under_the_name_the_image_reads() {
        let config = DisplayConfig {
            graphics: Graphics::Software,
            density: 0,
            multi_window: true,
        };
        let props = config.properties().expect("properties");
        assert_eq!(props.get("persist.waydroid.multi_windows"), Some("true"));
        assert_eq!(props.get("persist.cookie.multi_windows"), Some("true"));

        let off = DisplayConfig {
            multi_window: false,
            ..config
        }
        .properties()
        .expect("properties");
        assert_eq!(off.get("persist.waydroid.multi_windows"), Some("false"));
    }

    #[test]
    fn density_zero_is_left_unset_rather_than_written_as_zero() {
        let config = DisplayConfig {
            graphics: Graphics::Software,
            density: 0,
            multi_window: true,
        };
        let props = config.properties().expect("properties");
        assert_eq!(props.get("ro.sf.lcd_density"), None);
    }

    #[test]
    fn software_rendering_names_no_gbm_device() {
        let config = DisplayConfig {
            graphics: Graphics::Software,
            density: 0,
            multi_window: true,
        };
        let props = config.properties().expect("properties");
        assert_eq!(props.get("gralloc.gbm.device"), None);
        assert_eq!(props.get("ro.hardware.egl"), Some("swiftshader"));
    }

    #[test]
    fn the_container_socket_path_is_the_one_the_image_expects() {
        assert_eq!(container_wayland_socket(), "/run/xdg/wayland-0");
    }

    #[test]
    fn a_socket_path_with_whitespace_is_refused() {
        // The container config is whitespace-separated, so this would change
        // which file gets mounted.
        assert!(socket_path_is_safe(Path::new("/run/user/1000/wayland-0")));
        assert!(!socket_path_is_safe(Path::new(
            "/run/user/my display/wayland-0"
        )));
        assert!(!socket_path_is_safe(Path::new("/run/a\nb")));
        assert!(!socket_path_is_safe(Path::new("relative/path")));
        assert!(!socket_path_is_safe(Path::new("")));
    }

    #[test]
    fn a_missing_wayland_session_is_explained() {
        let message = no_wayland_message();
        assert!(message.contains("Wayland"), "{message}");
        assert!(message.contains("WAYLAND_DISPLAY"), "{message}");
    }
}
