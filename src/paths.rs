//! Where Cookie keeps its things.
//!
//! Everything lives under the XDG base directories in the user's home. No
//! system paths, no root, nothing that needs `sudo`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// `$XDG_DATA_HOME/cookie`, usually `~/.local/share/cookie`.
pub fn data_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().context("cannot work out your data directory (is $HOME set?)")?;
    Ok(base.join("cookie"))
}

/// `$XDG_CACHE_HOME/cookie`, usually `~/.cache/cookie`.
pub fn cache_dir() -> Result<PathBuf> {
    let base = dirs::cache_dir().context("cannot work out your cache directory (is $HOME set?)")?;
    Ok(base.join("cookie"))
}

/// The jar: one directory per installed cookie.
pub fn jar_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("jar"))
}

/// A single cookie's directory inside the jar.
pub fn cookie_dir(package: &str) -> Result<PathBuf> {
    Ok(jar_dir()?.join(package))
}

/// Where generated `.desktop` launchers go (milestone 2).
pub fn applications_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().context("cannot work out your data directory (is $HOME set?)")?;
    Ok(base.join("applications"))
}

/// Create a directory and everything above it.
pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))
}

/// A temporary directory under Cookie's cache that deletes itself on drop.
///
/// We roll our own rather than add a dependency: the needs are modest, and
/// keeping scratch space under `~/.cache/cookie` means a crashed run leaves
/// its leftovers somewhere obvious instead of in `/tmp`.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub fn new(tag: &str) -> Result<Self> {
        let root = cache_dir()?.join("scratch");
        ensure_dir(&root)?;

        // pid plus a monotonic counter is enough: one process, many scratch
        // dirs, no collisions.
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("{tag}-{}-{seq}", std::process::id()));

        // Start from a clean slate if a previous run died here.
        if path.exists() {
            let _ = fs::remove_dir_all(&path);
        }
        ensure_dir(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        // Best effort: a failure here must not mask the real result.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Render a path with `~` for the home directory, for tidier output.
pub fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// Reduce an archive entry name to a safe, bare filename.
///
/// Archive entries come from files the user did not write, so this is the
/// single place that decides what is allowed to become a path component.
/// Every directory part is dropped, every character outside a small safe set
/// becomes `_`, and no `..` survives — so a hostile archive cannot write
/// outside the directory we chose for it.
///
/// Returns `None` when nothing usable is left.
pub fn sanitize_file_name(entry: &str) -> Option<String> {
    let base = Path::new(entry).file_name()?.to_str()?;

    let mapped: String = base
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            _ => '_',
        })
        .collect();

    // Collapse any run of dots, which removes `..` wherever it appears.
    let mut cleaned = String::with_capacity(mapped.len());
    let mut dots = 0usize;
    for c in mapped.chars() {
        if c == '.' {
            dots += 1;
            continue;
        }
        if dots > 0 {
            // A single dot is an ordinary extension separator; more than one
            // is someone being clever.
            cleaned.push(if dots == 1 { '.' } else { '_' });
            dots = 0;
        }
        cleaned.push(c);
    }
    if dots == 1 {
        cleaned.push('.');
    } else if dots > 1 {
        cleaned.push('_');
    }

    let trimmed = cleaned.trim_matches(|c| c == '.' || c == '_');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Turn an app name into something safe for a filename.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "cookie".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_names_keep_ordinary_files_intact() {
        assert_eq!(
            sanitize_file_name("app/base.apk").as_deref(),
            Some("base.apk")
        );
        assert_eq!(
            sanitize_file_name("split_config.arm64_v8a.apk").as_deref(),
            Some("split_config.arm64_v8a.apk")
        );
    }

    #[test]
    fn sanitized_names_cannot_traverse() {
        for hostile in [
            "../../etc/passwd",
            "..%2f..%2fetc",
            "app/../../../root/.ssh/authorized_keys",
            "....//....//evil.apk",
        ] {
            let cleaned = sanitize_file_name(hostile).unwrap_or_default();
            assert!(!cleaned.contains('/'), "{hostile} -> {cleaned}");
            assert!(!cleaned.contains(".."), "{hostile} -> {cleaned}");
            assert!(!cleaned.starts_with('.'), "{hostile} -> {cleaned}");
        }
    }

    #[test]
    fn sanitized_names_reject_the_unusable() {
        assert_eq!(sanitize_file_name(""), None);
        assert_eq!(sanitize_file_name(".."), None);
        assert_eq!(sanitize_file_name("app/"), Some("app".to_string()));
        assert_eq!(sanitize_file_name("..."), None);
        assert_eq!(sanitize_file_name("///"), None);
    }

    #[test]
    fn slugify_handles_real_app_names() {
        assert_eq!(slugify("Signal"), "signal");
        assert_eq!(slugify("F-Droid"), "f-droid");
        assert_eq!(slugify("My  App!!"), "my-app");
        assert_eq!(slugify("  leading"), "leading");
        assert_eq!(slugify("***"), "cookie");
        assert_eq!(slugify(""), "cookie");
        assert_eq!(
            slugify("org.thoughtcrime.securesms"),
            "org-thoughtcrime-securesms"
        );
    }

    #[test]
    fn scratch_dir_cleans_up_after_itself() {
        let path = {
            let scratch = ScratchDir::new("test").expect("scratch dir");
            let p = scratch.path().to_path_buf();
            assert!(p.is_dir());
            fs::write(p.join("junk"), b"junk").expect("write");
            p
        };
        assert!(!path.exists(), "scratch dir should be gone after drop");
    }

    #[test]
    fn scratch_dirs_do_not_collide() {
        let a = ScratchDir::new("test").expect("a");
        let b = ScratchDir::new("test").expect("b");
        assert_ne!(a.path(), b.path());
    }
}
