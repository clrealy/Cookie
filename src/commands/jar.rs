//! `cookie jar` — what is in the jar.

use anyhow::Result;

use crate::cli::JarArgs;
use crate::jar;
use crate::system;
use crate::ui;

pub fn run(args: &JarArgs) -> Result<()> {
    let cookies = jar::list()?;

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
