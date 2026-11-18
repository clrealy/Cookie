//! Running other programs, with errors a person can act on.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// What a finished command left behind.
#[derive(Debug, Clone)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// stderr if there is any, otherwise stdout: whichever carries the
    /// program's complaint.
    pub fn message(&self) -> String {
        let stderr = self.stderr.trim();
        if !stderr.is_empty() {
            return stderr.to_string();
        }
        self.stdout.trim().to_string()
    }
}

/// Run a command to completion, capturing both streams.
///
/// A non-zero exit is *not* an error here: callers decide what it means.
pub fn run<S: AsRef<OsStr>>(program: &str, args: &[S]) -> Result<Output> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run `{program}`"))?;
    Ok(Output {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Run a command and turn a non-zero exit into an error.
pub fn run_checked<S: AsRef<OsStr>>(program: &str, args: &[S]) -> Result<Output> {
    let out = run(program, args)?;
    if out.ok() {
        return Ok(out);
    }
    let rendered: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    let message = out.message();
    anyhow::bail!(
        "`{program} {}` failed{}{}",
        rendered.join(" "),
        match out.code {
            Some(code) => format!(" (exit {code})"),
            None => " (killed by a signal)".to_string(),
        },
        if message.is_empty() {
            String::new()
        } else {
            format!(":\n{}", indent(&message))
        }
    )
}

/// Find an executable on `PATH`, without shelling out to `which`.
pub fn which(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return is_executable(&path).then_some(path);
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Indent a block of program output so it reads as quoted text.
pub fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout() {
        let out = run("sh", &["-c", "printf hello"]).expect("runs");
        assert!(out.ok());
        assert_eq!(out.stdout, "hello");
    }

    #[test]
    fn non_zero_is_not_an_error_for_run() {
        let out = run("sh", &["-c", "exit 3"]).expect("runs");
        assert!(!out.ok());
        assert_eq!(out.code, Some(3));
    }

    #[test]
    fn run_checked_reports_the_program_complaint() {
        let err = run_checked("sh", &["-c", "echo nope >&2; exit 1"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "{err}");
        assert!(err.contains("exit 1"), "{err}");
    }

    #[test]
    fn missing_program_is_a_clear_error() {
        let err = run("cookie-definitely-not-a-real-program", &["x"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot run"), "{err}");
    }

    #[test]
    fn which_finds_a_real_binary() {
        assert!(which("sh").is_some());
        assert!(which("cookie-definitely-not-a-real-program").is_none());
    }

    #[test]
    fn indent_quotes_each_line() {
        assert_eq!(indent("a\nb"), "  a\n  b");
    }
}
