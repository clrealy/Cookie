//! `cookie burnt` — the health check.

use anyhow::Result;

use crate::backend;
use crate::cli::BurntArgs;
use crate::health;

/// Returns the process exit code: 1 when something is actually broken.
pub fn run(args: &BurntArgs, backend_name: &str) -> Result<i32> {
    let backend = backend::detect(Some(backend_name))?;
    let report = health::run(backend.as_ref())?;
    health::print(&report, args.all);
    Ok(if report.is_healthy() { 0 } else { 1 })
}
