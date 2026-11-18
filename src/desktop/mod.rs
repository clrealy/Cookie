//! Making cookies look like ordinary programs to the desktop.
//!
//! Three things, all under the user's own directories, none needing root:
//!
//! - the `application/x-cookie-package` MIME type, so a `.cpk` is recognised;
//! - `cookie.desktop`, so double-clicking one runs `cookie eat`;
//! - one `.desktop` launcher per installed app, with its icon, so it turns up
//!   in the application menu like anything else.
//!
//! Every path is passed in rather than looked up, so the tests drive exactly
//! the same code the binary does, against a scratch directory.

pub mod entry;
pub mod icons;
pub mod mime;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::assets;
use crate::backend::cmd;
use crate::cpk;
use crate::jar::Cookie;
use crate::paths;

/// Desktop id of Cookie's own entry, the one that opens `.cpk` files.
pub const HANDLER_DESKTOP_ID: &str = "cookie.desktop";

/// Themed icon name for Cookie itself.
pub const COOKIE_ICON_NAME: &str = "cookie";

/// Themed icon name shown on `.cpk` files in a file manager.
pub const MIME_ICON_NAME: &str = "application-x-cookie-package";

/// Marks a `.desktop` file as ours, so removal never touches anyone else's.
const MANAGED_KEY: &str = "X-Cookie-Managed";

/// The directories desktop integration writes into.
#[derive(Debug, Clone)]
pub struct DesktopDirs {
    pub applications: PathBuf,
    pub mime_root: PathBuf,
    pub mime_packages: PathBuf,
    pub icons: PathBuf,
    pub mimeapps: PathBuf,
}

impl DesktopDirs {
    /// The real XDG locations for this user.
    pub fn user() -> Result<Self> {
        let data =
            dirs::data_dir().context("cannot work out your data directory (is $HOME set?)")?;
        let config =
            dirs::config_dir().context("cannot work out your config directory (is $HOME set?)")?;
        Ok(Self {
            applications: data.join("applications"),
            mime_root: data.join("mime"),
            mime_packages: data.join("mime/packages"),
            icons: data.join("icons"),
            mimeapps: config.join("mimeapps.list"),
        })
    }

    /// The same layout under an arbitrary root, for tests.
    pub fn under(root: &Path) -> Self {
        Self {
            applications: root.join("share/applications"),
            mime_root: root.join("share/mime"),
            mime_packages: root.join("share/mime/packages"),
            icons: root.join("share/icons"),
            mimeapps: root.join("config/mimeapps.list"),
        }
    }
}

/// The `.desktop` filename for an installed app.
pub fn app_desktop_id(package: &str) -> String {
    format!("cookie-{}.desktop", sanitize_component(package))
}

/// The themed icon name for an installed app.
pub fn app_icon_name(package: &str) -> String {
    format!("cookie-{}", sanitize_component(package))
}

/// Package ids are `[A-Za-z0-9_.]` by Android's own rules, but the value
/// reaches us from a file we did not write, so it is reduced to a safe set
/// before becoming part of a filename.
fn sanitize_component(package: &str) -> String {
    package
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            _ => '_',
        })
        .collect()
}

/// Absolute path to the running `cookie` binary.
///
/// Launchers must not rely on `PATH`: a desktop session's environment is not
/// a login shell's, and `~/.local/bin` is often missing from it.
pub fn cookie_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "cookie".to_string())
}

/// Cookie's own desktop entry: the handler for `.cpk` files.
///
/// `NoDisplay=true` keeps it out of the application menu — it is a file
/// handler, not something anyone wants to launch on its own.
pub fn handler_entry(binary: &str) -> entry::DesktopEntry {
    let mut e = entry::DesktopEntry::new();
    e.set("Type", "Application")
        .set("Version", "1.0")
        .set("Name", "Cookie")
        .set("GenericName", "Android app runner")
        .set(
            "Comment",
            "Install and run an Android app packaged as a cookie",
        )
        .set_exec(binary, &["eat", "%f"])
        .set("Icon", COOKIE_ICON_NAME)
        .set_bool("Terminal", false)
        .set_bool("NoDisplay", true)
        .set_bool("StartupNotify", true)
        .set_list("MimeType", &[cpk::MIME_TYPE])
        .set_list("Categories", &["System", "Utility"])
        .set_bool(MANAGED_KEY, true);
    e
}

