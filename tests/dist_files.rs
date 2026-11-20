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

// ---------------------------------------------------------------------------
// The GURT recipe
// ---------------------------------------------------------------------------

fn gurtbuild() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("dist/gurt/GURTBUILD");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn field(recipe: &str, key: &str) -> Option<String> {
    recipe
        .lines()
        .find_map(|l| l.trim().strip_prefix(&format!("{key}=")))
        .map(|v| v.trim().trim_matches('"').to_string())
}

/// The commit is pinned in two places — the source and the icon URL. A recipe
/// with those out of step builds the right code and shows the wrong icon,
/// or a 404.
#[test]
fn the_recipe_pins_one_commit_everywhere() {
    let recipe = gurtbuild();
    let hashes: std::collections::BTreeSet<&str> = recipe
        .split(|c: char| !c.is_ascii_hexdigit())
        .filter(|s| s.len() == 40)
        .collect();

    assert_eq!(
        hashes.len(),
        1,
        "the recipe should pin exactly one commit, found: {hashes:?}"
    );
    // And it is actually referenced from both places.
    assert!(recipe.contains("#commit="), "{recipe}");
    assert!(recipe.contains("raw.githubusercontent.com"), "{recipe}");
}

/// gurt decides whether a package is outdated by comparing `pkgver-pkgrel`.
/// A recipe whose commit moved while pkgrel stayed put builds something new
/// and tells every installed copy it is already current — which is exactly
/// what happened, three times, before `repoint.sh` existed.
#[test]
fn the_recipe_has_a_plausible_release_number() {
    let recipe = gurtbuild();
    let rel: u32 = field(&recipe, "pkgrel")
        .expect("pkgrel")
        .parse()
        .expect("pkgrel must be a number");
    assert!(rel >= 1, "pkgrel starts at 1");

    // Every repoint bumps it, so by the time the pin has moved it cannot
    // still be at its initial value.
    assert!(
        rel > 1,
        "the pinned commit has moved since this recipe was written, so pkgrel \
         must have moved too — use dist/gurt/repoint.sh"
    );

    assert!(field(&recipe, "pkgver").is_some(), "pkgver");
    assert_eq!(field(&recipe, "pkgname").as_deref(), Some("cookie"));
}

/// The linter in the GURT repository fails on the word anywhere in a recipe,
/// comments included.
#[test]
fn the_recipe_never_mentions_a_privilege_command() {
    let recipe = gurtbuild();
    for forbidden in ["sudo", "doas"] {
        assert!(
            !recipe.contains(forbidden),
            "GURT recipes must never name `{forbidden}`"
        );
    }
}
