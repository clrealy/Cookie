//! Terminal output: colour when it helps, plain text when it would not.
//!
//! No dependency here. We respect `NO_COLOR`, `CLICOLOR_FORCE` and whether
//! stdout is actually a terminal, which is all a CLI really needs.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(false);

/// Work out once whether to colourise, honouring an explicit `--no-color`.
pub fn init(force_off: bool) {
    // NO_COLOR and --no-color both mean the same thing, and both beat
    // CLICOLOR_FORCE: an explicit "off" wins over an explicit "on".
    let on = if force_off || std::env::var_os("NO_COLOR").is_some() {
        false
    } else if std::env::var_os("CLICOLOR_FORCE").is_some() {
        true
    } else {
        std::io::stdout().is_terminal()
    };
    COLOR.store(on, Ordering::Relaxed);
}

fn color_on() -> bool {
    COLOR.load(Ordering::Relaxed)
}

macro_rules! style {
    ($name:ident, $code:literal) => {
        pub fn $name(text: &str) -> String {
            if color_on() {
                format!("\x1b[{}m{}\x1b[0m", $code, text)
            } else {
                text.to_string()
            }
        }
    };
}

style!(bold, "1");
style!(dim, "2");
style!(red, "31");
style!(green, "32");
style!(yellow, "33");
style!(blue, "34");
style!(magenta, "35");
style!(cyan, "36");

/// Outcome of a single health check, and the shape of most status output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
    Info,
}

impl Level {
    pub fn symbol(self) -> String {
        match self {
            Level::Ok => green("✔"),
            Level::Warn => yellow("!"),
            Level::Fail => red("✘"),
            Level::Info => blue("·"),
        }
    }
}

/// `✔ label — detail`
pub fn status_line(level: Level, label: &str, detail: &str) {
    if detail.is_empty() {
        println!("  {} {}", level.symbol(), label);
    } else {
        println!("  {} {}  {}", level.symbol(), label, dim(detail));
    }
}

/// A short indented note under a status line.
pub fn note(text: &str) {
    for line in text.lines() {
        if line.is_empty() {
            // Indenting a blank line just leaves trailing whitespace.
            println!();
        } else {
            println!("      {line}");
        }
    }
}

/// A fix suggestion under a failing check.
pub fn fix(text: &str) {
    let mut first = true;
    for line in text.lines() {
        if first {
            println!("      {} {line}", cyan("fix:"));
            first = false;
        } else {
            println!("           {line}");
        }
    }
}

pub fn heading(text: &str) {
    println!("\n{}", bold(text));
}

/// A step in progress, e.g. during `bake`.
pub fn step(text: &str) {
    println!("{} {}", magenta("::"), text);
}

pub fn success(text: &str) {
    println!("{} {}", green("✔"), text);
}

pub fn warn(text: &str) {
    eprintln!("{} {}", yellow("warning:"), text);
}

/// Human-readable byte size.
pub fn bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_sizes_read_naturally() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn styling_is_a_no_op_without_color() {
        init(true);
        assert_eq!(bold("hi"), "hi");
        assert_eq!(red("hi"), "hi");
    }
}
