//! `cookie toss` — uninstall.

use std::io::{IsTerminal, Write};

use anyhow::{bail, Result};

use crate::backend;
use crate::cli::TossArgs;
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

    let backend = backend::detect(Some(backend_name))?;
    backend.ensure_available()?;

    let status = backend.status()?;
    if status.state.is_running() {
        ui::step(&format!("removing {} from the oven", cookie.name()));
        backend.uninstall(&cookie.package)?;
    } else {
        // The jar entry can still go; the app itself will have to wait.
        ui::warn(
            "the oven is not running, so the app itself could not be removed from Android.\n\
             Its jar entry is gone. Start the oven and toss it again to finish the job:\n  \
             cookie oven start",
        );
    }

    jar::remove(&cookie.package)?;
    ui::success(&format!("Tossed {}.", cookie.name()));

    Ok(())
}
