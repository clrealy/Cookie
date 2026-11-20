//! `cookie jar` — what is in the jar.

use anyhow::Result;

use crate::adopt::{self, Detail};
use crate::backend::{self, AndroidBackend};
use crate::cli::JarArgs;
use crate::desktop::{self, DesktopDirs};
use crate::jar;
use crate::system;
use crate::ui::{self, Level};

pub fn run(args: &JarArgs, backend_name: &str) -> Result<()> {
    let cookies = jar::list()?;

    // Ask the runtime what it actually has. Best effort on purpose: `cookie
    // jar` has always worked with the oven cold, and it should keep doing so.
    let drift = look_for_drift(backend_name, &cookies);

    if args.adopt {
        return adopt_strays(backend_name, drift);
    }

    if cookies.is_empty() {
        println!("The jar is empty.");
        println!();
        println!("  Bake something:  {}", ui::cyan("cookie bake <app.apk>"));
        println!("  Then eat it:     {}", ui::cyan("cookie eat <app.cpk>"));
        return Ok(());
    }

    let name_width = cookies
        .iter()
        .map(|c| c.name().chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let version_width = cookies
        .iter()
        .map(|c| c.manifest.version_display().chars().count())
        .max()
        .unwrap_or(7)
        .max(7);

    ui::heading(&format!(
        "{} cookie{} in the jar",
        cookies.len(),
        if cookies.len() == 1 { "" } else { "s" }
    ));

    let host = system::host_abi();

    for cookie in &cookies {
        let version = cookie.manifest.version_display();
        print!(
            "  {:<name_width$}  {:<version_width$}",
            cookie.name(),
            version,
            name_width = name_width,
            version_width = version_width
        );

        // Flag anything whose native code this host cannot run.
        let foreign = system::foreign_abis(&cookie.manifest.cookie.abis);
        let runs_here = cookie.manifest.cookie.abis.is_empty()
            || cookie.manifest.cookie.abis.iter().any(|a| a == host)
            || foreign.is_empty();
        if runs_here {
            println!("  {}", ui::dim(&cookie.package));
        } else {
            println!(
                "  {}  {}",
                ui::dim(&cookie.package),
                ui::yellow(&format!("needs {}", foreign.join("/")))
            );
        }

        if args.long {
            let abis = if cookie.manifest.cookie.abis.is_empty() {
                "any".to_string()
            } else {
                cookie.manifest.cookie.abis.join(", ")
            };
            println!(
                "{}",
                ui::dim(&format!(
                    "      abis {abis}   installed {}   backend {}",
                    cookie.installed_at(),
                    cookie.record.backend
                ))
            );
        }
    }

    report_drift(&drift);

    println!();
    println!(
        "  {}  {}",
        ui::dim("logs:"),
        ui::cyan("cookie crumbs <name>")
    );
    println!(
        "  {}  {}",
        ui::dim("remove:"),
        ui::cyan("cookie toss <name>")
    );

    Ok(())
}

/// Compare the jar against the runtime, if the runtime can be reached.
fn look_for_drift(backend_name: &str, cookies: &[jar::Cookie]) -> Option<adopt::Reconciliation> {
    let backend = backend::detect(Some(backend_name)).ok()?;
    // Deliberately not `ensure_available()`: its value is the error message,
    // and building that now asks the package manager whether the backend is
    // installable — a second of work for a string this function discards.
    // `status()` answers the only question here, and answers it without
    // running anything when the backend is simply absent.
    if !backend.status().ok()?.state.is_running() {
        return None;
    }
    let installed = backend.list_installed().ok()?;
    Some(adopt::reconcile(cookies, &installed))
}

/// Say what the runtime has that the jar does not, and the other way round.
fn report_drift(drift: &Option<adopt::Reconciliation>) {
    let Some(drift) = drift else {
        return;
    };
    if drift.is_clean() {
        return;
    }

    if !drift.strays.is_empty() {
        ui::heading(&format!(
            "{} app{} the runtime has that Cookie did not install",
            drift.strays.len(),
            if drift.strays.len() == 1 { "" } else { "s" }
        ));
        for app in &drift.strays {
            match &app.name {
                Some(name) => ui::status_line(Level::Info, name, &app.package),
                None => ui::status_line(Level::Info, &app.package, ""),
            }
        }
        ui::note(&ui::dim(
            "These have no launcher, and `cookie crumbs` cannot find them.\n\
             Take them in with:  cookie jar --adopt",
        ));
    }

    if !drift.stale.is_empty() {
        ui::heading(&format!(
            "{} jar entr{} for app{} the runtime no longer has",
            drift.stale.len(),
            if drift.stale.len() == 1 { "y" } else { "ies" },
            if drift.stale.len() == 1 { "" } else { "s" }
        ));
        for package in &drift.stale {
            ui::status_line(Level::Warn, package, "removed outside Cookie");
        }
        ui::note(&ui::dim("Drop one with:  cookie toss <name>"));
    }
}

/// `cookie jar --adopt`
fn adopt_strays(backend_name: &str, drift: Option<adopt::Reconciliation>) -> Result<()> {
    let Some(drift) = drift else {
        ui::warn(
            "Cookie could not ask the runtime what it has, so there is nothing to compare \
             against.\nStart it with:  cookie oven start",
        );
        return Ok(());
    };

    if drift.strays.is_empty() {
        ui::success("Nothing to adopt — the jar already knows about every installed app.");
        return Ok(());
    }

    let backend = backend::detect(Some(backend_name))?;
    let dirs = DesktopDirs::user().ok();
    let mut taken = 0;
    let mut partial = 0;

    for app in &drift.strays {
        match adopt::adopt_into_jar(backend.as_ref(), app) {
            Ok((cookie, detail)) => {
                taken += 1;
                if detail == Detail::NameOnly {
                    partial += 1;
                }
                ui::status_line(
                    Level::Ok,
                    cookie.name(),
                    match detail {
                        Detail::FromApk => "read from its APK",
                        Detail::NameOnly => "name only — its APK could not be read",
                    },
                );
                add_launcher(&dirs, backend.as_ref(), &cookie);
            }
            Err(err) => ui::warn(&format!("could not adopt {}: {err}", app.package)),
        }
    }

    if let Some(dirs) = &dirs {
        desktop::refresh(dirs);
    }

    println!();
    ui::success(&format!(
        "Adopted {taken} app{}.",
        if taken == 1 { "" } else { "s" }
    ));
    if partial > 0 {
        ui::note(&ui::dim(&format!(
            "{partial} of them has only a name and a package id: Cookie could not reach its\n\
             APK, so there is no icon, version or signer recorded. An update check cannot\n\
             compare signatures for those."
        )));
    }
    Ok(())
}

/// Give an adopted app a menu entry, as `cookie eat` would.
fn add_launcher(dirs: &Option<DesktopDirs>, backend: &dyn AndroidBackend, cookie: &jar::Cookie) {
    let Some(dirs) = dirs else { return };
    let class = backend.window_class(&cookie.package);
    if let Err(err) = desktop::install_launcher(dirs, cookie, class.as_deref()) {
        ui::warn(&format!(
            "{} is in the jar, but could not be added to your menu: {err}",
            cookie.name()
        ));
    }
}
