//! The oven, end to end against a scratch directory.
//!
//! Everything here runs without LXC, without an Android image, and without
//! kernel binder support — which is the point. The parts of the oven that can
//! work on any machine are the parts milestone 4 actually delivers, and they
//! are tested as such. What cannot work yet has to *say so*, and that is
//! tested too.

use std::fs;
use std::path::{Path, PathBuf};

use cookie::backend::oven::{
    binder, container::ContainerConfig, image, Oven, OvenDirs, OvenRecord,
};
use cookie::backend::AndroidBackend;
use cookie::paths::ScratchDir;

fn oven(scratch: &ScratchDir) -> Oven {
    Oven::with_dirs(OvenDirs::under(&scratch.path().join("oven")))
}

/// A file carrying a real ext4 superblock magic, big enough to be plausible.
fn fake_ext4(path: &Path) {
    let mut data = vec![0u8; 20 * 1024 * 1024];
    let at = 1024 + 0x38;
    data[at..at + 2].copy_from_slice(&0xEF53u16.to_le_bytes());
    fs::write(path, &data).expect("write image");
}

// ---------------------------------------------------------------------------
// The shared system image
// ---------------------------------------------------------------------------

#[test]
fn an_image_is_checked_before_it_is_accepted() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven = oven(&scratch);

    let path = scratch.path().join("system.img");
    fake_ext4(&path);

    let registered = oven.add_images(&path, None).expect("add");
    assert_eq!(registered.system.format, image::ImageFormat::Ext4);
    assert!(registered.system.is_present());
    assert!(
        registered.system.path.is_absolute(),
        "the path must be resolved"
    );
}

#[test]
fn registration_persists_across_handles() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let root = scratch.path().join("oven");

    let path = scratch.path().join("system.img");
    fake_ext4(&path);
    Oven::with_dirs(OvenDirs::under(&root))
        .add_images(&path, None)
        .expect("add");

    // A completely separate handle reads it back.
    let reopened = Oven::with_dirs(OvenDirs::under(&root));
    let record = reopened.record().expect("record");
    assert!(record.system.is_some());
    assert_eq!(
        record.system.as_ref().map(|i| i.format),
        Some(image::ImageFormat::Ext4)
    );
}

#[test]
fn an_image_that_changed_under_us_is_not_trusted() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven = oven(&scratch);

    let path = scratch.path().join("system.img");
    fake_ext4(&path);
    oven.add_images(&path, None).expect("add");
    assert!(oven.readiness().can_start() || !oven.readiness().lxc_installed);

    // Something replaced the file.
    fs::write(&path, b"not the image you registered").expect("write");
    let image = oven
        .record()
        .expect("record")
        .system
        .expect("still recorded");
    assert!(!image.is_present());
    assert!(!oven.readiness().can_start());
}

#[test]
fn forgetting_an_image_leaves_the_file_alone() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven = oven(&scratch);

    let path = scratch.path().join("system.img");
    fake_ext4(&path);
    oven.add_images(&path, None).expect("add");

    assert!(oven.remove_image().expect("remove"));
    assert_eq!(oven.record().expect("record"), OvenRecord::default());
    assert!(path.is_file(), "the image file itself must survive");
    assert!(!oven.remove_image().expect("remove again"));
}

#[test]
fn a_sparse_image_is_named_as_such_rather_than_rejected_vaguely() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let path = scratch.path().join("sparse.img");
    let mut data = vec![0u8; 20 * 1024 * 1024];
    data[0..4].copy_from_slice(&0xED26_FF3Au32.to_le_bytes());
    fs::write(&path, &data).expect("write");

    let err = oven(&scratch)
        .add_images(&path, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("sparse"), "{err}");
    assert!(err.contains("simg2img"), "{err}");
    // And nothing was recorded.
    assert!(oven(&scratch).record().expect("record").system.is_none());
}

// ---------------------------------------------------------------------------
// Honesty
// ---------------------------------------------------------------------------

/// The oven must never imply it can run an app. This is the test that keeps
/// milestone 4 honest.
#[test]
fn the_oven_does_not_pretend_it_can_show_a_window() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven = oven(&scratch);

    // No image is registered, so nothing can run whatever the display says.
    let readiness = oven.readiness();
    assert!(!readiness.can_start());
    assert!(!readiness.can_show_windows());

    let err = oven.launch("com.example.teapot").unwrap_err().to_string();
    assert!(
        err.contains("system image") || err.contains("LXC") || err.contains("Wayland"),
        "it must name what is missing:\n{err}"
    );
}

