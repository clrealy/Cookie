//! `cookie kitchen` — desktop integration.

use anyhow::Result;

use crate::backend;
use crate::cli::{KitchenAction, KitchenArgs};
use crate::cpk;
use crate::desktop::{self, DesktopDirs};
use crate::jar;
use crate::paths;
use crate::ui::{self, Level};

pub fn run(args: &KitchenArgs, backend_name: &str) -> Result<i32> {
    let dirs = DesktopDirs::user()?;

    match args.action.as_ref().unwrap_or(&KitchenAction::Setup) {
        KitchenAction::Setup => setup(&dirs, backend_name),
        KitchenAction::Status => Ok(status(&dirs)),
        KitchenAction::Remove(remove_args) => remove(&dirs, remove_args.launchers),
    }
}

fn setup(dirs: &DesktopDirs, backend_name: &str) -> Result<i32> {
    ui::heading("Setting up the kitchen");

    for line in desktop::setup(dirs)? {
        ui::status_line(Level::Ok, &line, "");
    }

    // Give everything already installed a menu entry.
    let cookies = jar::list()?;
    if !cookies.is_empty() {
        // The window class is backend-specific, so ask the backend — but a
        // backend that is not installed must not stop the rest of the setup.
        let window_class = backend::detect(Some(backend_name)).ok();

        let mut made = 0;
        for cookie in &cookies {
            let class = window_class
                .as_ref()
                .and_then(|b| b.window_class(&cookie.package));
            match desktop::install_launcher(dirs, cookie, class.as_deref()) {
                Ok(_) => made += 1,
                Err(err) => ui::warn(&format!(
                    "could not add {} to the menu: {err}",
                    cookie.name()
                )),
            }
        }
        if made > 0 {
            ui::status_line(
                Level::Ok,
                &format!("added {made} app{} to your menu", plural(made)),
                "",
            );
        }
    }

    for problem in desktop::refresh(dirs) {
        ui::status_line(Level::Warn, "database refresh", "");
        ui::fix(&problem);
    }

    println!();
    ui::success("The kitchen is ready.");
    ui::note(
        "Double-clicking a .cpk now runs it, and installed apps appear in your \
         application menu.\n\
         Some desktops only notice new menu entries after you log out and back in.",
    );

    Ok(0)
}

fn status(dirs: &DesktopDirs) -> i32 {
    let status = desktop::status(dirs);

    ui::heading("Kitchen");
    ui::status_line(
        if status.mime_registered {
            Level::Ok
        } else {
            Level::Fail
        },
        "mime type",
        cpk::MIME_TYPE,
    );
    ui::status_line(
        if status.handler_installed {
            Level::Ok
        } else {
            Level::Fail
        },
        "handler",
        &paths::tilde(&dirs.applications.join(desktop::HANDLER_DESKTOP_ID)),
    );
    ui::status_line(
        if status.is_default_handler {
            Level::Ok
        } else {
            Level::Warn
        },
        "opens .cpk files",
        if status.is_default_handler {
            "Cookie"
        } else {
            "not set as the default"
        },
    );
    ui::status_line(
        if status.launchers > 0 {
            Level::Ok
        } else {
            Level::Info
        },
        "menu entries",
        &format!("{} app{}", status.launchers, plural(status.launchers)),
    );

    println!();
    if status.is_ready() {
        ui::success("Double-clicking a cookie will work.");
        0
    } else {
        println!("  Set it up with:  {}", ui::cyan("cookie kitchen setup"));
        1
    }
}

fn remove(dirs: &DesktopDirs, launchers: bool) -> Result<i32> {
    ui::heading("Clearing the kitchen");

    let done = desktop::teardown(dirs, launchers)?;
    if done.is_empty() {
        ui::status_line(Level::Info, "nothing to remove", "");
    }
    for line in done {
        ui::status_line(Level::Ok, &line, "");
    }

    desktop::refresh(dirs);

    println!();
    if launchers {
        ui::success("Desktop integration removed, menu entries included.");
    } else {
        ui::success("Desktop integration removed.");
        ui::note(&ui::dim(
            "App menu entries were left alone. Remove those too with:\n\
             cookie kitchen remove --launchers",
        ));
    }
    Ok(0)
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