/// A launcher for one installed app.
pub fn app_entry(
    cookie: &Cookie,
    binary: &str,
    icon: &icons::IconRef,
    window_class: Option<&str>,
) -> entry::DesktopEntry {
    let mut e = entry::DesktopEntry::new();
    e.set("Type", "Application")
        .set("Version", "1.0")
        .set("Name", &entry::clean_name(cookie.name()))
        .set("Comment", "Android app, running in Cookie")
        // Launching by package id rather than by .cpk path: the cookie file
        // may have been moved or deleted since, but the installed app has not.
        .set_exec(binary, &["eat", &cookie.package])
        .set_bool("Terminal", false)
        .set_bool("StartupNotify", true)
        .set_list("Categories", &["Utility"]);

    if let Some(value) = icon.value() {
        e.set("Icon", &value);
    }
    // Lets the taskbar match the app's window to this launcher instead of
    // showing it as a second, unnamed entry.
    if let Some(class) = window_class {
        e.set("StartupWMClass", class);
    }

    e.set("X-Cookie-Package", &cookie.package)
        .set_bool(MANAGED_KEY, true);
    e
}

/// What desktop integration is currently in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub mime_registered: bool,
    pub handler_installed: bool,
    pub is_default_handler: bool,
    pub launchers: usize,
}

impl Status {
    /// True when a double-click on a cookie will work.
    pub fn is_ready(&self) -> bool {
        self.mime_registered && self.handler_installed && self.is_default_handler
    }
}

pub fn status(dirs: &DesktopDirs) -> Status {
    Status {
        mime_registered: dirs.mime_packages.join(mime::MIME_PACKAGE_FILE).is_file(),
        handler_installed: dirs.applications.join(HANDLER_DESKTOP_ID).is_file(),
        is_default_handler: mime::default_application(&dirs.mimeapps, cpk::MIME_TYPE).as_deref()
            == Some(HANDLER_DESKTOP_ID),
        launchers: list_launchers(dirs).map(|l| l.len()).unwrap_or(0),
    }
}

/// Install the MIME type, the handler, and Cookie's icons.
pub fn setup(dirs: &DesktopDirs) -> Result<Vec<String>> {
    let mut done = Vec::new();

    let path = mime::install(&dirs.mime_packages)?;
    done.push(format!(
        "registered {} ({})",
        cpk::MIME_TYPE,
        paths::tilde(&path)
    ));

    // Cookie's launcher icon, and the icon file managers show on .cpk files.
    for (name, context, description) in [
        (COOKIE_ICON_NAME, "apps", "Cookie"),
        (MIME_ICON_NAME, "mimetypes", "cookie file"),
    ] {
        for bytes in [assets::COOKIE_ICON_256, assets::COOKIE_ICON_128] {
            icons::install_in(&dirs.icons, context, name, bytes, description, None)?;
        }
    }
    done.push("installed Cookie's icons".to_string());

    let binary = cookie_binary();
    paths::ensure_dir(&dirs.applications)?;
    let handler_path = dirs.applications.join(HANDLER_DESKTOP_ID);
    fs::write(&handler_path, handler_entry(&binary).render())
        .with_context(|| format!("cannot write {}", handler_path.display()))?;
    done.push(format!("installed {}", paths::tilde(&handler_path)));

    mime::set_default_application(&dirs.mimeapps, cpk::MIME_TYPE, HANDLER_DESKTOP_ID)?;
    done.push(format!(
        "set Cookie as the default for .cpk files ({})",
        paths::tilde(&dirs.mimeapps)
    ));

    Ok(done)
}

/// Undo [`setup`]. App launchers are left alone unless `launchers` is set.
pub fn teardown(dirs: &DesktopDirs, launchers: bool) -> Result<Vec<String>> {
    let mut done = Vec::new();

    if mime::remove(&dirs.mime_packages)? {
        done.push(format!("unregistered {}", cpk::MIME_TYPE));
    }

    let handler_path = dirs.applications.join(HANDLER_DESKTOP_ID);
    if handler_path.exists() {
        fs::remove_file(&handler_path)
            .with_context(|| format!("cannot remove {}", handler_path.display()))?;
        done.push(format!("removed {}", paths::tilde(&handler_path)));
    }

    mime::clear_default_application(&dirs.mimeapps, cpk::MIME_TYPE)?;

    for name in [COOKIE_ICON_NAME, MIME_ICON_NAME] {
        icons::remove(&dirs.icons, name)?;
    }
    done.push("removed Cookie's icons".to_string());

    if launchers {
        let mut removed = 0;
        for package in list_launchers(dirs)? {
            if remove_launcher(dirs, &package)? {
                removed += 1;
            }
        }
        if removed > 0 {
            done.push(format!("removed {removed} app launcher(s)"));
        }
    }

    Ok(done)
}

