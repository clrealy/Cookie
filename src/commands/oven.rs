//! `cookie oven` — the shared Android runtime.

use anyhow::Result;

use crate::backend::oven::{Oven, OvenDirs};
use crate::backend::{self, OvenState};
use crate::cli::{
    OvenAction, OvenArgs, OvenImageAction, OvenImageArgs, OvenPropAction, OvenPropArgs,
};
use crate::health;
use crate::paths;
use crate::ui::{self, Level};

pub fn run(args: &OvenArgs, backend_name: &str, verbose: bool) -> Result<i32> {
    let backend = backend::detect(Some(backend_name))?;

    match &args.action {
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
        OvenAction::Image(image_args) => image(image_args),

        OvenAction::Recipe => recipe(),

        OvenAction::Prop(prop_args) => prop(prop_args),

        OvenAction::Status => {
            let status = backend.status()?;
            let (level, text) = match &status.state {
                OvenState::Running => (Level::Ok, "running".to_string()),
                OvenState::Stopped => (Level::Warn, "stopped".to_string()),
                OvenState::NotInitialized => {
                    (Level::Fail, "the Android image is not set up".to_string())
                }
                OvenState::NotInstalled => (
                    Level::Fail,
                    // Backends say what is actually missing; for the oven
                    // that is LXC, not the oven itself.
                    if status.raw.trim().is_empty() {
                        format!("{} is not installed", backend.name())
                    } else {
                        status.raw.trim().to_string()
                    },
                ),
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
                // Keep the backend in the suggested commands, or following
                // them lands the user on a different backend than the one
                // they just asked about.
                let suffix = if backend_name == "waydroid" || backend_name == "auto" {
                    String::new()
                } else {
                    format!(" --backend {backend_name}")
                };
                println!();
                println!(
                    "  Start it with:  {}",
                    ui::cyan(&format!("cookie oven start{suffix}"))
                );
                println!(
                    "  Full check:     {}",
                    ui::cyan(&format!("cookie burnt{suffix}"))
                );
                return Ok(1);
            }
            Ok(0)
        }
    }
}

/// `cookie oven image` — the shared Android system image.
fn image(args: &OvenImageArgs) -> Result<i32> {
    let oven = Oven::with_dirs(OvenDirs::user()?);

    match args.action.as_ref().unwrap_or(&OvenImageAction::Show) {
        OvenImageAction::Add(add) => {
            ui::step(&format!("checking {}", add.path.display()));
            let image = oven.add_image(&add.path)?;
            ui::success("Registered the system image.");
            ui::note(&format!(
                "path    {}\nformat  {}\nsize    {}",
                paths::tilde(&image.path),
                image.format.label(),
                ui::bytes(image.size)
            ));
            println!();
            println!(
                "  Light the oven with:  {}",
                ui::cyan("cookie oven start --backend oven")
            );
            Ok(0)
        }

        OvenImageAction::Remove => {
            if oven.remove_image()? {
                ui::success("Forgot the registered system image.");
                ui::note(&ui::dim("The file itself was left alone."));
            } else {
                println!("No system image was registered.");
            }
            Ok(0)
        }

        OvenImageAction::Show => {
            let record = oven.record()?;
            match record.image {
                Some(image) if image.is_present() => {
                    ui::heading("System image");
                    ui::status_line(Level::Ok, "path", &paths::tilde(&image.path));
                    ui::status_line(Level::Info, "format", image.format.label());
                    ui::status_line(Level::Info, "size", &ui::bytes(image.size));
                    Ok(0)
                }
                Some(image) => {
                    ui::heading("System image");
                    ui::status_line(
                        Level::Fail,
                        "path",
                        &format!("{} (missing or changed)", paths::tilde(&image.path)),
                    );
                    println!();
                    println!(
                        "  Register it again:  {}",
                        ui::cyan("cookie oven image add <path>")
                    );
                    Ok(1)
                }
                None => {
                    println!("No system image is registered.");
                    println!();
                    println!("  Cookie does not download one. Supply a LineageOS-derived");
                    println!("  system image and register it:");
                    println!(
                        "    {}",
                        ui::cyan("cookie oven image add /path/to/system.img")
                    );
                    Ok(1)
                }
            }
        }
    }
}

/// `cookie oven recipe` — the container configuration, as it would be written.
///
/// Printed to stdout on its own so it can be redirected or diffed. Anything
/// about where it came from goes to stderr.
fn recipe() -> Result<i32> {
    let oven = Oven::with_dirs(OvenDirs::user()?);
    let config = oven.container_config()?;

    eprintln!(
        "{}",
        ui::dim(&format!(
            "# built for this machine; `cookie oven start --backend oven` writes it to {}",
            paths::tilde(&oven.dirs().config)
        ))
    );
    print!("{}", config.render());
    Ok(0)
}

/// `cookie oven prop` — the Android properties the oven hands to the image.
fn prop(args: &OvenPropArgs) -> Result<i32> {
    let oven = Oven::with_dirs(OvenDirs::user()?);

    match args.action.as_ref().unwrap_or(&OvenPropAction::List) {
        OvenPropAction::Set(set) => {
            oven.set_property(&set.key, &set.value)?;
            ui::success(&format!("{} = {}", set.key, set.value));
            ui::note(&ui::dim(
                "Properties starting with ro. are fixed while Android boots, so this takes\n\
                 effect the next time the oven starts.",
            ));
            Ok(0)
        }

        OvenPropAction::Unset(unset) => {
            if oven.unset_property(&unset.key)? {
                ui::success(&format!("{} is back to Cookie's own answer", unset.key));
            } else {
                println!("{} was not overridden.", unset.key);
            }
            Ok(0)
        }

        OvenPropAction::List => {
            let props = oven.properties()?;
            let overrides = oven.record()?.properties;

            ui::heading("Android properties");
            let width = props.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
            for (key, value) in props.iter() {
                let source = if overrides.get(key).is_some() {
                    ui::yellow("set by you")
                } else {
                    ui::dim("worked out")
                };
                println!("  {key:<width$}  {value}  {source}");
            }

            println!();
            println!(
                "  {}  {}",
                ui::dim("override:"),
                ui::cyan("cookie oven prop set <name> <value>")
            );
            Ok(0)
        }
    }
}
