//! `skiffd` keeps sessions alive across client restarts. Clients (the desktop
//! app, later a TUI or a remote client) speak the protocol in `skiff_core`.

mod command;
pub mod identity;
pub mod paths;
pub mod pty;
pub mod screen;
pub mod server;
pub mod session;
pub mod shellenv;
pub mod stop;
pub mod workspace;
