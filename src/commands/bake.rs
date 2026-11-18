//! `cookie bake` — APKs in, one `.cpk` out.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::apk::{self, bundle};
use crate::cli::BakeArgs;
use crate::cpk::pack;
use crate::paths;
use crate::system;
use crate::ui;

pub fn run(args: &BakeArgs) -> Result<()> {
    let inputs = bundle::resolve(&args.inputs)?;

    if inputs.apks.len() == 1 {
        ui::step("reading the APK");
    } else {
        ui::step(&format!("reading {} APKs", inputs.apks.len()));
    }

    let mut infos = Vec::with_capacity(inputs.apks.len());
    for path in &inputs.apks {
        infos.push(apk::inspect(path)?);
    }

    let sorted = pack::sort_apks(infos)?;
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| sorted.base.display_name());

    let output = match &args.output {
        Some(path) => path.clone(),
        None => PathBuf::from(format!("{}.cpk", paths::slugify(&name))),
    };

    if output.exists() && !args.force {
        bail!(
            "{} already exists.\n\
             Overwrite it with --force, or pick another name with --output.",
            output.display()
        );
    }
    if output.is_dir() {
        bail!("{} is a directory, not a file", output.display());
    }

    // Say something early if the app cannot run on this machine, while the
    // user still has the APK in front of them.
    warn_about_abis(&sorted);

    let manifest = pack::bake(&sorted, args.name.as_deref(), &output)?;

    let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);

    println!();
    ui::success(&format!(
        "Baked {} {}",
        ui::bold(&manifest.cookie.name),
        ui::dim(&format!("-> {}", paths::tilde(&output)))
    ));
    ui::note(&format!(
        "package   {}\n\
         version   {}\n\
         abis      {}\n\
         size      {}",
        manifest.cookie.package,
        manifest.version_display(),
        if manifest.cookie.abis.is_empty() {
            "any (no native code)".to_string()
        } else {
            manifest.cookie.abis.join(", ")
        },
        ui::bytes(size)
    ));
    println!();
    println!(
        "  Eat it with:  {}",
        ui::cyan(&format!("cookie eat {}", paths::tilde(&output)))
    );

    Ok(())
}

/// Warn when the APK's native code cannot run on this host.
fn warn_about_abis(sorted: &pack::Sorted) {
    let mut abis: Vec<String> = sorted.base.abis.clone();
    for split in &sorted.splits {
        for abi in &split.abis {
            if !abis.contains(abi) {
                abis.push(abi.clone());
            }
        }
    }
    if abis.is_empty() {
        return;
    }

    let host = system::host_abi();
    if abis.iter().any(|abi| abi == host) {
        return;
    }

    let foreign = system::foreign_abis(&abis);
    if foreign.is_empty() {
        return;
    }

    ui::warn(&format!(
        "this app ships native code for {} but not for {host}.\n\
         It will only run if ARM translation is installed. Check with:\n  cookie burnt",
        foreign.join(", ")
    ));
}
