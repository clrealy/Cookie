//! `cookie toss` — uninstall.

use std::io::{IsTerminal, Write};

use anyhow::{bail, Result};

use crate::backend;
use crate::cli::TossArgs;
use crate::desktop::{self, DesktopDirs};
use crate::jar;
use crate::ui;

pub fn run(args: &TossArgs, backend_name: &str) -> Result<()> {
    let cookie = jar::resolve(&args.name)?;

    if !args.yes {
        if !std::io::stdin().is_terminal() {
            bail!(
                "refusing to toss {} without a confirmation.\n\
                 Re-run with --yes if you meant it.",
                cookie.name()
            );
        }
        print!(
            "Toss {} ({})? Its app data goes too. [y/N] ",
            ui::bold(cookie.name()),
            cookie.package
        );
        std::io::stdout().flush().ok();

        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim().to_lowercase();
        if answer != "y" && answer != "yes" {
            println!("Left it in the jar.");
            return Ok(());
        }
    }

    // Removing the app from Android needs a working backend; removing it from
    // the jar and the menu does not. A missing or cold runtime must not leave
    // the user stuck with an entry they cannot get rid of.
    match remove_from_the_oven(&cookie, backend_name) {
        Ok(true) => {}
        Ok(false) => ui::warn(&format!(
            "the oven is not running, so {} is still installed inside Android.\n\
             Its jar entry and menu entry are gone. To finish the job:\n  \
             cookie oven start && cookie toss {} --yes",
            cookie.name(),
            cookie.package
        )),
        Err(err) => ui::warn(&format!(
            "{} could not be removed from Android:\n{err}\n\
             Its jar entry and menu entry are gone anyway.",
            cookie.name()
        )),
    }

    // Take the menu entry and its icon with it. A failure here is cosmetic.
    match DesktopDirs::user() {
        Ok(dirs) => match desktop::remove_launcher(&dirs, &cookie.package) {
            Ok(true) => {
                desktop::refresh(&dirs);
            }
            Ok(false) => {}
            Err(err) => ui::warn(&format!(
                "could not remove the application-menu entry: {err}"
            )),
        },
        Err(err) => ui::warn(&format!("could not work out where launchers live: {err}")),
    }

    jar::remove(&cookie.package)?;
    ui::success(&format!("Tossed {}.", cookie.name()));

    Ok(())
}

/// Take the app out of the Android runtime.
///
/// `Ok(false)` means the runtime is not running, which is a warning rather
/// than a failure: the rest of the removal still goes ahead.
fn remove_from_the_oven(cookie: &jar::Cookie, backend_name: &str) -> Result<bool> {
    let backend = backend::detect(Some(backend_name))?;
    backend.ensure_available()?;

    if !backend.status()?.state.is_running() {
        return Ok(false);
    }

    ui::step(&format!("removing {} from the oven", cookie.name()));
    backend.uninstall(&cookie.package)?;
    Ok(true)
}
