//! The command line, in grandma's kitchen vocabulary.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Run Android apps on Linux like they belong there.
#[derive(Debug, Parser)]
#[command(
    name = "cookie",
    version,
    about = "Run Android apps on Linux like they belong there",
    long_about = "Cookie wraps an Android app into a .cpk file that you can double-click, \
                  and runs it in its own ordinary desktop window.\n\n\
                  Wine runs Windows programs. Darling runs macOS programs. Both sound like \
                  something your grandma would offer you, and grandmas give you cookies.",
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub global: GlobalArgs,
}

#[derive(Debug, Args, Clone)]
pub struct GlobalArgs {
    /// Never colourise output (NO_COLOR is honoured too).
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Show more detail, including raw backend output.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Android runtime to use.
    #[arg(long, global = true, value_name = "NAME", default_value = "auto")]
    pub backend: String,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Wrap an APK into a .cpk cookie
    #[command(long_about = "Wrap one or more APKs into a single .cpk file.\n\n\
                            Give it a plain APK, a base APK plus its splits, or one \
                            .xapk / .apks / .apkm bundle. Cookie only ever reads files you \
                            hand it; it never downloads an app for you.")]
    Bake(BakeArgs),

    /// Install a cookie if needed, then run it
    #[command(long_about = "Install a cookie and launch it.\n\n\
                            Give it a .cpk file, or the name of an app already in the jar. \
                            Installing also adds the app to your application menu, with its \
                            own icon.")]
    Eat(EatArgs),

    /// List the cookies in the jar
    Jar(JarArgs),

    /// Show an app's logs
    #[command(long_about = "Show the Android log for one installed app.\n\n\
                            When the app is running its logs are filtered by process id, \
                            which is exact. When it is not, Cookie falls back to matching \
                            the package name in the log text.")]
    Crumbs(CrumbsArgs),

    /// Check whether anything is burnt
    Burnt(BurntArgs),

    /// Manage the shared Android runtime
    Oven(OvenArgs),

    /// Uninstall a cookie
    Toss(TossArgs),

    /// Set up desktop integration
    #[command(long_about = "Teach your desktop about cookies.\n\n\
                            Registers the application/x-cookie-package MIME type, installs the \
                            handler that makes a double-clicked .cpk run `cookie eat`, and \
                            builds an application-menu entry for everything already in the jar. \
                            Everything goes under your own home directory; nothing needs root.")]
    Kitchen(KitchenArgs),
}

#[derive(Debug, Args)]
pub struct KitchenArgs {
    #[command(subcommand)]
    pub action: Option<KitchenAction>,
}

#[derive(Debug, Subcommand)]
pub enum KitchenAction {
    /// Register the MIME type, the handler and every app launcher
    Setup,
    /// Show what is currently registered
    Status,
    /// Remove Cookie's desktop integration
    Remove(KitchenRemoveArgs),
}

#[derive(Debug, Args)]
pub struct KitchenRemoveArgs {
    /// Also remove every app's menu entry
    #[arg(long)]
    pub launchers: bool,
}

#[derive(Debug, Args)]
pub struct BakeArgs {
    /// APK files, or one .xapk / .apks / .apkm bundle
    #[arg(value_name = "APK", required = true)]
    pub inputs: Vec<PathBuf>,

    /// Where to write the .cpk (default: <name>.cpk here)
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Override the app's display name
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Overwrite an existing .cpk
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct EatArgs {
    /// A .cpk file, or the name of a cookie already in the jar
    #[arg(value_name = "FILE_OR_NAME")]
    pub target: String,

    /// Install again even if it is already in the jar
    #[arg(long)]
    pub reinstall: bool,

    /// Install it, but do not launch it
    #[arg(long)]
    pub no_launch: bool,

    /// Do not create or refresh its application-menu entry
    #[arg(long)]
    pub no_launcher: bool,

    /// Install an older version over a newer one, keeping its data
    #[arg(long)]
    pub allow_downgrade: bool,

    /// Replace an app signed by a different key. Deletes its data.
    #[arg(long)]
    pub replace: bool,
}

#[derive(Debug, Args)]
pub struct JarArgs {
    /// Show packages, ABIs and install dates
    #[arg(short, long)]
    pub long: bool,
}

#[derive(Debug, Args)]
pub struct CrumbsArgs {
    /// App name or package id
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Keep printing new log lines as they arrive
    #[arg(short, long)]
    pub follow: bool,

