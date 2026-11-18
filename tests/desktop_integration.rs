//! Desktop integration, end to end against a scratch directory.
//!
//! `DesktopDirs::under` points the whole module at a temporary tree, so these
//! tests drive exactly the code the binary runs without touching the real
//! home directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use cookie::cpk::manifest::{BakedMeta, CookieMeta, CpkManifest, CPK_FORMAT_VERSION};
use cookie::desktop::{self, icons, mime, DesktopDirs};
use cookie::jar;
use cookie::paths::ScratchDir;

fn manifest(name: &str, package: &str) -> CpkManifest {
    CpkManifest {
        cpk_version: CPK_FORMAT_VERSION,
        cookie: CookieMeta {
            name: name.to_string(),
            package: package.to_string(),
            version_name: Some("1.0".into()),
            version_code: Some(1),
            abis: vec!["arm64-v8a".into()],
            icon: Some("icon.png".into()),
            min_sdk: None,
            target_sdk: None,
            launchable_activity: None,
        },
        baked: BakedMeta {
            by: "cookie test".into(),
            at: "2026-11-18T00:00:00Z".into(),
            base_apk: "app/base.apk".into(),
            splits: Vec::new(),
            signature: None,
            legacy_signature_sha256: None,
            files: BTreeMap::new(),
        },
    }
}

/// Put one app in a scratch jar and return it.
fn stock(jar_dir: &Path, name: &str, package: &str, icon: Option<&[u8]>) -> jar::Cookie {
    jar::put_in(jar_dir, &manifest(name, package), icon, None, "waydroid").expect("jar entry");
    jar::get_in(jar_dir, package)
        .expect("read back")
        .expect("present")
}

#[test]
fn setup_writes_everything_a_double_click_needs() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());

    assert!(!desktop::status(&dirs).is_ready(), "nothing installed yet");

    desktop::setup(&dirs).expect("setup");

    let status = desktop::status(&dirs);
    assert!(status.mime_registered);
    assert!(status.handler_installed);
    assert!(status.is_default_handler);
    assert!(status.is_ready());

    assert!(dirs.mime_packages.join("cookie.xml").is_file());
    assert!(dirs
        .applications
        .join(desktop::HANDLER_DESKTOP_ID)
        .is_file());
    assert!(dirs.icons.join("hicolor/256x256/apps/cookie.png").is_file());
    assert!(dirs
        .icons
        .join("hicolor/256x256/mimetypes/application-x-cookie-package.png")
        .is_file());
}

#[test]
fn setup_is_idempotent() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());

    desktop::setup(&dirs).expect("first");
    let first = fs::read_to_string(dirs.mimeapps.clone()).expect("read");
    desktop::setup(&dirs).expect("second");
    let second = fs::read_to_string(dirs.mimeapps.clone()).expect("read");

    assert_eq!(
        first, second,
        "running setup twice must not duplicate entries"
    );
    assert_eq!(
        second.matches("application/x-cookie-package=").count(),
        1,
        "{second}"
    );
}

#[test]
fn teardown_undoes_setup() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());

    desktop::setup(&dirs).expect("setup");
    desktop::teardown(&dirs, false).expect("teardown");

    let status = desktop::status(&dirs);
    assert!(!status.mime_registered);
    assert!(!status.handler_installed);
    assert!(!status.is_default_handler);
    assert!(!dirs.icons.join("hicolor/256x256/apps/cookie.png").exists());
}

#[test]
fn a_launcher_is_created_with_a_themed_icon() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());
    let jar_dir = scratch.path().join("jar");

    let cookie = stock(
        &jar_dir,
        "Teapot",
        "com.example.teapot",
        Some(cookie::assets::COOKIE_ICON_256),
    );

    let path = desktop::install_launcher(&dirs, &cookie, Some("waydroid.com.example.teapot"))
        .expect("launcher");
    let text = fs::read_to_string(&path).expect("read");

    assert!(text.starts_with("[Desktop Entry]\n"), "{text}");
    assert!(text.contains("Name=Teapot"), "{text}");
    assert!(text.contains("eat com.example.teapot"), "{text}");
    assert!(text.contains("Icon=cookie-com.example.teapot"), "{text}");
    assert!(
        text.contains("StartupWMClass=waydroid.com.example.teapot"),
        "{text}"
    );
    assert!(
        text.contains("X-Cookie-Package=com.example.teapot"),
        "{text}"
    );
    assert!(text.contains("Terminal=false"), "{text}");

    assert!(dirs
        .icons
        .join("hicolor/256x256/apps/cookie-com.example.teapot.png")
        .is_file());
}

