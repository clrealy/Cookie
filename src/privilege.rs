//! Raising privileges, on a system that may not have `sudo`.
//!
//! Cookie needs root for exactly three things — mounting binderfs, mounting
//! the Android images, and unmounting them again — because mounting a
//! filesystem needs root. It asks for that privilege one command at a time
//! and keeps no privileged helper around.
//!
//! Which command does the asking is not a given. `doas` is the usual choice
//! on Gentoo, Alpine and Void, and plenty of systems carry it instead of
//! `sudo` rather than alongside it. So Cookie looks, rather than assuming —
//! both for the commands it runs and for the ones it tells the user to run,
//! since advice naming a program they have not got is no advice at all.

use std::sync::OnceLock;

use anyhow::{bail, Result};

use crate::backend::cmd;

/// The command that raises privileges on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevator {
    Doas,
    Sudo,
}

impl Elevator {
    pub fn name(self) -> &'static str {
        match self {
            Elevator::Doas => "doas",
            Elevator::Sudo => "sudo",
        }
    }
}

/// What is actually available here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Available {
    /// Already root; nothing has to raise anything.
    AlreadyRoot,
    /// This command will do it.
    Found(Elevator),
    /// Not root, and neither `doas` nor `sudo` is installed.
    Nothing,
}

static DETECTED: OnceLock<Available> = OnceLock::new();

/// Work out, once, how this system raises privileges.
pub fn available() -> Available {
    *DETECTED.get_or_init(detect)
}

/// The decision, split out so the rules are testable without the cache.
fn decide(is_root: bool, has_doas: bool, has_sudo: bool, override_name: Option<&str>) -> Available {
    // An explicit choice wins, even for root: the user may be writing a
    // command down for later.
    match override_name.map(str::trim) {
        Some("doas") => return Available::Found(Elevator::Doas),
        Some("sudo") => return Available::Found(Elevator::Sudo),
        _ => {}
    }

    if is_root {
        return Available::AlreadyRoot;
    }

    // `doas` first when both are present. A system carrying sudo may simply
    // have come that way, but installing doas is a deliberate act, so it is
    // the stronger signal of what the user wants used.
    if has_doas {
        return Available::Found(Elevator::Doas);
    }
    if has_sudo {
        return Available::Found(Elevator::Sudo);
    }
    Available::Nothing
}

fn detect() -> Available {
    decide(
        running_as_root(),
        cmd::which("doas").is_some(),
        cmd::which("sudo").is_some(),
        std::env::var("COOKIE_ELEVATE").ok().as_deref(),
    )
}

/// What to call the privilege command when writing advice.
///
/// Advice is read by someone who is about to type it, so it names what they
/// have. With neither installed — or while running as root, where the
/// question has not come up — it falls back to `sudo`, which is the one
/// everyone recognises.
pub fn name() -> &'static str {
    match available() {
        Available::Found(elevator) => elevator.name(),
        Available::AlreadyRoot | Available::Nothing => "sudo",
    }
}

/// `doas mount -t binder binder /dev/binderfs`
///
/// For printing, not for running.
pub fn hint(command: &str) -> String {
    format!("{} {command}", name())
}

/// Are we root already?
pub fn running_as_root() -> bool {
    // Reading the status file avoids a libc call for one number, and works
    // the same inside a container.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .and_then(|values| values.split_whitespace().next().map(str::to_string))
        })
        .map(|uid| uid == "0")
        .unwrap_or(false)
}

