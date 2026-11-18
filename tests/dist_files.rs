//! The files in `dist/` are for packagers installing Cookie system-wide.
//!
//! They have to say exactly what `cookie kitchen setup` writes, or a
//! packaged Cookie and a hand-installed one behave differently. Rather than
//! trust that, these tests regenerate the content and compare.
//!
//! To update them after changing the generator:
//!
//! ```sh
//! COOKIE_REGENERATE_DIST=1 cargo test --test dist_files
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use cookie::desktop::{handler_entry, mime};

fn dist(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("dist")
        .join(name)
}

/// Compare a dist file against freshly generated content, or rewrite it.
fn check(name: &str, expected: &str) {
    let path = dist(name);

    if std::env::var_os("COOKIE_REGENERATE_DIST").is_some() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dist/");
        }
        fs::write(&path, expected).expect("write dist file");
        return;
    }

    let actual = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "cannot read {}: {err}\n\
             Create it with: COOKIE_REGENERATE_DIST=1 cargo test --test dist_files",
            path.display()
        )
    });

    assert_eq!(
        actual,
        expected,
        "\n{} has drifted from what Cookie generates.\n\
         Update it with: COOKIE_REGENERATE_DIST=1 cargo test --test dist_files\n",
        path.display()
    );
}

/// The system-wide handler uses a bare `cookie`, since a packaged binary is
/// on `PATH`. The per-user one written by `kitchen setup` uses an absolute
/// path instead, because a desktop session's `PATH` often lacks
/// `~/.local/bin`.
#[test]
fn packaged_desktop_entry_matches_what_cookie_generates() {
    check("cookie.desktop", &handler_entry("cookie").render());
}

#[test]
fn packaged_mime_definition_matches_what_cookie_generates() {
    check("cookie.xml", &mime::mime_xml());
}

#[test]
fn the_packaged_entry_opens_cpk_files() {
    let rendered = handler_entry("cookie").render();
    assert!(rendered.contains("Exec=cookie eat %f"), "{rendered}");
    assert!(
        rendered.contains("MimeType=application/x-cookie-package;"),
        "{rendered}"
    );
    assert!(rendered.contains("NoDisplay=true"), "{rendered}");
}

#[test]
fn the_packaged_mime_definition_has_no_glob() {
    // See desktop::mime: a glob would claim every .cpk on the system,
    // including other formats that share the extension.
    assert!(!mime::mime_xml().contains("<glob"));
}
