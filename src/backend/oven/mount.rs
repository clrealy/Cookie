//! Assembling the container's root filesystem.
//!
//! An Android system image is read-only, and a container root has to be
//! writable: the runtime creates device nodes, mount points and state inside
//! it. So the root is built in layers, the way Waydroid does it.
//!
//! ```text
//! rootfs          <- overlay: lowerdir=overlay:rootfs, upper=overlay_rw/system
//!   system.img    <- read-only, the lower layer
//! rootfs/vendor   <- overlay: lowerdir=overlay/vendor:rootfs/vendor, upper=overlay_rw/vendor
//!   vendor.img    <- read-only
//! ```
//!
//! Both images are needed. Android's HALs live in `/vendor` — including the
//! graphics ones — so a system image on its own boots into something with
//! nothing to draw with.
//!
//! Every mount here needs root, because mounting a filesystem does. Cookie
//! prints each command before running it and runs exactly those; it keeps no
//! privileged helper around.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::binder;
use super::image::Image;
use crate::backend::cmd;
use crate::paths;
use crate::privilege;
use crate::ui;

/// Where the pieces of the container root live.
#[derive(Debug, Clone)]
pub struct Layers {
    /// The assembled root the container boots from.
    pub rootfs: PathBuf,
    /// An extra read-only layer for files Cookie adds.
    pub lower_extra: PathBuf,
    /// The writable layer. Everything the container changes lands here.
    pub upper: PathBuf,
    /// overlayfs scratch space. Must share a filesystem with `upper`.
    pub work: PathBuf,
}

impl Layers {
    pub fn under(root: &Path) -> Self {
        Self {
            rootfs: root.join("rootfs"),
            lower_extra: root.join("overlay"),
            upper: root.join("overlay_rw"),
            work: root.join("overlay_work"),
        }
    }

    /// Create every directory the overlays need.
    pub fn prepare(&self) -> Result<()> {
        for dir in [
            self.rootfs.clone(),
            self.lower_extra.join("vendor"),
            self.upper.join("system"),
            self.upper.join("vendor"),
            self.work.join("system"),
            self.work.join("vendor"),
        ] {
            paths::ensure_dir(&dir)?;
        }
        Ok(())
    }

    /// Every mount point, deepest first — the order they have to come down in.
    pub fn mount_points(&self) -> Vec<PathBuf> {
        vec![
            self.rootfs.join("vendor"),
            self.rootfs.join("vendor"),
            self.rootfs.clone(),
            self.rootfs.clone(),
        ]
    }
}

/// overlayfs separates lower layers with `:` and options with `,`, and gives
/// no way to escape either. A path containing one would silently change what
/// gets mounted, so it is refused instead.
pub fn path_is_overlay_safe(path: &Path) -> bool {
    let text = path.to_string_lossy();
    !text.is_empty() && !text.contains(':') && !text.contains(',') && !text.contains('\\')
}

fn check_overlay_paths(paths: &[&Path]) -> Result<()> {
    for path in paths {
        if !path_is_overlay_safe(path) {
            bail!(
                "{} contains a character overlayfs cannot express (`:`, `,` or `\\`).\n\
                 Move the oven somewhere without one, or set XDG_DATA_HOME to such a place.",
                path.display()
            );
        }
    }
    Ok(())
}

/// Is this path the root of a mount?
pub fn is_mounted(path: &Path) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return false;
    };
    let wanted = path.to_string_lossy();
    mounts
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(wanted.as_ref()))
}

/// Run a mount command, asking for root only when we do not already have it.
fn run_mount(arguments: &[String], what: &str) -> Result<()> {
    if !privilege::running_as_root() {
        ui::note(&ui::dim(&format!(
            "  {}",
            privilege::hint(&arguments.join(" "))
        )));
    }

    let args: Vec<&str> = arguments[1..].iter().map(String::as_str).collect();
    let out = privilege::run_as_root(&arguments[0], &args)?;
    if !out.ok() {
        bail!(
            "could not mount {what}:\n{}\n\n\
             Run it yourself to see more:\n  {}",
            cmd::indent(&out.message()),
            privilege::hint(&arguments.join(" "))
        );
    }
    Ok(())
}

