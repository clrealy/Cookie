//! Telling the user something when there is no terminal to tell them in.
//!
//! A cookie launched from the application menu has no stdout anyone will
//! read. When something goes wrong there, the message has to arrive some
//! other way or it may as well not exist.

use std::io::IsTerminal;

use crate::backend::cmd;

/// Is anyone watching stderr?
pub fn has_terminal() -> bool {
    std::io::stderr().is_terminal()
}

/// Show a desktop notification, if this system can.
///
/// Tries `kdialog` first — Plasma is the first-class target — then
/// `notify-send`. Returns whether anything was actually shown, so callers can
/// fall back to printing.
pub fn error(title: &str, body: &str) -> bool {
    // Keep it to one paragraph: notifications truncate, and the full detail
    // is in the log either way.
    let short: String = body.lines().take(4).collect::<Vec<_>>().join("\n");

    if cmd::which("kdialog").is_some() {
        let message = format!("{title}\n\n{short}");
        if let Ok(out) = cmd::run("kdialog", &["--title", "Cookie", "--error", &message]) {
            if out.ok() {
                return true;
            }
        }
    }

    if cmd::which("notify-send").is_some() {
        if let Ok(out) = cmd::run(
            "notify-send",
            &[
                "--app-name=Cookie",
                "--icon=cookie",
                "--urgency=critical",
                title,
                &short,
            ],
        ) {
            if out.ok() {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_about_the_terminal_never_panics() {
        let _ = has_terminal();
    }

    #[test]
    fn notifying_with_no_tools_present_reports_failure() {
        // In a container with neither kdialog nor notify-send this returns
        // false rather than erroring, which is what callers rely on.
        if cmd::which("kdialog").is_none() && cmd::which("notify-send").is_none() {
            assert!(!error("Test", "body"));
        }
    }
}