#[test]
fn burnt_is_told_what_the_display_is_doing() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let notes = oven(&scratch).health_notes();

    let display = notes
        .iter()
        .find(|n| n.label == "display")
        .expect("the display must be reported");
    // Whether it blocks depends on the machine; a blocking note must always
    // carry advice.
    if display.blocking {
        assert!(display.advice.is_some(), "a blocking note needs advice");
    }

    // The one thing the host cannot check has to be stated either way.
    let caveat = notes
        .iter()
        .find(|n| n.label == "image graphics")
        .expect("the image caveat must be reported");
    assert!(caveat.advice.is_some());
    assert!(
        !caveat.blocking,
        "it is a caveat, not a failure: it cannot be known either way"
    );

    // And the image itself is still reported, with advice when missing.
    let image = notes
        .iter()
        .find(|n| n.label == "system image")
        .expect("the image must be reported");
    assert!(image.blocking);
    assert!(image
        .advice
        .as_ref()
        .map(|a| a.contains("does not download"))
        .unwrap_or(false));
}

#[test]
fn the_oven_never_offers_to_fetch_an_image() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let notes = oven(&scratch).health_notes();
    let all = notes
        .iter()
        .filter_map(|n| n.advice.clone())
        .collect::<Vec<_>>()
        .join("\n");

    for forbidden in ["http://", "https://", "wget", "curl", "download the"] {
        assert!(
            !all.to_lowercase().contains(forbidden),
            "the oven must not point at a download ({forbidden}):\n{all}"
        );
    }
}

#[test]
fn waydroid_remains_the_default_backend() {
    let default = cookie::backend::detect(None).expect("default backend");
    assert_eq!(default.name(), "waydroid");

    let explicit = cookie::backend::detect(Some("oven")).expect("oven backend");
    assert_eq!(explicit.name(), "oven");

    let err = match cookie::backend::detect(Some("nonsense")) {
        Err(err) => err.to_string(),
        Ok(backend) => panic!("an unknown backend resolved to {}", backend.name()),
    };
    assert!(err.contains("waydroid"), "{err}");
    assert!(err.contains("oven"), "{err}");
}

// ---------------------------------------------------------------------------
// The container
// ---------------------------------------------------------------------------

#[test]
fn the_container_config_is_built_from_this_machine() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven = oven(&scratch);

    let config = oven.container_config().expect("config");
    assert_eq!(config.rootfs, oven.dirs().rootfs);
    assert_eq!(config.data_dir, oven.dirs().data);
    assert_eq!(config.binder_devices, binder::REQUIRED_DEVICES);

    // Whatever this machine has, the rendering has to be valid.
    let rendered = config.render();
    assert!(rendered.contains("lxc.init.cmd = /init"), "{rendered}");
    assert!(rendered.contains("lxc.rootfs.path = dir:"), "{rendered}");
}

#[test]
fn writing_the_config_creates_the_directories_it_references() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven = oven(&scratch);

    let path = oven.write_container_config().expect("write");
    assert!(path.is_file());
    assert!(oven.dirs().rootfs.is_dir());
    assert!(oven.dirs().data.is_dir());

    let text = fs::read_to_string(&path).expect("read");
    // The paths in the config must be the ones that now exist.
    assert!(
        text.contains(&oven.dirs().rootfs.display().to_string()),
        "{text}"
    );
    assert!(
        text.contains(&oven.dirs().data.display().to_string()),
        "{text}"
    );
}

#[test]
fn the_config_denies_devices_before_allowing_any() {
    let config = ContainerConfig {
        rootfs: PathBuf::from("/x/rootfs"),
        data_dir: PathBuf::from("/x/data"),
        binder_path: PathBuf::from("/dev/binderfs"),
        binder_devices: vec!["binder".to_string()],
        render_nodes: Vec::new(),
        wayland_socket: None,
        pulse_socket: None,
        cgroup_v2: true,
        host_egl_dirs: Vec::new(),
        uid: 1000,
        gid: 1000,
    };
    let rendered = config.render();

    let deny = rendered.find("devices.deny = a").expect("deny-all");
    let allow = rendered.find("devices.allow").expect("an allow");
    assert!(
        deny < allow,
        "a deny-all after the allows undoes them:\n{rendered}"
    );
}

#[test]
fn rewriting_the_config_is_stable() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven = oven(&scratch);

    let first = fs::read_to_string(oven.write_container_config().expect("write")).expect("read");
    let second = fs::read_to_string(oven.write_container_config().expect("write")).expect("read");
    assert_eq!(first, second);
}

// ---------------------------------------------------------------------------
// Binder
// ---------------------------------------------------------------------------

#[test]
fn binder_advice_names_the_kernel_options_and_refuses_to_build_a_module() {
    let message = binder::unsupported_message();
    assert!(message.contains("CONFIG_ANDROID_BINDER_IPC"), "{message}");
    assert!(message.contains("CONFIG_ANDROID_BINDERFS"), "{message}");
    assert!(
        message.contains("will not build or load a kernel module"),
        "this is the promise Cookie makes, and it has to be in the message:\n{message}"
    );
}

#[test]
fn the_mount_command_is_shown_before_root_is_asked_for() {
    // The command is returned rather than run, so the caller can print it.
    let command = binder::mount_command(Path::new("/dev/binderfs"));
    assert_eq!(command[0], "mount");
    assert!(command.contains(&"binder".to_string()));
    assert!(command.contains(&"/dev/binderfs".to_string()));
}