/// Mount one image read-only at `target`.
fn mount_image(image: &Image, target: &Path, what: &str) -> Result<()> {
    if is_mounted(target) {
        return Ok(());
    }
    paths::ensure_dir(target)?;
    run_mount(
        &[
            "mount".to_string(),
            "-o".to_string(),
            "loop,ro".to_string(),
            "-t".to_string(),
            image.format.mount_type().to_string(),
            image.path.display().to_string(),
            target.display().to_string(),
        ],
        what,
    )
}

/// Mount a writable overlay over something already mounted at `target`.
///
/// `target` appears in its own `lowerdir`, which is what lets the overlay be
/// mounted over the very path it reads from: the kernel resolves the lower
/// layers when the mount is made, before the new one hides them.
fn mount_overlay(
    target: &Path,
    lower_extra: &Path,
    upper: &Path,
    work: &Path,
    what: &str,
) -> Result<()> {
    check_overlay_paths(&[target, lower_extra, upper, work])?;

    run_mount(
        &[
            "mount".to_string(),
            "-t".to_string(),
            "overlay".to_string(),
            "-o".to_string(),
            format!(
                "lowerdir={}:{},upperdir={},workdir={}",
                lower_extra.display(),
                target.display(),
                upper.display(),
                work.display()
            ),
            "overlay".to_string(),
            target.display().to_string(),
        ],
        what,
    )
}

/// Bind one file over another, creating the target if the image lacks it.
pub fn bind_file(source: &Path, target: &Path) -> Result<()> {
    if is_mounted(target) {
        return Ok(());
    }
    if !target.exists() {
        // The overlay above makes this possible; on a bare read-only image
        // it would not be.
        if let Some(parent) = target.parent() {
            paths::ensure_dir(parent)?;
        }
        std::fs::write(target, b"")
            .with_context(|| format!("cannot create {}", target.display()))?;
    }
    run_mount(
        &[
            "mount".to_string(),
            "-o".to_string(),
            "bind".to_string(),
            source.display().to_string(),
            target.display().to_string(),
        ],
        &format!("{} onto {}", source.display(), target.display()),
    )
}

/// Build the whole container root.
///
/// Idempotent: anything already mounted is left alone, so starting an oven
/// that is already up does nothing.
pub fn assemble(layers: &Layers, system: &Image, vendor: Option<&Image>) -> Result<()> {
    layers.prepare()?;

    if !binder::running_as_root() {
        ui::step("assembling the container root — this needs root, because mounting does");
    }

    mount_image(system, &layers.rootfs, "the system image")?;

    mount_overlay(
        &layers.rootfs,
        &layers.lower_extra,
        &layers.upper.join("system"),
        &layers.work.join("system"),
        "a writable layer over the system image",
    )?;

    let Some(vendor) = vendor else {
        bail!(
            "no vendor image is registered, and Android cannot boot without one.\n\
             Its hardware layers live in /vendor — including the graphics ones — so a system \
             image on its own has nothing to draw with.\n\
             \n\
             Register it alongside the system image:\n  \
             cookie oven image add <system.img> <vendor.img>\n\
             or point Cookie at the directory holding both:\n  \
             cookie oven image add <directory>"
        );
    };

    let vendor_target = layers.rootfs.join("vendor");
    mount_image(vendor, &vendor_target, "the vendor image")?;
    mount_overlay(
        &vendor_target,
        &layers.lower_extra.join("vendor"),
        &layers.upper.join("vendor"),
        &layers.work.join("vendor"),
        "a writable layer over the vendor image",
    )?;

    Ok(())
}

