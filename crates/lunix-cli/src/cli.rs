//! The command line: the `clap` tree of the `lunix` binary.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use lunix_format::local::{FreeFloor, OnFull};
use lunix_iface::ctl::{Compress, JournalDamagePolicy, VolumeMode};

use crate::fsck::FsckArgs;
use crate::key::KeyArgs;
use crate::parse::{
    parse_compress, parse_duration_ms, parse_floor, parse_id_len, parse_size, parse_size_u32,
};
use crate::secret_input::PassphraseArgs;

/// The control socket the daemon listens on by default.
pub const DEFAULT_SOCKET: &str = "/run/lunix/ctl.sock";

const EXIT_CODES: &str = "\
Exit codes:
  0  success
  1  generic error (including a daemon that cannot be reached)
  2  usage error
  3  busy (the volume or device is in use)
  4  not found
  5  no space
  6  downgrade, wrong key, rollback or fork (the credential or history does not match)
  7  journal damage found and the attach asked to refuse

Sizes are digits with an optional K, M, G or T suffix (powers of 1024).
Secrets are read from a terminal prompt, --passphrase-fd or a key file, never from
the command line or the environment.";

/// The `lunix` command line.
#[derive(Debug, Parser)]
#[command(name = "lunix", version, about = "Command line client of lunixd", after_help = EXIT_CODES)]
#[command(disable_help_subcommand = true)]
pub struct Cli {
    /// The control socket of lunixd.
    #[arg(long, global = true, value_name = "PATH", default_value = DEFAULT_SOCKET)]
    pub socket: PathBuf,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create and list volumes.
    Volume(VolumeArgs),
    /// Attach a volume and expose it as /dev/lunix/NAME.
    Attach(AttachArgs),
    /// Detach a volume.
    Detach(NameArg),
    /// Show one volume's status, or every volume's.
    Status(StatusArgs),
    /// Grow a volume.
    Resize(ResizeArgs),
    /// Show or change a volume's space policy.
    Space(SpaceArgs),
    /// Run a garbage-collection pass, a dry run, or show the collector state.
    Gc(GcArgs),
    /// Check (and repair) a detached volume offline.
    Fsck(FsckArgs),
    /// List and change the key slots of a detached volume.
    Key(KeyArgs),
}

/// A command that names one volume.
#[derive(Debug, Args)]
pub struct NameArg {
    /// The volume name.
    pub name: String,
}

/// `lunix volume ...`
#[derive(Debug, Args)]
pub struct VolumeArgs {
    /// The volume action.
    #[command(subcommand)]
    pub command: VolumeCommand,
}

/// The `volume` actions.
#[derive(Debug, Subcommand)]
pub enum VolumeCommand {
    /// Create a volume (integrity mode unless told otherwise).
    Create(CreateArgs),
    /// List the volumes the daemon knows.
    List(ListArgs),
}

/// The three mode flags, mutually exclusive.
#[derive(Debug, Args)]
#[group(multiple = false)]
pub struct ModeArgs {
    /// Encrypted volume: AEAD extents and sealed metadata. Takes the same
    /// key options as the default mode.
    #[arg(long)]
    pub encrypt: bool,

    /// Integrity volume (the default): keyed tags and HMACs detect tampering
    /// by anyone without the key; the contents are not encrypted. A random key
    /// is written to <dir>/<uuid>/integrity.key (0600) unless --key-file or
    /// --passphrase says otherwise, and attach finds it by itself.
    #[arg(long)]
    pub integrity: bool,

    /// Plain volume: unkeyed checksums detect corruption only and protect
    /// against nobody who can write the storage. No key material is created,
    /// so it cannot be combined with --key-file or --passphrase.
    #[arg(long, conflicts_with_all = ["key_file", "passphrase", "passphrase_fd"])]
    pub plain: bool,
}

impl ModeArgs {
    /// The mode the flags select; integrity when none was given.
    pub fn mode(&self) -> VolumeMode {
        if self.encrypt {
            VolumeMode::Encrypted
        } else if self.plain {
            VolumeMode::Plain
        } else {
            VolumeMode::Integrity
        }
    }
}

/// `lunix volume create`
#[derive(Debug, Args)]
#[command(after_help = "\
Modes: --encrypt, --integrity and --plain are mutually exclusive (default: integrity).
The default writes a random key to <dir>/<uuid>/integrity.key (0600); attach reads it
by itself. --passphrase adds a passphrase slot for a volume that moves between hosts.")]
pub struct CreateArgs {
    /// The volume name (letters, digits, `.`, `_`, `-`; at most 64).
    pub name: String,

    /// Volume size (a multiple of 4096 bytes), such as 200G.
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub size: u64,

    /// The directory that holds the volume's files (made absolute here: the
    /// daemon does not share this process's working directory).
    #[arg(long, value_name = "DIR")]
    pub dir: PathBuf,

    /// Chunk size: a power of two from 16K to 1M.
    #[arg(long, value_name = "SIZE", value_parser = parse_size_u32, default_value = "64K")]
    pub chunk: u32,

    /// The volume mode (default: integrity).
    #[command(flatten)]
    pub mode: ModeArgs,

    /// Add a passphrase slot (for a volume that moves between hosts).
    #[command(flatten)]
    pub pass: PassphraseArgs,

