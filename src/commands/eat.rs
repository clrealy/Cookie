//! `cookie eat` — install if needed, then run.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::backend::{self, AndroidBackend, OvenState};
use crate::cli::EatArgs;
use crate::cpk::{manifest::CpkManifest, unpack};
use crate::desktop::{self, DesktopDirs};
use crate::jar::{self, Cookie};
use crate::paths::{self, ScratchDir};
use crate::ui;

/// What the user asked for.
#[derive(Debug)]
enum Target {
    /// A `.cpk` on disk: install it, then run it.
    File(PathBuf),
    /// Something already in the jar: just run it.
    Installed(Box<Cookie>),
}

/// Work out whether the argument names a file or an installed app.
///
/// A path that exists, or anything ending in `.cpk`, is treated as a file —
/// so a mistyped path reports "no such file" rather than "no such cookie".
/// Anything else is looked up in the jar, which is what makes
/// `cookie eat Signal` and the generated menu entries work.
fn resolve_target(argument: &str) -> Result<Target> {
    let as_path = Path::new(argument);
    let looks_like_a_file = as_path.exists()
        || argument
            .rsplit_once('.')
            .map(|(_, ext)| ext.eq_ignore_ascii_case("cpk"))
            .unwrap_or(false)
        || argument.contains('/');

    if looks_like_a_file {
        if !as_path.exists() {
            bail!("{} does not exist", as_path.display());
        }
        if as_path.is_dir() {
            bail!("{} is a directory, not a cookie", as_path.display());
        }
        return Ok(Target::File(as_path.to_path_buf()));
    }

    Ok(Target::Installed(Box::new(jar::resolve(argument)?)))
}

pub fn run(args: &EatArgs, backend_name: &str) -> Result<()> {
    let target = resolve_target(&args.target)?;

    let backend = backend::detect(Some(backend_name))?;
    backend.ensure_available()?;

    warm_the_oven(backend.as_ref())?;
    // One window per app is the entire point of Cookie.
    backend.ensure_multi_window()?;

    let (manifest, source) = match &target {
        Target::File(path) => (unpack::read_manifest(path)?, Some(path.clone())),
        Target::Installed(cookie) => (cookie.manifest.clone(), None),
    };
    let package = manifest.cookie.package.clone();

    install_if_needed(backend.as_ref(), args, &manifest, source.as_deref())?;
    record_in_jar(backend.as_ref(), args, &manifest, source.as_deref())?;

    if !args.no_launcher {
        add_to_the_menu(backend.as_ref(), &package)?;
    }

    if args.no_launch {
        ui::success(&format!("{} is installed.", manifest.cookie.name));
        ui::note(&ui::dim(&format!(
            "run it with: cookie eat {}",
            manifest.cookie.name
        )));
        return Ok(());
    }

    ui::step(&format!("launching {}", manifest.cookie.name));
    backend.launch(&package)?;
    ui::success(&format!("{} is running", manifest.cookie.name));
    ui::note(&ui::dim(&format!(
        "logs: cookie crumbs {}",
        manifest.cookie.name
    )));

    Ok(())
}

/// Double-clicking a cookie should just work, so light the oven if it is cold.
fn warm_the_oven(backend: &dyn AndroidBackend) -> Result<()> {
    let status = backend.status()?;
    if status.state.is_running() {
        return Ok(());
    }
    if !matches!(
        status.state,
        OvenState::NotInitialized | OvenState::NotInstalled
    ) {
        ui::step("the oven is cold, lighting it");
    }
    // `start` explains the not-initialised and not-installed cases properly.
    backend.start()
}

fn install_if_needed(
    backend: &dyn AndroidBackend,
    args: &EatArgs,
    manifest: &CpkManifest,
    source: Option<&Path>,
) -> Result<()> {
    let package = &manifest.cookie.package;
    let already = backend.is_installed(package)?;

    if already && !args.reinstall {
        return Ok(());
    }

    let Some(cpk) = source else {
        // Running something from the jar that the runtime no longer has.
        bail!(
            "{} is in the jar, but the oven does not have it installed.\n\
             Install it again from its cookie:\n  cookie eat <file.cpk>\n\
             Or drop the stale entry:\n  cookie toss {}",
            manifest.cookie.name,
            manifest.cookie.name
        );
    };

    ui::step(&format!("installing {}", manifest.cookie.name));
    let scratch = ScratchDir::new("eat")?;
    let apks = unpack::extract_apks(cpk, manifest, scratch.path())?;
    backend
        .install(&apks)
        .with_context(|| format!("could not install {}", manifest.cookie.name))
}

/// Keep the jar in step with what the runtime has.
fn record_in_jar(
    backend: &dyn AndroidBackend,
    args: &EatArgs,
    manifest: &CpkManifest,
    source: Option<&Path>,
) -> Result<()> {
    let package = &manifest.cookie.package;
    let already_recorded = jar::get(package)?.is_some();

    // Nothing changed and the jar already knows: leave the install date alone.
    if already_recorded && !args.reinstall && source.is_none() {
        return Ok(());
    }

    let icon = match source {
        Some(cpk) => unpack::read_icon(cpk, manifest)?,
        None => None,
    };

    let dir = jar::put(manifest, icon.as_deref(), source, backend.name())?;
    if !already_recorded {
        ui::note(&ui::dim(&format!("jar entry: {}", paths::tilde(&dir))));
    }
    Ok(())
}

/// Give the app a menu entry with its own icon.
///
/// A failure here is cosmetic — the app is installed and will run — so it
/// warns rather than aborting.
fn add_to_the_menu(backend: &dyn AndroidBackend, package: &str) -> Result<()> {
    let Some(cookie) = jar::get(package)? else {
        return Ok(());
    };

    let dirs = match DesktopDirs::user() {
        Ok(dirs) => dirs,
        Err(err) => {
            ui::warn(&format!("could not work out where launchers go: {err}"));
            return Ok(());
        }
    };

    match desktop::install_launcher(&dirs, &cookie, backend.window_class(package).as_deref()) {
        Ok(path) => {
            ui::note(&ui::dim(&format!("menu entry: {}", paths::tilde(&path))));
            desktop::refresh(&dirs);
        }
        Err(err) => ui::warn(&format!(
            "{} is installed, but it could not be added to your application menu:\n{err}",
            cookie.name()
        )),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_path_is_reported_as_a_path() {
        let err = resolve_target("/nope/missing.cpk").unwrap_err().to_string();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn anything_with_a_slash_is_treated_as_a_path() {
        let err = resolve_target("./nope").unwrap_err().to_string();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn a_bare_name_is_looked_up_in_the_jar() {
        // With no jar this reports a jar problem, not a file problem, which
        // is the distinction that matters.
        let err = resolve_target("Signal").unwrap_err().to_string();
        assert!(
            err.contains("jar") || err.contains("no cookie called"),
            "{err}"
        );
    }
}
