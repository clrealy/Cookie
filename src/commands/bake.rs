//! `cookie bake` — APKs in, one `.cpk` out.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::apk::{self, bundle};
use crate::cli::BakeArgs;
use crate::cpk::manifest::CpkManifest;
use crate::cpk::pack;
use crate::jar;
use crate::paths;
use crate::system;
use crate::ui;
use crate::update::{self, Decision};

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
    report_against_installed(&manifest, &output);

    Ok(())
}

/// Say what this cookie would do to the app already installed, if any.
///
/// Worth saying at bake time rather than leaving it for `eat`: the user has
/// the APK in front of them now, and a signing-key mismatch is much easier to
/// make sense of while they still remember where the file came from.
fn report_against_installed(manifest: &CpkManifest, output: &std::path::Path) {
    let eat = format!("cookie eat {}", paths::tilde(output));

    // A jar we cannot read is not worth failing a successful bake over.
    let installed = jar::get(&manifest.cookie.package).unwrap_or_default();

    let decision = update::decide(installed.as_ref(), manifest);

    match &decision {
        Decision::Fresh => {
            println!();
            println!("  Eat it with:  {}", ui::cyan(&eat));
        }

        Decision::Update { from, to } => {
            println!();
            ui::status_line(
                ui::Level::Ok,
                &format!("updates the installed {}", manifest.cookie.name),
                &format!("{from} -> {to}, keeping its data"),
            );
            println!();
            println!("  Eat it with:  {}", ui::cyan(&eat));
        }

        Decision::Reinstall { version } => {
            println!();
            ui::status_line(
                ui::Level::Info,
                &format!("{} {version} is already installed", manifest.cookie.name),
                "eating this will reinstall it, keeping its data",
            );
            println!();
            println!("  Eat it with:  {}", ui::cyan(&eat));
        }

        Decision::Downgrade { from, to } => {
            println!();
            ui::status_line(
                ui::Level::Warn,
                &format!("older than the installed {}", manifest.cookie.name),
                &format!("{from} is installed, this is {to}"),
            );
            ui::fix(&format!(
                "Android will not downgrade an app by default. To do it anyway:\n  \
                 {eat} --allow-downgrade"
            ));
        }

        Decision::SignerMismatch {
            installed: old,
            incoming: new,
        } => {
            println!();
            ui::status_line(
                ui::Level::Fail,
                "signed by a different key",
                &format!(
                    "{} is already installed and was signed by someone else",
                    manifest.cookie.name
                ),
            );
            ui::note(&update::signer_mismatch_message(
                &manifest.cookie.name,
                &manifest.cookie.package,
                old,
                new,
            ));
        }

        Decision::SignerUnknown { reason } => {
            println!();
            ui::status_line(
                ui::Level::Warn,
                &format!(
                    "cannot be checked against the installed {}",
                    manifest.cookie.name
                ),
                "",
            );
            ui::note(&update::signer_unknown_message(
                &manifest.cookie.name,
                *reason,
            ));
            println!();
            println!("  Eat it with:  {}", ui::cyan(&eat));
        }
    }
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
