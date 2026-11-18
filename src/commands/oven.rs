//! `cookie oven` — the shared Android runtime.

use anyhow::Result;

use crate::backend::{self, OvenState};
use crate::cli::{OvenAction, OvenArgs};
use crate::health;
use crate::ui::{self, Level};

pub fn run(args: &OvenArgs, backend_name: &str, verbose: bool) -> Result<i32> {
    let backend = backend::detect(Some(backend_name))?;

    match args.action {
        OvenAction::Start => {
            let before = backend.status()?;
            if before.state.is_running() {
                ui::success("The oven is already warm.");
                return Ok(0);
            }
            ui::step("lighting the oven");
            backend.start()?;
            ui::success("The oven is warm.");
            Ok(0)
        }
        OvenAction::Stop => {
            let before = backend.status()?;
            if !before.state.is_running() {
                ui::success("The oven is already off.");
                return Ok(0);
            }
            ui::step("turning the oven off");
            backend.stop()?;
            ui::success("The oven is off.");
            Ok(0)
        }
        OvenAction::Status => {
            let status = backend.status()?;
            let (level, text) = match &status.state {
                OvenState::Running => (Level::Ok, "running".to_string()),
                OvenState::Stopped => (Level::Warn, "stopped".to_string()),
                OvenState::NotInitialized => {
                    (Level::Fail, "the Android image is not set up".to_string())
                }
                OvenState::NotInstalled => {
                    (Level::Fail, format!("{} is not installed", backend.name()))
                }
                OvenState::Unknown(what) => (Level::Warn, format!("unclear: {what}")),
            };

            ui::heading("Oven");
            ui::status_line(level, "state", &text);
            ui::status_line(Level::Info, "backend", backend.name());
            if let Some(running) = status.container_running {
                ui::status_line(
                    if running { Level::Ok } else { Level::Warn },
                    "container service",
                    if running { "running" } else { "not running" },
                );
            }
            if let Some(vendor) = &status.vendor_type {
                ui::status_line(Level::Info, "vendor type", vendor);
            }
            if let Some(ip) = &status.ip_address {
                ui::status_line(Level::Info, "ip address", ip);
            }
            ui::status_line(Level::Info, "host", &health::init_summary());

            if verbose && !status.raw.trim().is_empty() {
                ui::heading("Raw backend output");
                ui::note(status.raw.trim());
            }

            if !status.state.is_running() {
                println!();
                println!("  Start it with:  {}", ui::cyan("cookie oven start"));
                println!("  Full check:     {}", ui::cyan("cookie burnt"));
                return Ok(1);
            }
            Ok(0)
        }
    }
}