/// Create or refresh the menu entry for one installed app.
pub fn install_launcher(
    dirs: &DesktopDirs,
    cookie: &Cookie,
    window_class: Option<&str>,
) -> Result<PathBuf> {
    paths::ensure_dir(&dirs.applications)?;

    // Prefer a themed icon; fall back to the copy in the app's jar directory.
    let icon_name = app_icon_name(&cookie.package);
    let icon = match cookie.icon_path() {
        Some(path) => {
            let bytes =
                fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
            icons::install(&dirs.icons, &icon_name, &bytes, cookie.name(), Some(&path))?
        }
        None => icons::IconRef::None,
    };

    let binary = cookie_binary();
    let path = dirs.applications.join(app_desktop_id(&cookie.package));
    fs::write(
        &path,
        app_entry(cookie, &binary, &icon, window_class).render(),
    )
    .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(path)
}

/// Remove one app's menu entry and icon.
pub fn remove_launcher(dirs: &DesktopDirs, package: &str) -> Result<bool> {
    let path = dirs.applications.join(app_desktop_id(package));
    let existed = path.exists();
    if existed {
        // Only ever remove a file we wrote.
        if is_managed(&path) {
            fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
        } else {
            return Ok(false);
        }
    }
    icons::remove(&dirs.icons, &app_icon_name(package))?;
    Ok(existed)
}

/// Package ids of every launcher Cookie manages.
pub fn list_launchers(dirs: &DesktopDirs) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(&dirs.applications) else {
        return Ok(found);
    };
    for item in entries.flatten() {
        let path = item.path();
        let is_ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("cookie-") && n.ends_with(".desktop"))
            .unwrap_or(false);
        if !is_ours {
            continue;
        }
        if let Some(package) = managed_package(&path) {
            found.push(package);
        }
    }
    found.sort();
    Ok(found)
}

/// Did Cookie write this `.desktop` file?
fn is_managed(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .any(|l| l.trim() == format!("{MANAGED_KEY}=true"))
        })
        .unwrap_or(false)
}

/// The package a managed launcher is for.
fn managed_package(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    if !text
        .lines()
        .any(|l| l.trim() == format!("{MANAGED_KEY}=true"))
    {
        return None;
    }
    text.lines()
        .find_map(|l| l.trim().strip_prefix("X-Cookie-Package="))
        .map(|v| v.trim().to_string())
}

/// Ask the desktop to re-read what we just wrote.
///
/// Both tools are optional. A missing one is reported, not fatal: the files
/// are on disk either way and the next login picks them up.
pub fn refresh(dirs: &DesktopDirs) -> Vec<String> {
    let mut problems = Vec::new();

    match cmd::which("update-mime-database") {
        Some(_) => {
            let path = dirs.mime_root.display().to_string();
            if let Ok(out) = cmd::run("update-mime-database", &[path.as_str()]) {
                if !out.ok() {
                    problems.push(format!("update-mime-database failed: {}", out.message()));
                }
            }
        }
        None => problems.push(
            "update-mime-database is not installed, so the MIME type will not be picked up \
             until something else refreshes the database (usually your next login).\n\
             It comes with shared-mime-info."
                .to_string(),
        ),
    }

    match cmd::which("update-desktop-database") {
        Some(_) => {
            let path = dirs.applications.display().to_string();
            if let Ok(out) = cmd::run("update-desktop-database", &[path.as_str()]) {
                if !out.ok() {
                    problems.push(format!("update-desktop-database failed: {}", out.message()));
                }
            }
        }
        None => problems.push(
            "update-desktop-database is not installed, so file associations may not take \
             effect until your next login.\n\
             It comes with desktop-file-utils."
                .to_string(),
        ),
    }

    // Icon caches are optional; KDE does not need one and regenerates its own.
    if cmd::which("gtk-update-icon-cache").is_some() {
        let hicolor = dirs.icons.join("hicolor");
        if hicolor.is_dir() {
            let path = hicolor.display().to_string();
            let _ = cmd::run("gtk-update-icon-cache", &["-q", "-t", "-f", path.as_str()]);
        }
    }

    problems
}