#[test]
fn a_launcher_for_an_app_with_no_icon_still_works() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());
    let jar_dir = scratch.path().join("jar");

    let cookie = stock(&jar_dir, "Plain", "com.example.plain", None);
    let path = desktop::install_launcher(&dirs, &cookie, None).expect("launcher");
    let text = fs::read_to_string(&path).expect("read");

    assert!(text.contains("Name=Plain"), "{text}");
    assert!(
        !text.contains("Icon="),
        "no icon key when there is no icon:\n{text}"
    );
    assert!(
        !text.contains("StartupWMClass="),
        "no window class when the backend cannot say:\n{text}"
    );
}

#[test]
fn a_webp_icon_falls_back_to_a_path_reference() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());
    let jar_dir = scratch.path().join("jar");

    // Icon themes only index PNG, SVG and XPM, so a WebP icon has to be
    // referenced by absolute path instead.
    let mut m = manifest("Webby", "com.example.webby");
    m.cookie.icon = Some("icon.webp".into());
    jar::put_in(
        &jar_dir,
        &m,
        Some(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
        None,
        "waydroid",
    )
    .expect("jar entry");
    let cookie = jar::get_in(&jar_dir, "com.example.webby")
        .expect("read")
        .expect("present");

    let path = desktop::install_launcher(&dirs, &cookie, None).expect("launcher");
    let text = fs::read_to_string(&path).expect("read");

    assert!(
        text.contains("Icon=/"),
        "an absolute path was expected:\n{text}"
    );
    assert!(text.contains("icon.webp"), "{text}");
}

#[test]
fn removing_a_launcher_takes_its_icon_too() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());
    let jar_dir = scratch.path().join("jar");

    let cookie = stock(
        &jar_dir,
        "Teapot",
        "com.example.teapot",
        Some(cookie::assets::COOKIE_ICON_256),
    );
    desktop::install_launcher(&dirs, &cookie, None).expect("launcher");

    assert!(desktop::remove_launcher(&dirs, "com.example.teapot").expect("remove"));
    assert!(!dirs
        .applications
        .join("cookie-com.example.teapot.desktop")
        .exists());
    assert!(!dirs
        .icons
        .join("hicolor/256x256/apps/cookie-com.example.teapot.png")
        .exists());

    // Removing it again is not an error.
    assert!(!desktop::remove_launcher(&dirs, "com.example.teapot").expect("remove again"));
}

#[test]
fn only_our_own_desktop_files_are_touched() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());
    fs::create_dir_all(&dirs.applications).expect("mkdir");

    // Something else's file that happens to match our naming.
    let impostor = dirs.applications.join("cookie-com.example.other.desktop");
    fs::write(
        &impostor,
        "[Desktop Entry]\nType=Application\nName=Someone else's\nExec=true\n",
    )
    .expect("write");

    assert!(
        !desktop::remove_launcher(&dirs, "com.example.other").expect("remove"),
        "a file without our marker must not be removed"
    );
    assert!(impostor.exists(), "someone else's launcher must survive");

    // And it is not listed as ours.
    assert!(desktop::list_launchers(&dirs).expect("list").is_empty());
}

#[test]
fn launchers_are_listed_by_package() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());
    let jar_dir = scratch.path().join("jar");

    for (name, package) in [
        ("Teapot", "com.example.teapot"),
        ("Kettle", "com.example.kettle"),
    ] {
        let cookie = stock(&jar_dir, name, package, None);
        desktop::install_launcher(&dirs, &cookie, None).expect("launcher");
    }

    assert_eq!(
        desktop::list_launchers(&dirs).expect("list"),
        vec![
            "com.example.kettle".to_string(),
            "com.example.teapot".to_string()
        ]
    );
}

#[test]
fn teardown_with_launchers_removes_them_all() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());
    let jar_dir = scratch.path().join("jar");

    desktop::setup(&dirs).expect("setup");
    let cookie = stock(&jar_dir, "Teapot", "com.example.teapot", None);
    desktop::install_launcher(&dirs, &cookie, None).expect("launcher");
    assert_eq!(desktop::status(&dirs).launchers, 1);

    desktop::teardown(&dirs, true).expect("teardown");
    assert_eq!(desktop::status(&dirs).launchers, 0);
}

