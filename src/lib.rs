//! Cookie: run Android apps on Linux like they belong there.
//!
//! Wine runs Windows programs, Darling runs macOS programs, and both sound
//! like something a grandma would offer you. Grandmas give you cookies.
//!
//! The pieces:
//!
//! - [`apk`] reads APK files, including Android's compiled binary XML.
//! - [`cpk`] is the Cookie Package format: one app, ready to install.
//! - [`backend`] is the Android runtime behind it all, behind a trait so the
//!   Waydroid backend can be swapped for Cookie's own oven later.
//! - [`jar`] remembers what is installed.
//! - [`health`] is `cookie burnt`.

pub mod apk;
pub mod assets;
pub mod backend;
pub mod cli;
pub mod commands;
pub mod cpk;
pub mod desktop;
pub mod health;
pub mod jar;
pub mod notify;
pub mod paths;
pub mod system;
pub mod ui;
pub mod util;

use anyhow::Result;

use cli::{Cli, Command};

/// Run a parsed command line. Returns the process exit code.
pub fn run(cli: Cli) -> Result<i32> {
    let backend_name = cli.global.backend.as_str();

    match &cli.command {
        Command::Bake(args) => {
            commands::bake::run(args)?;
            Ok(0)
        }
        Command::Eat(args) => {
            commands::eat::run(args, backend_name)?;
            Ok(0)
        }
        Command::Jar(args) => {
            commands::jar::run(args)?;
            Ok(0)
        }
        Command::Crumbs(args) => {
            commands::crumbs::run(args, backend_name)?;
            Ok(0)
        }
        Command::Burnt(args) => commands::burnt::run(args, backend_name),
        Command::Oven(args) => commands::oven::run(args, backend_name, cli.global.verbose),
        Command::Toss(args) => {
            commands::toss::run(args, backend_name)?;
            Ok(0)
        }
        Command::Kitchen(args) => commands::kitchen::run(args, backend_name),
    }
}