    /// How many past lines to show
    #[arg(short = 'n', long, value_name = "N", default_value_t = 200)]
    pub lines: usize,
}

#[derive(Debug, Args)]
pub struct BurntArgs {
    /// Also show detail for checks that passed
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct OvenArgs {
    #[command(subcommand)]
    pub action: OvenAction,
}

#[derive(Debug, Subcommand)]
pub enum OvenAction {
    /// Warm up the Android runtime
    Start,
    /// Shut the Android runtime down
    Stop,
    /// Say whether it is running
    Status,
    /// Print the container configuration Cookie would use
    #[command(
        long_about = "Print the container configuration the oven generates.\n\n\
                            This is what Cookie asks of LXC, built from what your machine \
                            actually has: your binder devices, your GPU, your Wayland socket. \
                            It needs nothing installed, so it is a way to check the setup \
                            before committing to it."
    )]
    Recipe,
    /// Manage the shared Android system image
    #[command(long_about = "The Android system image every cookie shares.\n\n\
                            Cookie does not download one. Which system image to trust is your \
                            call, not Cookie's, so you supply the file and Cookie checks that \
                            it is usable.")]
    Image(OvenImageArgs),
}

#[derive(Debug, Args)]
pub struct OvenImageArgs {
    #[command(subcommand)]
    pub action: Option<OvenImageAction>,
}

#[derive(Debug, Subcommand)]
pub enum OvenImageAction {
    /// Register a system image
    Add(OvenImageAddArgs),
    /// Forget the registered image
    Remove,
    /// Show what is registered
    Show,
}

#[derive(Debug, Args)]
pub struct OvenImageAddArgs {
    /// Path to the image, usually system.img
    #[arg(value_name = "FILE")]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
pub struct TossArgs {
    /// App name or package id
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Do not ask for confirmation
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn bake_takes_several_apks() {
        let cli = Cli::try_parse_from(["cookie", "bake", "base.apk", "split.apk"]).expect("parses");
        match cli.command {
            Command::Bake(args) => assert_eq!(args.inputs.len(), 2),
            other => panic!("expected bake, got {other:?}"),
        }
    }

    #[test]
    fn bake_needs_at_least_one_input() {
        assert!(Cli::try_parse_from(["cookie", "bake"]).is_err());
    }

    #[test]
    fn crumbs_follow_has_a_short_flag() {
        let cli = Cli::try_parse_from(["cookie", "crumbs", "signal", "-f"]).expect("parses");
        match cli.command {
            Command::Crumbs(args) => {
                assert!(args.follow);
                assert_eq!(args.lines, 200);
            }
            other => panic!("expected crumbs, got {other:?}"),
        }
    }

    #[test]
    fn crumbs_line_count_is_settable() {
        let cli = Cli::try_parse_from(["cookie", "crumbs", "signal", "-n", "50"]).expect("parses");
        match cli.command {
            Command::Crumbs(args) => assert_eq!(args.lines, 50),
            other => panic!("expected crumbs, got {other:?}"),
        }
    }

    #[test]
    fn oven_requires_an_action() {
        assert!(Cli::try_parse_from(["cookie", "oven"]).is_err());
        assert!(Cli::try_parse_from(["cookie", "oven", "start"]).is_ok());
        assert!(Cli::try_parse_from(["cookie", "oven", "nonsense"]).is_err());
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let cli = Cli::try_parse_from(["cookie", "jar", "--no-color"]).expect("parses");
        assert!(cli.global.no_color);
    }
}