#[test]
fn binder_state_reading_never_panics_on_this_host() {
    let state = binder::state();
    // Whatever it is, is_ready must agree with itself.
    let _ = state.is_ready();
    let _ = binder::devices_present(Path::new(binder::DEFAULT_MOUNT_POINT));
    let _ = binder::devices_missing(Path::new(binder::DEFAULT_MOUNT_POINT));
}

// ---------------------------------------------------------------------------
// Commands that need a running container
// ---------------------------------------------------------------------------

#[test]
fn app_commands_refuse_clearly_without_an_image() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven = oven(&scratch);

    // Each of these needs a prepared oven, and must say which part is absent
    // rather than failing obscurely.
    let errors = [
        oven.install(&[PathBuf::from("/tmp/x.apk")], &Default::default())
            .unwrap_err()
            .to_string(),
        oven.uninstall("com.example.teapot")
            .unwrap_err()
            .to_string(),
        oven.list_installed().unwrap_err().to_string(),
    ];

    for err in errors {
        assert!(
            err.contains("system image") || err.contains("LXC"),
            "the message must name what is missing:\n{err}"
        );
    }
}

#[test]
fn window_classes_do_not_collide_with_waydroids() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven_class = oven(&scratch).window_class("com.example.teapot");
    let waydroid_class = cookie::backend::detect(Some("waydroid"))
        .expect("waydroid")
        .window_class("com.example.teapot");

    assert_ne!(
        oven_class, waydroid_class,
        "two backends must not claim the same window class"
    );
    assert_eq!(oven_class.as_deref(), Some("cookie.com.example.teapot"));
}

// ---------------------------------------------------------------------------
// The container root, assembled for real
// ---------------------------------------------------------------------------

/// Can this machine actually mount things?
///
/// The assembly test below does real loop and overlay mounts, so it only
/// runs where that is possible. Skipping is better than a test that passes
/// by not testing anything, so it says when it skips.
fn can_really_mount() -> bool {
    let root = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|v| v.split_whitespace().next().map(str::to_string))
        })
        .map(|uid| uid == "0")
        .unwrap_or(false);
    root && Path::new("/dev/loop-control").exists()
}

/// Build a real ext4 image with the directories an Android image has.
fn real_ext4_image(path: &Path, scratch: &Path) -> bool {
    let build = scratch.join(format!(
        "build-{}",
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    for dir in ["dev", "data", "vendor", "system", "proc", "sys"] {
        if std::fs::create_dir_all(build.join(dir)).is_err() {
            return false;
        }
    }
    let _ = std::fs::write(build.join("init"), b"");

    let ok = std::process::Command::new("truncate")
        .args(["-s", "64M"])
        .arg(path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return false;
    }

    std::process::Command::new("mkfs.ext4")
        .args(["-q", "-F", "-d"])
        .arg(&build)
        .arg(path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The blocker this whole layer exists to fix: on a bare read-only image,
/// the container cannot create the device nodes and mount points it needs.
/// With the overlay it can.
#[test]
fn the_assembled_root_is_writable_where_a_bare_image_is_not() {
    if !can_really_mount() {
        eprintln!("skipping: needs root and loop devices to mount anything");
        return;
    }

    let scratch = ScratchDir::new("oven-assemble").expect("scratch");
    let system_path = scratch.path().join("system.img");
    let vendor_path = scratch.path().join("vendor.img");
    if !real_ext4_image(&system_path, scratch.path())
        || !real_ext4_image(&vendor_path, scratch.path())
    {
        eprintln!("skipping: could not build test images");
        return;
    }

    let oven = Oven::with_dirs(OvenDirs::under(&scratch.path().join("oven")));
    let set = oven
        .add_images(&system_path, Some(&vendor_path))
        .expect("register both images");
    let layers = oven.layers();

    cookie::backend::oven::mount::assemble(&layers, &set.system, set.vendor.as_ref())
        .expect("assemble the container root");

    // Everything that failed on a bare read-only image.
    let created = [
        layers.rootfs.join("dev/binder"),
        layers.rootfs.join("dev/sw_sync"),
        layers.rootfs.join("run/xdg"),
        layers.rootfs.join("vendor/waydroid.prop"),
    ];
    for path in &created {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
        }
        std::fs::write(path, b"")
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", path.display()));
    }

    // The vendor image is mounted in its own right, not just a directory.
    assert!(
        cookie::backend::oven::mount::is_mounted(&layers.rootfs.join("vendor")),
        "the vendor image must be mounted at rootfs/vendor"
    );

    // Writes went to the upper layer, leaving the images untouched.
    assert!(
        layers.upper.join("system/dev/binder").exists(),
        "writes must land in the overlay, not the image"
    );

    // And it all comes apart again.
    let problems = cookie::backend::oven::mount::disassemble(&layers);
    assert!(problems.is_empty(), "unmount problems: {problems:?}");
    assert!(
        !cookie::backend::oven::mount::is_mounted(&layers.rootfs),
        "nothing should be left mounted"
    );
}