/// Run a command as root.
///
/// Runs it directly when already root, and otherwise through whichever
/// elevator this system has. Returns the command's output; a non-zero exit
/// is the caller's to interpret.
pub fn run_as_root(program: &str, args: &[&str]) -> Result<cmd::Output> {
    match available() {
        Available::AlreadyRoot => cmd::run(program, args),
        Available::Found(elevator) => {
            let mut full: Vec<&str> = vec![program];
            full.extend_from_slice(args);
            cmd::run(elevator.name(), &full)
        }
        Available::Nothing => {
            let rendered = std::iter::once(program)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            bail!(
                "this needs root, and neither `doas` nor `sudo` is installed.\n\
                 Run it yourself as root:\n  {rendered}\n\
                 Or install one of them — on Gentoo:\n  \
                 emerge --ask app-admin/doas"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_needs_nothing() {
        assert_eq!(decide(true, false, false, None), Available::AlreadyRoot);
        assert_eq!(decide(true, true, true, None), Available::AlreadyRoot);
    }

    #[test]
    fn whichever_one_is_installed_is_used() {
        assert_eq!(
            decide(false, true, false, None),
            Available::Found(Elevator::Doas)
        );
        assert_eq!(
            decide(false, false, true, None),
            Available::Found(Elevator::Sudo)
        );
    }

    /// Installing doas is a deliberate act; sudo may simply have come with
    /// the system. So doas wins when both are there.
    #[test]
    fn doas_wins_when_both_are_present() {
        assert_eq!(
            decide(false, true, true, None),
            Available::Found(Elevator::Doas)
        );
    }

    #[test]
    fn neither_installed_is_reported_rather_than_guessed_at() {
        assert_eq!(decide(false, false, false, None), Available::Nothing);
    }

    #[test]
    fn an_explicit_choice_overrides_everything() {
        // Including the other one being the only one installed, and
        // including already being root.
        assert_eq!(
            decide(false, false, true, Some("doas")),
            Available::Found(Elevator::Doas)
        );
        assert_eq!(
            decide(false, true, false, Some("sudo")),
            Available::Found(Elevator::Sudo)
        );
        assert_eq!(
            decide(true, true, true, Some("sudo")),
            Available::Found(Elevator::Sudo)
        );
    }

    #[test]
    fn an_unrecognised_override_is_ignored_rather_than_obeyed() {
        // Obeying it would mean running an arbitrary program as root.
        assert_eq!(
            decide(false, true, false, Some("rm")),
            Available::Found(Elevator::Doas)
        );
        assert_eq!(decide(false, false, false, Some("")), Available::Nothing);
    }

    #[test]
    fn overrides_tolerate_surrounding_space() {
        assert_eq!(
            decide(false, false, true, Some(" doas ")),
            Available::Found(Elevator::Doas)
        );
    }

    #[test]
    fn advice_falls_back_to_the_name_everyone_knows() {
        // Whatever this machine has, the name must be one of the two and
        // must not be empty.
        assert!(["doas", "sudo"].contains(&name()));
        assert!(hint("mount -t binder binder /dev/binderfs").ends_with("/dev/binderfs"));
        assert!(hint("x").starts_with(name()));
    }

    #[test]
    fn root_detection_agrees_with_proc() {
        let detected = running_as_root();
        let from_proc = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("Uid:"))
                    .and_then(|v| v.split_whitespace().next().map(str::to_string))
            });
        if let Some(uid) = from_proc {
            assert_eq!(detected, uid == "0");
        }
    }
}

#[cfg(test)]
mod no_placeholder_tests {
    //! A sweep for the mistake this module invites.
    //!
    //! Advice strings name the privilege command by interpolating `{su}`.
    //! In a `format!` a missing binding is a compile error — but in a plain
    //! string literal it is not, and `{su}` reaches the user verbatim. Three
    //! did exactly that. These call the message-producing functions and
    //! check the output, which is the only place the difference shows.

    use crate::backend::oven::{binder, image_hal_caveat};
    use crate::system::InitSystem;

    fn assert_clean(label: &str, text: &str) {
        assert!(
            !text.contains("{su}"),
            "{label} still has an unsubstituted placeholder:\n{text}"
        );
        assert!(
            !text.contains("{}"),
            "{label} has an unsubstituted argument:\n{text}"
        );
    }

    #[test]
    fn advice_never_reaches_the_user_with_a_placeholder_in_it() {
        assert_clean("binder unsupported", &binder::unsupported_message());
        assert_clean("image hal caveat", &image_hal_caveat());
        assert_clean(
            "no wayland",
            &crate::backend::oven::display::no_wayland_message(),
        );

        for init in [
            InitSystem::Systemd,
            InitSystem::OpenRc,
            InitSystem::Runit,
            InitSystem::S6,
            InitSystem::Unknown,
        ] {
            assert_clean("start hint", &init.start_service_hint("waydroid-container"));
            assert_clean(
                "enable hint",
                &init.enable_service_hint("waydroid-container"),
            );
        }
    }

    /// Every check `cookie burnt` can produce, including the advice on each.
    #[test]
    fn no_health_check_carries_a_placeholder() {
        let backend = crate::backend::detect(Some("oven")).expect("oven backend");
        let report = crate::health::run(backend.as_ref()).expect("health report");

        for check in &report.checks {
            assert_clean(&format!("check `{}` detail", check.label), &check.detail);
            if let Some(fix) = &check.fix {
                assert_clean(&format!("check `{}` fix", check.label), fix);
            }
        }

        // And the same for the Waydroid backend's own notes.
        let waydroid = crate::backend::detect(Some("waydroid")).expect("waydroid backend");
        for note in waydroid.health_notes() {
            assert_clean(&note.label, &note.detail);
            if let Some(advice) = &note.advice {
                assert_clean(&note.label, advice);
            }
        }
        for note in backend.health_notes() {
            assert_clean(&note.label, &note.detail);
            if let Some(advice) = &note.advice {
                assert_clean(&note.label, advice);
            }
        }
    }
}