#[test]
fn an_existing_mimeapps_list_survives_setup() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());
    fs::create_dir_all(dirs.mimeapps.parent().expect("parent")).expect("mkdir");
    fs::write(
        &dirs.mimeapps,
        "[Default Applications]\n\
         text/plain=org.kde.kwrite.desktop\n\
         inode/directory=org.kde.dolphin.desktop\n",
    )
    .expect("write");

    desktop::setup(&dirs).expect("setup");
    let text = fs::read_to_string(&dirs.mimeapps).expect("read");

    assert!(text.contains("text/plain=org.kde.kwrite.desktop"), "{text}");
    assert!(
        text.contains("inode/directory=org.kde.dolphin.desktop"),
        "{text}"
    );
    assert!(
        text.contains("application/x-cookie-package=cookie.desktop"),
        "{text}"
    );

    // And teardown leaves the others alone.
    desktop::teardown(&dirs, false).expect("teardown");
    let text = fs::read_to_string(&dirs.mimeapps).expect("read");
    assert!(text.contains("text/plain=org.kde.kwrite.desktop"), "{text}");
    assert!(!text.contains("x-cookie-package"), "{text}");
}

#[test]
fn a_hostile_app_name_cannot_inject_desktop_keys() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());
    let jar_dir = scratch.path().join("jar");

    // An APK label is attacker-controlled. This one tries to add its own
    // Exec key by closing the Name value with a newline.
    let evil = "Harmless\nExec=/bin/sh -c 'touch /tmp/pwned'\nX-Evil=yes";
    let cookie = stock(&jar_dir, evil, "com.example.evil", None);

    let path = desktop::install_launcher(&dirs, &cookie, None).expect("launcher");
    let text = fs::read_to_string(&path).expect("read");

    // What matters is that the payload stayed inside the Name value and
    // never became a key of its own.
    let keys: Vec<&str> = text
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .collect();
    assert_eq!(
        keys.iter().filter(|k| **k == "Exec").count(),
        1,
        "exactly one Exec key:\n{text}"
    );
    assert_eq!(
        keys.iter().filter(|k| **k == "Name").count(),
        1,
        "exactly one Name key:\n{text}"
    );
    assert!(!keys.contains(&"X-Evil"), "keys were {keys:?} in:\n{text}");
    assert!(
        text.lines()
            .any(|l| l.starts_with("Exec=") && l.contains("eat com.example.evil")),
        "the one Exec key must be ours:\n{text}"
    );
    // And the name is still readable rather than welded together.
    assert!(
        text.contains("Name=Harmless Exec="),
        "words either side of the newline must stay separate:\n{text}"
    );
}

#[test]
fn a_hostile_package_id_cannot_escape_the_applications_directory() {
    let scratch = ScratchDir::new("desktop").expect("scratch");
    let dirs = DesktopDirs::under(scratch.path());

    let id = desktop::app_desktop_id("../../../../etc/cron.d/evil");
    assert!(!id.contains('/'), "{id}");
    assert!(id.starts_with("cookie-"), "{id}");
    assert!(id.ends_with(".desktop"), "{id}");

    let icon = desktop::app_icon_name("../../evil");
    assert!(!icon.contains('/'), "{icon}");

    // And nothing lands outside the tree.
    let jar_dir = scratch.path().join("jar");
    let cookie = stock(&jar_dir, "Evil", "../../../../tmp/evil", None);
    let path = desktop::install_launcher(&dirs, &cookie, None).expect("launcher");
    assert!(
        path.starts_with(&dirs.applications),
        "{} escaped {}",
        path.display(),
        dirs.applications.display()
    );
}

#[test]
fn the_mime_definition_is_what_the_desktop_expects() {
    let xml = mime::mime_xml();
    assert!(
        xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"),
        "{xml}"
    );
    assert!(
        xml.contains("xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\""),
        "{xml}"
    );
    // Detection is by content: see desktop::mime for why there is no glob.
    assert!(!xml.contains("<glob"), "{xml}");
    assert!(xml.contains("manifest.toml"), "{xml}");
}

#[test]
fn icon_sizes_are_read_from_the_png_itself() {
    assert_eq!(
        icons::png_size(cookie::assets::COOKIE_ICON_256),
        Some((256, 256))
    );
    assert_eq!(
        icons::png_size(cookie::assets::COOKIE_ICON_128),
        Some((128, 128))
    );
    assert_eq!(icons::png_size(b"not a png"), None);
}
