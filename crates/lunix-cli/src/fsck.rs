//! `lunix fsck`: built in M13b. This file holds the final argument type and
//! entry point signature; M13b replaces the body of [`run_fsck`] (and may add
//! fields to [`FsckArgs`] it needs).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;

use crate::secret_input::PassphraseArgs;

/// Arguments of `lunix fsck NAME`.
#[derive(Debug, Args)]
pub struct FsckArgs {
    /// The volume name.
    pub name: String,

    /// The directory the volume was created in (the --dir of `volume create`).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Verify every extent, not only the metadata.
    #[arg(long)]
    pub deep: bool,

    /// Rebuild the chunk index from the segments.
    #[arg(long)]
    pub rebuild_index: bool,

    /// Repair what can be repaired (the volume must be detached).
    #[arg(long)]
    pub repair: bool,

    /// With --repair: also run garbage collection on condemned files.
    #[arg(long, requires = "repair")]
    pub gc: bool,

    /// With --repair: set aside a tip that does not authenticate and make its
    /// predecessor the tip (drops that one commit).
    #[arg(long, requires = "repair")]
    pub truncate_tip: bool,

    /// With --repair: accept this copy as the trusted one and lower the
    /// rollback floor to its tip (only after you verified the copy; loses
    /// nothing, but disables rollback detection for the older history).
    #[arg(long, requires = "repair")]
    pub reset_floor: bool,

    /// With --repair: delete orphaned files above the tip.
    #[arg(long, requires = "repair")]
    pub delete_orphans: bool,

    /// A key file to unlock the volume with.
    #[arg(long, value_name = "FILE")]
    pub key_file: Option<PathBuf>,

    /// The unlock passphrase.
    #[command(flatten)]
    pub pass: PassphraseArgs,

    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,

    /// The control socket (set by `main` from the global option; used to
    /// refuse when the volume is attached).
    #[arg(skip)]
    pub socket: PathBuf,
}

/// Runs `lunix fsck`.
pub fn run_fsck(args: FsckArgs) -> ExitCode {
    let _ = args;
    eprintln!("lunix fsck: built in M13b");
    ExitCode::from(2)
}
