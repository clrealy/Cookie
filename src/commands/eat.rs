//! `cookie eat` — install if needed, then run.

use anyhow::{bail, Context, Result};

use crate::backend::{self, OvenState};
use crate::cli::EatArgs;
use crate::cpk::unpack;
use crate::jar;
use crate::paths::{self, ScratchDir};
use crate::ui;

pub fn run(args: &EatArgs, backend_name: &str) -> Result<()> {
    if !args.cpk.exists() {
        bail!("{} does not exist", args.cpk.display());
    }

    let manifest = unpack::read_manifest(&args.cpk)?;
    let package = manifest.cookie.package.clone();

    let backend = backend::detect(Some(backend_name))?;
    backend.ensure_available()?;

    // Double-clicking a .cpk should just work, so warm the oven if it is cold.
    let status = backend.status()?;
    if !status.state.is_running() {
        match status.state {
            OvenState::NotInitialized | OvenState::NotInstalled => {
                // `start` explains these properly.
                backend.start()?;
            }
            _ => {
                ui::step("the oven is cold, lighting it");
                backend.start()?;
            }
        }
    }

    // One window per app is the entire point of Cookie.
    backend.ensure_multi_window()?;

    let already_installed = backend.is_installed(&package)?;
    let in_jar = jar::get(&package)?.is_some();

    if already_installed && !args.reinstall {
        ui::step(&format!("{} is already installed", manifest.cookie.name));
    } else {
        ui::step(&format!("installing {}", manifest.cookie.name));
        let scratch = ScratchDir::new("eat")?;
        let apks = unpack::extract_apks(&args.cpk, &manifest, scratch.path())?;
        backend
            .install(&apks)
            .with_context(|| format!("could not install {}", manifest.cookie.name))?;
    }

    // Record it in the jar even if the runtime already had it: that is how an
    // app installed by other means gets adopted.
    if !in_jar || args.reinstall || !already_installed {
        let icon = unpack::read_icon(&args.cpk, &manifest)?;
        let dir = jar::put(&manifest, icon.as_deref(), Some(&args.cpk), backend.name())?;
        if args.reinstall || !in_jar {
            ui::note(&ui::dim(&format!("jar entry: {}", paths::tilde(&dir))));
        }
    }

    if args.no_launch {
        ui::success(&format!(
            "{} is installed. Run it with: cookie eat {}",
            manifest.cookie.name,
            paths::tilde(&args.cpk)
        ));
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
