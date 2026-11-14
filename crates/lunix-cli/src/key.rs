//! `lunix key`: built in M13b. This file holds the final argument types and
//! entry point signature; M13b replaces the body of [`run_key`] (and may add
//! fields to the argument types it needs).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};

use crate::secret_input::PassphraseArgs;

/// Arguments of `lunix key`.
#[derive(Debug, Args)]
pub struct KeyArgs {
    /// The key action.
    #[command(subcommand)]
    pub command: KeyCommand,

    /// The control socket (set by `main` from the global option).
    #[arg(skip)]
    pub socket: PathBuf,
}

/// The `key` actions; all work offline on a detached volume.
#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    /// List the key slots (a plain volume has none).
    Ls(KeyLsArgs),
    /// Add a key slot.
    AddSlot(KeyAddArgs),
    /// Remove a key slot.
    RmSlot(KeyRmArgs),
}

/// `lunix key ls NAME`
#[derive(Debug, Args)]
pub struct KeyLsArgs {
    /// The volume name.
    pub name: String,

    /// The directory the volume was created in.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

/// `lunix key add-slot NAME`
#[derive(Debug, Args)]
pub struct KeyAddArgs {
    /// The volume name.
    pub name: String,

    /// The directory the volume was created in.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// The new slot's passphrase.
    #[command(flatten)]
    pub pass: PassphraseArgs,

    /// The new slot's key file (a key-file slot instead of a passphrase slot).
    #[arg(long, value_name = "FILE")]
    pub key_file: Option<PathBuf>,

    /// An existing key file that unlocks the volume.
    #[arg(long, value_name = "FILE")]
    pub unlock_key_file: Option<PathBuf>,

    /// Read the existing passphrase that unlocks the volume from this
    /// descriptor.
    #[arg(long, value_name = "FD")]
    pub unlock_passphrase_fd: Option<u32>,
}

/// `lunix key rm-slot NAME --slot N`
#[derive(Debug, Args)]
pub struct KeyRmArgs {
    /// The volume name.
    pub name: String,

    /// The slot number from `key ls`.
    #[arg(long, value_name = "N")]
    pub slot: u32,

    /// The directory the volume was created in.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// An existing key file that unlocks the volume.
    #[arg(long, value_name = "FILE")]
    pub unlock_key_file: Option<PathBuf>,

    /// Read the existing passphrase that unlocks the volume from this
    /// descriptor.
    #[arg(long, value_name = "FD")]
    pub unlock_passphrase_fd: Option<u32>,
}

/// Runs `lunix key`.
pub fn run_key(args: KeyArgs) -> ExitCode {
    let _ = args;
    eprintln!("lunix key: built in M13b");
    ExitCode::from(2)
}
