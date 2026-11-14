//! The `lunix` command line client.
//!
//! The binary is a thin `main`; everything lives here so tests can reach it.
//! Socket subcommands talk to `lunixd` over the control protocol of
//! [`lunix_iface::ctl`]; `fsck` and `key` are built in M13b.

#![forbid(unsafe_code)]

pub mod cli;
pub mod client;
pub mod cmd;
pub mod error;
pub mod fsck;
pub mod key;
pub mod parse;
pub mod render;
pub mod secret_input;
pub mod warn;

pub use cli::Cli;
pub use cmd::run;
