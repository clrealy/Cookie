//! The `cookie` binary.

use std::process::ExitCode;

use clap::Parser;

use cookie::cli::Cli;
use cookie::{notify, ui};

fn main() -> ExitCode {
    let cli = Cli::parse();
    ui::init(cli.global.no_color);
    let verbose = cli.global.verbose;

    match cookie::run(cli) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(err) => {
            report(&err, verbose);
            ExitCode::FAILURE
        }
    }
}

/// Print an error the way a person would want to read it: the problem first,
/// then what it was caused by, indented, one per line.
///
/// A cookie launched from the application menu has no terminal attached, so
/// the same message also goes to a desktop notification. Without that, a
/// double-click that fails would simply do nothing at all.
fn report(err: &anyhow::Error, verbose: bool) {
    if !notify::has_terminal() {
        let detail = err
            .chain()
            .map(|cause| cause.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        notify::error("Cookie could not run that app", &detail);
    }

    eprintln!("{} {}", ui::red("error:"), first_line(&err.to_string()));

    let rest = remaining_lines(&err.to_string());
    if !rest.is_empty() {
        for line in rest.lines() {
            eprintln!("  {line}");
        }
    }

    for cause in err.chain().skip(1) {
        eprintln!(
            "{} {}",
            ui::dim("caused by:"),
            first_line(&cause.to_string())
        );
        let rest = remaining_lines(&cause.to_string());
        for line in rest.lines() {
            eprintln!("  {line}");
        }
    }

    if verbose {
        eprintln!("\n{}", ui::dim(&format!("{err:?}")));
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

fn remaining_lines(text: &str) -> String {
    let mut lines = text.lines();
    lines.next();
    lines.collect::<Vec<_>>().join("\n")
}