    /// Use this existing key file for the first slot instead of a generated one.
    #[arg(long, value_name = "FILE")]
    pub key_file: Option<PathBuf>,

    /// Compression: auto, lz4, zstd:N (N from -7 to 22) or none.
    #[arg(long, value_name = "POLICY", value_parser = parse_compress, default_value = "auto")]
    pub compress: Compress,

    /// Length of the chunk-id prefix kept in the block map: 16 or 32.
    #[arg(long, value_name = "BYTES", value_parser = parse_id_len, default_value = "16")]
    pub id_len: u8,
}

/// `lunix volume list`
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Print JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

/// How a credential is declared on attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CredentialMode {
    /// The credential unlocks an encrypted volume (the default when a key or
    /// passphrase is given).
    Encrypted,
    /// The credential is an integrity credential (a passphrase or key file for an
    /// integrity volume).
    Integrity,
}

/// The journal damage policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DamageArg {
    /// Log the damage and wipe the damaged tail (a torn last record is
    /// always crash residue and is wiped).
    Wipe,
    /// Refuse to attach on damage in the middle of the journal; run fsck.
    Refuse,
}

impl From<DamageArg> for JournalDamagePolicy {
    fn from(d: DamageArg) -> Self {
        match d {
            DamageArg::Wipe => Self::Wipe,
            DamageArg::Refuse => Self::Refuse,
        }
    }
}

/// `lunix attach`
#[derive(Debug, Args)]
pub struct AttachArgs {
    /// The volume name.
    pub name: String,

    /// Number of queues (1 to 16; the daemon's default when absent).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..=16))]
    pub queues: Option<u16>,

    /// Queue depth (1 to 4096).
    #[arg(long, value_name = "D", value_parser = clap::value_parser!(u16).range(1..=4096))]
    pub depth: Option<u16>,

    /// CPUs for the queue threads, such as 2-5,8.
    #[arg(long, value_name = "LIST")]
    pub cpus: Option<String>,

    /// Largest I/O buffer, at least 256K (such as 1M).
    #[arg(long, value_name = "SIZE", value_parser = parse_size_u32)]
    pub max_io_buf: Option<u32>,

    /// A key file the daemon reads to unlock the volume (an integrity volume
    /// with its own integrity.key needs nothing).
    #[arg(long, value_name = "FILE", conflicts_with_all = ["passphrase", "passphrase_fd"])]
    pub key_file: Option<PathBuf>,

    /// The unlock passphrase.
    #[command(flatten)]
    pub pass: PassphraseArgs,

    /// The key fingerprint you expect, 32 lowercase hex digits.
    #[arg(long, value_name = "HEX")]
    pub fingerprint: Option<String>,

    /// The mode of your credential. A passphrase or key file for an integrity
    /// volume must say `integrity`; the default with key material is `encrypted`.
    #[arg(long, value_enum, value_name = "MODE")]
    pub credential_mode: Option<CredentialMode>,

    /// What to do about journal damage found at recovery (the daemon's
    /// configured default when absent).
    #[arg(long, value_enum, value_name = "POLICY")]
    pub on_journal_damage: Option<DamageArg>,

    /// Attach although the commit chain back to this host's rollback floor
    /// cannot be walked (audited by the daemon).
    #[arg(long)]
    pub allow_unverifiable_fork: bool,
}

/// `lunix status`
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// The volume (every volume when absent).
    pub name: Option<String>,

    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

/// `lunix resize`
#[derive(Debug, Args)]
pub struct ResizeArgs {
    /// The volume name.
    pub name: String,

    /// The new size (grow only), such as 300G.
    #[arg(value_name = "SIZE", value_parser = parse_size)]
    pub size: u64,
}

/// What a full volume does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnFullArg {
    /// Block writers until space returns.
    Wait,
    /// Fail the write.
    Error,
}

impl From<OnFullArg> for OnFull {
    fn from(o: OnFullArg) -> Self {
        match o {
            OnFullArg::Wait => Self::Wait,
            OnFullArg::Error => Self::Error,
        }
    }
}

/// `lunix space`
#[derive(Debug, Args)]
pub struct SpaceArgs {
    /// The volume name.
    pub name: String,

    /// Free-space floor on the host file system: a percentage (10%) or a size.
    #[arg(long, value_name = "X", value_parser = parse_floor)]
    pub floor: Option<FreeFloor>,

    /// What writers do when the space ladder is exhausted.
    #[arg(long, value_enum, value_name = "POLICY")]
    pub on_full: Option<OnFullArg>,

    /// How long writers wait for space: 500ms, 60s, 5m or 1h.
    #[arg(long, value_name = "T", value_parser = parse_duration_ms)]
    pub full_timeout: Option<u64>,

    /// The most the local cache may grow to.
    #[arg(long, value_name = "X", value_parser = parse_size)]
    pub cache_max: Option<u64>,

    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

/// `lunix gc`
#[derive(Debug, Args)]
pub struct GcArgs {
    /// The volume name.
    pub name: String,

    /// Change nothing; report what a pass would do.
    #[arg(long, conflicts_with = "status")]
    pub dry_run: bool,

    /// Show the collector's state instead of running a pass.
    #[arg(long)]
    pub status: bool,

    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}
