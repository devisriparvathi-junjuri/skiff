//! Shared types for Skiff: the session model, the daemon wire protocol,
//! the `projects.toml` config, and the socket location.

pub mod alias;
pub mod config;
pub mod files;
pub mod git;
pub mod group;
#[cfg(unix)]
pub mod host_identity;
pub mod open;
pub mod project;
pub mod protocol;
pub mod session;
pub mod socket;
pub mod state;
pub mod shell;
pub mod tasks;
pub mod theme;

pub use group::{Group, Layout, SplitDir};
pub use project::{Project, Worktree};

/// The skiffd version. It moves apart from the app version: a release bumps it
/// only when `crates/skiffd` or `crates/skiff-core` changed. The app compares
/// the running daemon with this, so a release without daemon changes asks for
/// no restart.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Orders dotted versions by number. A part that is not a number counts as 0.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |v: &str| -> Vec<u64> { v.split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect() };
    parts(a).cmp(&parts(b))
}

/// The daemon's wire contract. An app that sees another number asks the user
/// to restart skiffd, which stops every session, so raise it only when a new
/// app cannot use the daemon that ran before it:
///
/// - Raise it when a request, reply or event the app uses changes shape or
///   meaning, or when the app needs a new request and has no fallback.
/// - Do not raise it to add a request, a reply, an event or an optional
///   (`#[serde(default)]`) field. An older daemon answers an unknown request
///   with an error, and both sides skip a message they cannot read.
/// - Do not raise it to remove a request the app no longer sends.
pub const PROTOCOL: u32 = 27;
