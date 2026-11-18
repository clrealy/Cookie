//! `cookie crumbs` — the logs for one app.

use anyhow::Result;

use crate::backend::{self, LogSpec};
use crate::cli::CrumbsArgs;
use crate::jar;
use crate::ui;

pub fn run(args: &CrumbsArgs, backend_name: &str) -> Result<()> {
    let cookie = jar::resolve(&args.name)?;
    let backend = backend::detect(Some(backend_name))?;
    backend.ensure_available()?;

    let status = backend.status()?;
    if !status.state.is_running() {
        ui::warn(
            "the oven is not running, so there are no live logs.\n\
             Start it with:  cookie oven start",
        );
    }

    // Filtering by process id is exact. It only works while the app is
    // running, so fall back to matching the package name in the log text.
    let pid = backend.pid_of(&cookie.package).unwrap_or(None);

    // This goes to stderr so `cookie crumbs app > log.txt` stays clean.
    match pid {
        Some(pid) => eprintln!(
            "{}",
            ui::dim(&format!(
                "crumbs from {} (pid {pid}){}",
                cookie.name(),
                if args.follow { ", following" } else { "" }
            ))
        ),
        None => eprintln!(
            "{}",
            ui::dim(&format!(
                "{} is not running, so these crumbs are matched on `{}` in the log text{}",
                cookie.name(),
                cookie.package,
                if args.follow { ", following" } else { "" }
            ))
        ),
    }

    let spec = LogSpec {
        package: cookie.package.clone(),
        pid,
        follow: args.follow,
        lines: args.lines.max(1),
    };

    backend.logs(&spec)
}
