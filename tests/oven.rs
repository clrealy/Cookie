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

    let registered = oven.add_image(&path).expect("add");
    assert_eq!(registered.format, image::ImageFormat::Ext4);
    assert!(registered.is_present());
    assert!(registered.path.is_absolute(), "the path must be resolved");
}

#[test]
fn registration_persists_across_handles() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let root = scratch.path().join("oven");

    let path = scratch.path().join("system.img");
    fake_ext4(&path);
    Oven::with_dirs(OvenDirs::under(&root))
        .add_image(&path)
        .expect("add");

    // A completely separate handle reads it back.
    let reopened = Oven::with_dirs(OvenDirs::under(&root));
    let record = reopened.record().expect("record");
    assert!(record.image.is_some());
    assert_eq!(
        record.image.as_ref().map(|i| i.format),
        Some(image::ImageFormat::Ext4)
    );
}

#[test]
fn an_image_that_changed_under_us_is_not_trusted() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let oven = oven(&scratch);

    let path = scratch.path().join("system.img");
    fake_ext4(&path);
    oven.add_image(&path).expect("add");
    assert!(oven.readiness().can_start() || !oven.readiness().lxc_installed);

    // Something replaced the file.
    fs::write(&path, b"not the image you registered").expect("write");
    let image = oven
        .record()
        .expect("record")
        .image
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
    oven.add_image(&path).expect("add");

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

    let err = oven(&scratch).add_image(&path).unwrap_err().to_string();
    assert!(err.contains("sparse"), "{err}");
    assert!(err.contains("simg2img"), "{err}");
    // And nothing was recorded.
    assert!(oven(&scratch).record().expect("record").image.is_none());
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

    let readiness = oven.readiness();
    assert!(!readiness.display_bridge);
    assert!(!readiness.can_run_apps());

    let err = oven.launch("com.example.teapot").unwrap_err().to_string();
    assert!(err.contains("display bridge"), "{err}");
    assert!(
        err.contains("Waydroid"),
        "the working backend must be named:\n{err}"
    );
}

#[test]
fn burnt_is_told_about_the_missing_bridge() {
    let scratch = ScratchDir::new("oven-it").expect("scratch");
    let notes = oven(&scratch).health_notes();

    let bridge = notes
        .iter()
        .find(|n| n.label == "display bridge")
        .expect("the bridge must be reported");
    assert!(bridge.blocking, "it stops apps running, so it must block");
    assert!(bridge.advice.is_some());

    // And the image is reported too, with advice when it is missing.
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
