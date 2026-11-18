//! The Cookie Package format.
//!
//! A `.cpk` is a zip archive holding one Android app and the metadata needed
//! to install, launch and show it like a native program. See
//! `docs/cpk-spec.md` for the full specification.

pub mod manifest;
pub mod pack;
pub mod unpack;

pub use manifest::{CpkManifest, CPK_FORMAT_VERSION};

/// Conventional file extension.
pub const EXTENSION: &str = "cpk";

/// MIME type registered with the desktop (milestone 2).
pub const MIME_TYPE: &str = "application/x-cookie-package";
