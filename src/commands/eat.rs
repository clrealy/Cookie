//! `cookie eat` — install if needed, then run.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::backend::{self, AndroidBackend, InstallOptions, OvenState};
use crate::cli::EatArgs;
use crate::cpk::{manifest::CpkManifest, unpack};
use crate::desktop::{self, DesktopDirs};
use crate::jar::{self, Cookie};
use crate::paths::{self, ScratchDir};
use crate::ui;
use crate::update::{self, Decision};

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

    let (manifest, source) = match &target {
        Target::File(path) => (unpack::read_manifest(path)?, Some(path.clone())),
        Target::Installed(cookie) => (cookie.manifest.clone(), None),
    };
    let package = manifest.cookie.package.clone();

    // Compare signing keys before anything else happens.
    //
    // This needs only the jar and the cookie, not the runtime, so a cookie
    // signed by the wrong key is refused without first starting a whole
    // Android container — and, more importantly, before `--replace` is
    // allowed to delete anything.
    let recorded = jar::get(&package)?;
    let decision = update::decide(recorded.as_ref(), &manifest);
    if let Decision::SignerMismatch {
        installed,
        incoming,
    } = &decision
    {
        if !args.replace {
            bail!(
                "{}",
                update::signer_mismatch_message(
                    &manifest.cookie.name,
                    &package,
                    installed,
                    incoming
                )
            );
        }
    }

    let backend = backend::detect(Some(backend_name))?;
    backend.ensure_available()?;

    warm_the_oven(backend.as_ref())?;
    // One window per app is the entire point of Cookie.
    backend.ensure_multi_window()?;

    install_if_needed(
        backend.as_ref(),
        args,
        &manifest,
        source.as_deref(),
        &decision,
    )?;
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

/// Install the app, acting on the decision made before the oven was lit.
///
/// Android ties an app's data to the key that signed it, so installing over
/// the top — which keeps that data — only happens once the keys are known to
/// match.
fn install_if_needed(
    backend: &dyn AndroidBackend,
    args: &EatArgs,
    manifest: &CpkManifest,
    source: Option<&Path>,
    decision: &Decision,
) -> Result<()> {
    let package = &manifest.cookie.package;
    let name = &manifest.cookie.name;
    let already_in_android = backend.is_installed(package)?;

    // Nothing to do: it is installed, the jar agrees, and the user is just
    // launching it.
    if already_in_android && !args.reinstall && source.is_none() {
        return Ok(());
    }

    let Some(cpk) = source else {
        bail!(
            "{name} is in the jar, but the oven does not have it installed.\n\
             Install it again from its cookie:\n  cookie eat <file.cpk>\n\
             Or drop the stale entry:\n  cookie toss {name}"
        );
    };

    // A jar entry for something Android no longer has describes nothing, so
    // it must not make this look like an update.
    let effective = if already_in_android {
        decision.clone()
    } else {
        Decision::Fresh
    };

    let options = resolve_decision(&effective, args, manifest, backend)?;

    if matches!(effective, Decision::Fresh) && !already_in_android {
        ui::step(&format!("installing {name}"));
    }

    let scratch = ScratchDir::new("eat")?;
    let apks = unpack::extract_apks(cpk, manifest, scratch.path())?;
    backend
        .install(&apks, &options)
        .with_context(|| format!("could not install {name}"))
}

/// Turn a decision into install options, refusing the ones that need a word
/// from the user first.
fn resolve_decision(
    decision: &Decision,
    args: &EatArgs,
    manifest: &CpkManifest,
    backend: &dyn AndroidBackend,
) -> Result<InstallOptions> {
    let name = &manifest.cookie.name;
    let package = &manifest.cookie.package;

    match decision {
        Decision::Fresh => Ok(InstallOptions::fresh()),

        Decision::Update { from, to } => {
            ui::step(&format!("updating {name} from {from} to {to}"));
            ui::note(&ui::dim("same signing key, so the app's data is kept"));
            Ok(InstallOptions::update())
        }

        Decision::Reinstall { version } => {
            ui::step(&format!("reinstalling {name} {version}"));
            ui::note(&ui::dim("same signing key, so the app's data is kept"));
            Ok(InstallOptions::update())
        }

        Decision::Downgrade { from, to } => {
            if !args.allow_downgrade {
                bail!(
                    "this cookie is {to}, older than the installed {from}.\n\
                     Android does not downgrade an app by default: the older build may not \
                     understand data the newer one wrote, which can leave the app unable to \
                     start.\n\
                     \n\
                     To do it anyway, keeping the existing data:\n  \
                     cookie eat <file.cpk> --allow-downgrade\n\
                     \n\
                     Or start the app clean, which deletes its data:\n  \
                     cookie toss {name} && cookie eat <file.cpk>"
                );
            }
            ui::warn(&format!(
                "downgrading {name} from {from} to {to}.\n\
                 The app keeps its data, which the older build may not be able to read."
            ));
            Ok(InstallOptions {
                keep_data: true,
                allow_downgrade: true,
            })
        }

        Decision::SignerMismatch {
            installed,
            incoming,
        } => {
            // `run` already refused this unless --replace was given, but the
            // check is repeated rather than assumed: this is the branch where
            // getting it wrong deletes someone's data.
            if !args.replace {
                bail!(
                    "{}",
                    update::signer_mismatch_message(name, package, installed, incoming)
                );
            }
            ui::warn(&format!(
                "replacing {name} with a build signed by a different key.\n\
                 The installed app and all of its data are being deleted first — this cannot \
                 be undone."
            ));
            // The old app has to go before the new one can take its place:
            // Android refuses to install over a different signature.
            ui::step(&format!("removing the installed {name}"));
            backend.uninstall(package)?;
            Ok(InstallOptions::fresh())
        }

        Decision::SignerUnknown { reason } => {
            ui::warn(&update::signer_unknown_message(name, *reason));
            Ok(InstallOptions::update())
        }
    }
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