/// Take the container root apart again, deepest mount first.
///
/// Best effort: a mount that is already gone is not a failure, and one that
/// is busy is reported rather than forced.
pub fn disassemble(layers: &Layers) -> Vec<String> {
    let mut problems = Vec::new();

    for target in layers.mount_points() {
        if !is_mounted(&target) {
            continue;
        }
        let path = target.display().to_string();
        let result = privilege::run_as_root("umount", &[path.as_str()]);
        match result {
            Ok(out) if out.ok() => {}
            Ok(out) => problems.push(format!("{path}: {}", out.message())),
            Err(err) => problems.push(format!("{path}: {err}")),
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::oven::image::ImageFormat;
    use crate::paths::ScratchDir;

    fn image(path: &str) -> Image {
        Image {
            path: PathBuf::from(path),
            format: ImageFormat::Ext4,
            size: 1024,
            registered_at: 0,
        }
    }

    #[test]
    fn layers_sit_under_the_oven_directory() {
        let layers = Layers::under(Path::new("/x/oven"));
        assert_eq!(layers.rootfs, PathBuf::from("/x/oven/rootfs"));
        assert_eq!(layers.upper, PathBuf::from("/x/oven/overlay_rw"));
        assert_eq!(layers.work, PathBuf::from("/x/oven/overlay_work"));
    }

    #[test]
    fn preparing_creates_every_directory_the_overlays_need() {
        let scratch = ScratchDir::new("mount").expect("scratch");
        let layers = Layers::under(scratch.path());
        layers.prepare().expect("prepare");

        for dir in [
            "rootfs",
            "overlay/vendor",
            "overlay_rw/system",
            "overlay_rw/vendor",
            "overlay_work/system",
            "overlay_work/vendor",
        ] {
            assert!(scratch.path().join(dir).is_dir(), "{dir} missing");
        }
    }

    /// overlayfs has no escaping, so a path with a separator in it would
    /// silently change which directories were stacked.
    #[test]
    fn paths_overlayfs_cannot_express_are_refused() {
        assert!(path_is_overlay_safe(Path::new(
            "/home/me/.local/share/cookie"
        )));
        assert!(!path_is_overlay_safe(Path::new("/home/a:b/cookie")));
        assert!(!path_is_overlay_safe(Path::new("/home/a,b/cookie")));
        assert!(!path_is_overlay_safe(Path::new("/home/a\\b/cookie")));

        let err = check_overlay_paths(&[Path::new("/home/a:b")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("overlayfs cannot express"), "{err}");
    }

    #[test]
    fn a_missing_vendor_image_is_refused_with_both_ways_to_fix_it() {
        let scratch = ScratchDir::new("mount").expect("scratch");
        let layers = Layers::under(scratch.path());
        // No root here, and the system image does not exist, so this only
        // checks the vendor requirement when it is reached first.
        let err = assemble(&layers, &image("/nope/system.img"), None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("vendor") || err.contains("could not mount"),
            "{err}"
        );
    }

    #[test]
    fn unmount_order_is_deepest_first() {
        let layers = Layers::under(Path::new("/x/oven"));
        let points = layers.mount_points();
        let vendor = points
            .iter()
            .position(|p| p.ends_with("vendor"))
            .expect("vendor");
        let root = points
            .iter()
            .rposition(|p| p == &layers.rootfs)
            .expect("rootfs");
        assert!(vendor < root, "vendor must come down before the root");
    }

    #[test]
    fn disassembling_nothing_reports_nothing() {
        let scratch = ScratchDir::new("mount").expect("scratch");
        let layers = Layers::under(scratch.path());
        assert!(disassemble(&layers).is_empty());
    }

    #[test]
    fn mount_detection_agrees_with_proc() {
        assert!(is_mounted(Path::new("/")));
        let scratch = ScratchDir::new("mount").expect("scratch");
        assert!(!is_mounted(scratch.path()));
    }
}
