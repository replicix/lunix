//! `lunix key ls|add-slot|rm-slot NAME`: offline key-slot management.
//!
//! The commands work on the family keyring beside the volume directory
//! (`<dir>/families/<family>/keyring`) with the volume detached, through
//! `lunix-crypto`. A change unlocks the keyring with an existing credential,
//! checks that the unwrapped root belongs to the volume (the descriptor MAC),
//! takes the volume `LOCK` and replaces the keyring atomically with a higher
//! `keyring_seq`. Secrets come from a terminal prompt, a file descriptor or a
//! key file, never from the command line or the environment, and are never
//! printed.
//!
//! A `plain` volume has no keyring and no slots. Removing a slot does not
//! revoke the credential of someone who already saw the root key (ADR-11).

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Subcommand};
use lunix_crypto::keyring::{self, KeyFile, NewSlot, Unlock};
use lunix_crypto::{verify_descriptor_mac, Argon2Cost, UnlockOptions};
use lunix_format::keyring::{Keyring, RootKind, SlotParams};
use lunix_format::local::LocalPinsFile;
use lunix_format::paths::local_keyring_path;
use lunix_format::volume::VolumeDescriptor;
use lunix_format::CryptoMode;
use lunix_iface::ctl::{CtlError, ErrorCode, Secret};
use serde::Serialize;

use crate::error::{sanitize, CliError};
use crate::fsck::{ensure_detached, locate};
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

/// The largest `volume` descriptor read (a descriptor is far smaller).
const MAX_VOLUME_FILE: u64 = 1 << 20;

/// The largest keyring read (32 slots of about 200 bytes).
const MAX_KEYRING_FILE: u64 = 1 << 20;

/// Runs `lunix key`.
pub fn run_key(args: KeyArgs) -> ExitCode {
    run_key_to(
        args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// [`run_key`] with the output streams given.
pub(crate) fn run_key_to(args: KeyArgs, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    let KeyArgs { command, socket } = args;
    let result = match command {
        KeyCommand::Ls(a) => ls(&a, &socket, out),
        KeyCommand::AddSlot(a) => add_slot(a, &socket, out),
        KeyCommand::RmSlot(a) => rm_slot(a, &socket, out),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(err, "lunix: {}: {}", e.class(), e);
            ExitCode::from(e.exit_code().max(1))
        }
    }
}

/// A volume's descriptor and keyring as found on disk.
struct Vol {
    name: String,
    dir: PathBuf,
    desc: VolumeDescriptor,
    keyring_path: PathBuf,
    keyring_bytes: Vec<u8>,
    keyring: Keyring,
}

fn read_capped(path: &Path, cap: u64, what: &str) -> Result<Vec<u8>, CliError> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(cap + 1).read_to_end(&mut buf))
        .map_err(|e| {
            CliError::Other(format!("cannot read the {what} ({}): {e}", path.display()))
        })?;
    if buf.len() as u64 > cap {
        return Err(CliError::Other(format!(
            "the {what} is larger than {cap} bytes"
        )));
    }
    Ok(buf)
}

/// Finds the volume, checks it is detached and reads its descriptor.
fn open_vol(
    name: &str,
    dir: Option<&Path>,
    socket: &Path,
) -> Result<(PathBuf, VolumeDescriptor), CliError> {
    let dir = locate(name, dir, socket)?;
    ensure_detached(name, socket)?;
    let bytes = read_capped(&dir.join("volume"), MAX_VOLUME_FILE, "volume descriptor")?;
    let desc = VolumeDescriptor::decode(&bytes)
        .map_err(|e| CliError::Other(format!("the volume descriptor is damaged: {e}")))?;
    Ok((dir, desc))
}

fn load_keyring(name: &str, dir: PathBuf, desc: VolumeDescriptor) -> Result<Vol, CliError> {
    let abs = std::path::absolute(&dir)
        .map_err(|e| CliError::Other(format!("cannot make the volume path absolute: {e}")))?;
    let parent = abs.parent().unwrap_or_else(|| Path::new("/"));
    let keyring_path = PathBuf::from(local_keyring_path(
        &parent.to_string_lossy(),
        &desc.family_id,
    ));
    let keyring_bytes = read_capped(&keyring_path, MAX_KEYRING_FILE, "keyring")?;
    let keyring = Keyring::decode(&keyring_bytes)
        .map_err(|e| CliError::Other(format!("the keyring is damaged: {e}")))?;
    Ok(Vol {
        name: name.to_owned(),
        dir: abs,
        desc,
        keyring_path,
        keyring_bytes,
        keyring,
    })
}

fn mode_name(m: CryptoMode) -> &'static str {
    match m {
        CryptoMode::Plain => "plain",
        CryptoMode::Integrity => "integrity",
        CryptoMode::Encrypted => "encrypted",
    }
}

fn root_name(k: RootKind) -> &'static str {
    match k {
        RootKind::Kmk => "kmk",
        RootKind::Ik => "ik",
    }
}

#[derive(Serialize)]
struct SlotInfo {
    id: u32,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    argon2id: Option<ArgonInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_ref: Option<String>,
}

#[derive(Serialize)]
struct ArgonInfo {
    m_kib: u32,
    t: u32,
    p: u32,
}

#[derive(Serialize)]
struct KeyListing {
    name: String,
    uuid: String,
    mode: &'static str,
    family: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    keyring_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dek_gen: Option<u32>,
    slots: Vec<SlotInfo>,
}

fn slot_infos(kr: &Keyring) -> Vec<SlotInfo> {
    kr.slots
        .iter()
        .map(|s| match &s.params {
            SlotParams::Passphrase(a) => SlotInfo {
                id: s.slot_id,
                kind: "passphrase",
                argon2id: Some(ArgonInfo {
                    m_kib: a.m_kib,
                    t: a.t,
                    p: a.p,
                }),
                key_ref: None,
            },
            SlotParams::KeyFile => SlotInfo {
                id: s.slot_id,
                kind: "key_file",
                argon2id: None,
                key_ref: None,
            },
            SlotParams::Kms { key_ref } => SlotInfo {
                id: s.slot_id,
                kind: "kms",
                argon2id: None,
                key_ref: Some(sanitize(key_ref)),
            },
        })
        .collect()
}

fn ls(a: &KeyLsArgs, socket: &Path, out: &mut dyn Write) -> Result<(), CliError> {
    let (dir, desc) = open_vol(&a.name, a.dir.as_deref(), socket)?;
    let mut listing = KeyListing {
        name: a.name.clone(),
        uuid: desc.volume_uuid.to_string(),
        mode: mode_name(desc.crypto_mode),
        family: desc.family_id.to_string(),
        keyring_seq: None,
        root: None,
        dek_gen: None,
        slots: Vec::new(),
    };
    if desc.crypto_mode.is_keyed() {
        let vol = load_keyring(&a.name, dir, desc)?;
        listing.keyring_seq = Some(vol.keyring.keyring_seq);
        listing.root = Some(root_name(vol.keyring.root_kind));
        listing.dek_gen = Some(vol.keyring.dek_gen.0);
        listing.slots = slot_infos(&vol.keyring);
    }
    if a.json {
        let s = serde_json::to_string_pretty(&listing)
            .map_err(|_| CliError::Other("cannot encode the output".into()))?;
        writeln!(out, "{s}")?;
        return Ok(());
    }
    writeln!(out, "volume {} ({})", sanitize(&listing.name), listing.mode)?;
    if listing.slots.is_empty() {
        writeln!(out, "a plain volume has no key slots")?;
        return Ok(());
    }
    writeln!(
        out,
        "keyring_seq {}, root key {}, dek_gen {}",
        listing.keyring_seq.unwrap_or(0),
        listing.root.unwrap_or("?"),
        listing.dek_gen.unwrap_or(0)
    )?;
    for s in &listing.slots {
        match (&s.argon2id, &s.key_ref) {
            (Some(a), _) => writeln!(
                out,
                "slot {:<3} passphrase  argon2id m={} KiB t={} p={}",
                s.id, a.m_kib, a.t, a.p
            )?,
            (_, Some(k)) => writeln!(out, "slot {:<3} kms         {k}", s.id)?,
            _ => writeln!(out, "slot {:<3} key file", s.id)?,
        }
    }
    writeln!(
        out,
        "(this listing is read without a credential and is not authenticated)"
    )?;
    Ok(())
}

/// The credential that unlocks the keyring for a change.
enum Cred {
    KeyFile(KeyFile),
    Pass(Secret),
}

impl Cred {
    fn unlock(&self) -> Unlock<'_> {
        match self {
            Self::KeyFile(k) => Unlock::KeyFile(k),
            Self::Pass(p) => Unlock::Passphrase(p.expose()),
        }
    }
}

fn read_key_file(path: &Path, what: &str) -> Result<KeyFile, CliError> {
    KeyFile::read(path).map_err(|e| CliError::Usage(format!("{what} {}: {e}", path.display())))
}

/// Gathers the unlocking credential: `--unlock-key-file`,
/// `--unlock-passphrase-fd`, or the volume's own `integrity.key`.
fn unlock_cred(key_file: Option<&Path>, fd: Option<u32>, vol_dir: &Path) -> Result<Cred, CliError> {
    match (key_file, fd) {
        (Some(_), Some(_)) => Err(CliError::Usage(
            "give one of --unlock-key-file and --unlock-passphrase-fd, not both".to_owned(),
        )),
        (Some(p), None) => Ok(Cred::KeyFile(read_key_file(p, "--unlock-key-file")?)),
        (None, Some(fd)) => {
            let pass = PassphraseArgs {
                passphrase: true,
                passphrase_fd: Some(fd),
            };
            match pass.read(false)? {
                Some(s) => Ok(Cred::Pass(s)),
                None => Err(CliError::Usage("no passphrase was read".to_owned())),
            }
        }
        (None, None) => {
            let default = vol_dir.join("integrity.key");
            if default.exists() {
                Ok(Cred::KeyFile(read_key_file(
                    &default,
                    "the volume's key file",
                )?))
            } else {
                Err(CliError::Usage(
                    "say how to unlock the volume: --unlock-key-file FILE or --unlock-passphrase-fd N".to_owned(),
                ))
            }
        }
    }
}

/// Takes the volume `LOCK` (an `flock`, as the engine and `lunix-fsck` do).
fn lock_volume(dir: &Path) -> Result<std::fs::File, CliError> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("LOCK"))
        .map_err(|e| CliError::Other(format!("cannot open the volume lock: {e}")))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(CliError::Daemon(CtlError::new(
            ErrorCode::Busy,
            "another process holds the volume lock (is the volume attached?)",
        ))),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(CliError::Other(format!("cannot lock the volume: {e}")))
        }
    }
}

/// The highest `keyring_seq` this host recorded for the volume (0 if none).
fn seen_keyring_seq(dir: &Path) -> u64 {
    std::fs::read(dir.join("state/pins"))
        .ok()
        .and_then(|b| LocalPinsFile::decode(&b).ok())
        .map_or(0, |p| p.pins.max_keyring_seq)
}

fn keyring_err(e: keyring::KeyringError) -> CliError {
    use keyring::KeyringError as K;
    match e {
        K::WrongCredential => CliError::Daemon(CtlError::new(
            ErrorCode::WrongKmk,
            "the credential does not unlock the keyring (wrong credential, or a damaged keyring)",
        )),
        K::Rollback => CliError::Daemon(CtlError::new(
            ErrorCode::Rollback,
            "the keyring is older than the highest keyring_seq this host has seen",
        )),
        K::OverPolicy => CliError::Other(
            "every slot of that kind costs more to unlock than this command allows".to_owned(),
        ),
        K::Invalid(why) => CliError::Usage(why.to_owned()),
        K::BelowFloor => CliError::Usage("argon2id parameters are below the floor".to_owned()),
        K::Unsupported(what) => CliError::Other(format!("unsupported: {what}")),
        K::Format(f) => CliError::Other(format!("the keyring is damaged: {f}")),
        K::Io(e) => CliError::Other(format!("i/o error: {e}")),
        _ => CliError::Other("keyring error".to_owned()),
    }
}

/// Unwraps the root with `cred` and checks that it belongs to the volume: the
/// descriptor MAC must verify under it.
fn unlock_root(vol: &Vol, cred: &Cred) -> Result<lunix_crypto::Root, CliError> {
    let unlocked = keyring::unlock(
        &vol.keyring_bytes,
        vol.desc.profile,
        &vol.desc.family_id,
        &cred.unlock(),
        seen_keyring_seq(&vol.dir),
        &UnlockOptions::default(),
    )
    .map_err(keyring_err)?;
    if unlocked.root.mode() != vol.desc.crypto_mode
        || !verify_descriptor_mac(&vol.desc, Some(&unlocked.root))
    {
        return Err(CliError::Daemon(CtlError::new(
            ErrorCode::WrongKmk,
            "the credential unlocks a key that does not belong to this volume",
        )));
    }
    Ok(unlocked.root)
}

/// Replaces the keyring: a temporary file beside it (same mode), `fsync`,
/// rename, `fsync` of the directory.
fn replace_keyring(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mode = std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o600);
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the keyring has no directory"))?;
    let tmp = dir.join(format!("keyring.tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        std::fs::File::open(dir)?.sync_all()
    };
    let r = write();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

fn add_slot(a: KeyAddArgs, socket: &Path, out: &mut dyn Write) -> Result<(), CliError> {
    let (dir, desc) = open_vol(&a.name, a.dir.as_deref(), socket)?;
    if !desc.crypto_mode.is_keyed() {
        return Err(CliError::Usage(
            "a plain volume has no key slots, so there is nothing to add".to_owned(),
        ));
    }
    if a.key_file.is_some() && a.pass.wanted() {
        return Err(CliError::Usage(
            "give one of --key-file and --passphrase for the new slot, not both".to_owned(),
        ));
    }
    if a.key_file.is_none() && !a.pass.wanted() {
        return Err(CliError::Usage(
            "say what the new slot is: --passphrase (or --passphrase-fd N), or --key-file FILE"
                .to_owned(),
        ));
    }
    if a.pass.passphrase_fd.is_some() && a.pass.passphrase_fd == a.unlock_passphrase_fd {
        return Err(CliError::Usage(
            "--passphrase-fd and --unlock-passphrase-fd need two different descriptors".to_owned(),
        ));
    }
    let vol = load_keyring(&a.name, dir, desc)?;
    let cred = unlock_cred(
        a.unlock_key_file.as_deref(),
        a.unlock_passphrase_fd,
        &vol.dir,
    )?;
    // The new slot's secret.
    let new_pass = a.pass.read(true)?;
    let (new_key, create_key_file) = match &a.key_file {
        None => (None, false),
        Some(p) if p.exists() => (Some(read_key_file(p, "--key-file")?), false),
        Some(_) => (Some(KeyFile::generate().map_err(keyring_err)?), true),
    };
    let _lock = lock_volume(&vol.dir)?;
    let root = unlock_root(&vol, &cred)?;
    let new_slot = match (&new_pass, &new_key) {
        (Some(p), _) => NewSlot::Passphrase {
            passphrase: p.expose(),
            cost: Argon2Cost::DEFAULT,
        },
        (None, Some(k)) => NewSlot::KeyFile(k),
        (None, None) => return Err(CliError::Usage("no new slot secret".to_owned())),
    };
    let (new_kr, id) =
        keyring::add_slot(&vol.keyring, vol.desc.profile, &root, &new_slot).map_err(keyring_err)?;
    let bytes = keyring::encode(&new_kr).map_err(keyring_err)?;
    // A key file the keyring will name is made durable first, and removed
    // again when the keyring cannot be replaced.
    let key_path = a.key_file.as_deref().filter(|_| create_key_file);
    if let (Some(p), Some(k)) = (key_path, &new_key) {
        k.write_new(p).map_err(|e| {
            CliError::Other(format!("cannot write the key file {}: {e}", p.display()))
        })?;
    }
    if let Err(e) = replace_keyring(&vol.keyring_path, &bytes) {
        if let Some(p) = key_path {
            let _ = std::fs::remove_file(p);
        }
        return Err(CliError::Other(format!("cannot replace the keyring: {e}")));
    }
    let kind = if new_pass.is_some() {
        "passphrase"
    } else {
        "key file"
    };
    writeln!(
        out,
        "added {kind} slot {id} to {} (keyring_seq {})",
        sanitize(&vol.name),
        new_kr.keyring_seq
    )?;
    if let Some(p) = key_path {
        writeln!(
            out,
            "wrote the new key file {} (mode 0600); keep a copy, nothing else can recreate it",
            p.display()
        )?;
    }
    Ok(())
}

fn rm_slot(a: KeyRmArgs, socket: &Path, out: &mut dyn Write) -> Result<(), CliError> {
    let (dir, desc) = open_vol(&a.name, a.dir.as_deref(), socket)?;
    if !desc.crypto_mode.is_keyed() {
        return Err(CliError::Usage(
            "a plain volume has no key slots, so there is nothing to remove".to_owned(),
        ));
    }
    let vol = load_keyring(&a.name, dir, desc)?;
    if !vol.keyring.slots.iter().any(|s| s.slot_id == a.slot) {
        return Err(CliError::Usage(format!(
            "slot {} does not exist (see `lunix key ls {}`)",
            a.slot,
            sanitize(&a.name)
        )));
    }
    if vol.keyring.slots.len() == 1 {
        return Err(CliError::Usage(
            "the last slot cannot be removed: the volume would be locked for good".to_owned(),
        ));
    }
    let cred = unlock_cred(
        a.unlock_key_file.as_deref(),
        a.unlock_passphrase_fd,
        &vol.dir,
    )?;
    let _lock = lock_volume(&vol.dir)?;
    let root = unlock_root(&vol, &cred)?;
    let new_kr =
        keyring::remove_slot(&vol.keyring, vol.desc.profile, &root, a.slot).map_err(keyring_err)?;
    let bytes = keyring::encode(&new_kr).map_err(keyring_err)?;
    replace_keyring(&vol.keyring_path, &bytes)
        .map_err(|e| CliError::Other(format!("cannot replace the keyring: {e}")))?;
    writeln!(
        out,
        "removed slot {} from {} (keyring_seq {})",
        a.slot,
        sanitize(&vol.name),
        new_kr.keyring_seq
    )?;
    writeln!(
        out,
        "note: the volume's root key is unchanged; whoever already used the removed credential may have kept the key"
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use lunix_crypto::keyring::Unlock;
    use lunix_crypto::seal_descriptor;
    use lunix_format::local::VolumeJson;
    use lunix_format::volume::CodecPolicy;
    use lunix_format::{FamilyId, IdLen, Profile, TimeMs, VolumeUuid};
    use lunix_iface::ctl::{Outcome, Response, VolumeState};
    use lunix_testkit::FakeCtlServer;

    use super::*;

    const CANARY: &[u8] = b"canary-pass-7f3a91c2";
    const UUID: [u8; 16] = [0x21; 16];
    const FAMILY: [u8; 16] = [0x42; 16];

    struct Fixture {
        // The server drops first (its shutdown connects to the socket path).
        server: FakeCtlServer,
        tmp: tempfile::TempDir,
        mode: CryptoMode,
        key_file: PathBuf,
    }

    impl Fixture {
        fn root(&self) -> PathBuf {
            self.tmp.path().join("vols")
        }
        fn vol_dir(&self) -> PathBuf {
            self.root().join(VolumeUuid(UUID).to_string())
        }
        fn keyring(&self) -> PathBuf {
            PathBuf::from(local_keyring_path(
                &self.root().to_string_lossy(),
                &FamilyId(FAMILY),
            ))
        }
        fn args(&self, cmd: KeyCommand) -> KeyArgs {
            KeyArgs {
                command: cmd,
                socket: self.server.path().to_owned(),
            }
        }
        fn run(&self, cmd: KeyCommand) -> (u8, String, String) {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_key_to(self.args(cmd), &mut out, &mut err);
            (
                exit_of(code),
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        }
        fn detached(&self) {
            self.status(VolumeState::Detached);
        }
        fn status(&self, state: VolumeState) {
            self.server.respond(
                "VolumeStatus",
                Outcome::Ok(Response::VolumeStatus(Box::new(
                    crate::fsck::status_for_test("v1", state),
                ))),
            );
        }
        fn keyring_seq(&self) -> u64 {
            Keyring::decode(&std::fs::read(self.keyring()).unwrap())
                .unwrap()
                .keyring_seq
        }
        fn slot_ids(&self) -> Vec<u32> {
            Keyring::decode(&std::fs::read(self.keyring()).unwrap())
                .unwrap()
                .slots
                .iter()
                .map(|s| s.slot_id)
                .collect()
        }
    }

    fn exit_of(c: ExitCode) -> u8 {
        // ExitCode has no accessor; compare through its Debug text.
        let text = format!("{c:?}");
        text.rsplit(|ch: char| !ch.is_ascii_digit())
            .find(|t| !t.is_empty())
            .and_then(|t| t.parse().ok())
            .unwrap_or(255)
    }

    /// A volume directory and keyring built with the real crates: integrity or
    /// encrypted, one key-file slot (`integrity.key` in the volume directory).
    fn fixture(mode: CryptoMode) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let server = FakeCtlServer::start(&tmp.path().join("ctl.sock")).unwrap();
        let root = tmp.path().join("vols");
        let dir = root.join(VolumeUuid(UUID).to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let key_file = dir.join("integrity.key");
        let kind = if mode == CryptoMode::Encrypted {
            RootKind::Kmk
        } else {
            RootKind::Ik
        };
        let kf = KeyFile::generate().unwrap();
        kf.write_new(&key_file).unwrap();
        let created = if mode == CryptoMode::Plain {
            None
        } else {
            Some(
                keyring::create(
                    Profile::Default,
                    FamilyId(FAMILY),
                    kind,
                    &NewSlot::KeyFile(&kf),
                )
                .unwrap(),
            )
        };
        let mut desc = VolumeDescriptor {
            crypto_mode: mode,
            profile: Profile::Default,
            block_size: 4096,
            chunk_size: 65536,
            map_fanout: 256,
            id_len: IdLen::Sixteen,
            codec_policy: CodecPolicy::Auto,
            codec_level: 1,
            volume_uuid: VolumeUuid(UUID),
            family_id: FamilyId(FAMILY),
            kmk_fingerprint: [0; 16],
            created_at: TimeMs(1_794_441_600_000),
            lineage: Vec::new(),
            mac: [0; 32],
        };
        let bytes = seal_descriptor(&mut desc, created.as_ref().map(|c| &c.root)).unwrap();
        std::fs::write(dir.join("volume"), bytes).unwrap();
        let json = VolumeJson::new("v1", &dir.to_string_lossy());
        std::fs::write(dir.join("volume.json"), json.to_json_pretty().unwrap()).unwrap();
        if let Some(c) = created {
            let kr = PathBuf::from(local_keyring_path(
                &root.to_string_lossy(),
                &FamilyId(FAMILY),
            ));
            std::fs::create_dir_all(kr.parent().unwrap()).unwrap();
            std::fs::write(&kr, c.bytes).unwrap();
        }
        let f = Fixture {
            server,
            tmp,
            mode,
            key_file,
        };
        f.detached();
        f
    }

    fn ls_args(f: &Fixture, json: bool) -> KeyCommand {
        KeyCommand::Ls(KeyLsArgs {
            name: "v1".into(),
            dir: Some(f.root()),
            json,
        })
    }

    fn add_args(f: &Fixture) -> KeyAddArgs {
        KeyAddArgs {
            name: "v1".into(),
            dir: Some(f.root()),
            pass: PassphraseArgs::default(),
            key_file: None,
            unlock_key_file: None,
            unlock_passphrase_fd: None,
        }
    }

    fn rm_args(f: &Fixture, slot: u32) -> KeyRmArgs {
        KeyRmArgs {
            name: "v1".into(),
            slot,
            dir: Some(f.root()),
            unlock_key_file: None,
            unlock_passphrase_fd: None,
        }
    }

    /// A file holding `secret`, kept open so its descriptor number is valid.
    fn secret_fd(dir: &Path, name: &str, secret: &[u8]) -> (std::fs::File, u32) {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(secret).unwrap();
        f.write_all(b"\n").unwrap();
        f.sync_all().unwrap();
        let ro = std::fs::File::open(&path).unwrap();
        let fd = u32::try_from(ro.as_raw_fd()).unwrap();
        (ro, fd)
    }

    #[test]
    fn ls_lists_the_slots_as_text_and_json() {
        let f = fixture(CryptoMode::Integrity);
        let (code, out, _) = f.run(ls_args(&f, false));
        assert_eq!(code, 0);
        assert!(out.contains("slot 1"), "{out}");
        assert!(out.contains("key file"), "{out}");
        assert!(out.contains("keyring_seq 1"), "{out}");
        let (code, out, _) = f.run(ls_args(&f, true));
        assert_eq!(code, 0);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mode"], "integrity");
        assert_eq!(v["slots"][0]["id"], 1);
        assert_eq!(v["slots"][0]["kind"], "key_file");
    }

    #[test]
    fn a_plain_volume_has_no_slots_and_says_so() {
        let f = fixture(CryptoMode::Plain);
        let (code, out, _) = f.run(ls_args(&f, false));
        assert_eq!(code, 0);
        assert!(out.contains("plain volume has no key slots"), "{out}");
        let (code, _, err) = f.run(KeyCommand::AddSlot(KeyAddArgs {
            key_file: Some(f.tmp.path().join("k2")),
            ..add_args(&f)
        }));
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("plain volume has no key slots"), "{err}");
        let (code, _, err) = f.run(KeyCommand::RmSlot(rm_args(&f, 1)));
        assert_eq!(code, 2, "{err}");
        assert_eq!(f.mode, CryptoMode::Plain);
    }

    #[test]
    fn add_passphrase_slot_then_it_unlocks_and_the_canary_never_leaks() {
        let f = fixture(CryptoMode::Encrypted);
        let (_hold, fd) = secret_fd(f.tmp.path(), "new-pass", CANARY);
        let (code, out, err) = f.run(KeyCommand::AddSlot(KeyAddArgs {
            pass: PassphraseArgs {
                passphrase: false,
                passphrase_fd: Some(fd),
            },
            ..add_args(&f)
        }));
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("added passphrase slot 2"), "{out}");
        assert_eq!(f.keyring_seq(), 2);
        // The canary is nowhere: output, keyring, arguments, environment.
        let hay = |h: &[u8]| h.windows(CANARY.len()).any(|w| w == CANARY);
        assert!(!hay(out.as_bytes()) && !hay(err.as_bytes()));
        assert!(!hay(&std::fs::read(f.keyring()).unwrap()));
        assert!(!hay(&std::fs::read("/proc/self/cmdline").unwrap()));
        assert!(!hay(
            &std::fs::read("/proc/self/environ").unwrap_or_default()
        ));
        // The new slot unlocks the volume's root.
        let bytes = std::fs::read(f.keyring()).unwrap();
        let ok = keyring::unlock(
            &bytes,
            Profile::Default,
            &FamilyId(FAMILY),
            &Unlock::Passphrase(CANARY),
            0,
            &UnlockOptions::default(),
        );
        assert!(ok.is_ok());
        // ls shows both slots, the passphrase one with its cost.
        let (_, out, _) = f.run(ls_args(&f, false));
        assert!(out.contains("passphrase  argon2id"), "{out}");
        // The old key file still works: remove slot 1 with the new passphrase.
        let (_hold2, fd2) = secret_fd(f.tmp.path(), "unlock-pass", CANARY);
        let (code, out, err) = f.run(KeyCommand::RmSlot(KeyRmArgs {
            unlock_passphrase_fd: Some(fd2),
            ..rm_args(&f, 1)
        }));
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("removed slot 1"), "{out}");
        assert_eq!(f.slot_ids(), vec![2]);
        assert_eq!(f.keyring_seq(), 3);
        assert!(!hay(out.as_bytes()));
    }

    #[test]
    fn add_key_file_slot_writes_a_0600_key_file_and_it_unlocks() {
        let f = fixture(CryptoMode::Integrity);
        let new_key = f.tmp.path().join("second.key");
        let (code, out, err) = f.run(KeyCommand::AddSlot(KeyAddArgs {
            key_file: Some(new_key.clone()),
            ..add_args(&f)
        }));
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("added key file slot 2"), "{out}");
        let mode = std::fs::metadata(&new_key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(f.slot_ids(), vec![1, 2]);
        // An existing key file is used as it is.
        let kf = KeyFile::read(&new_key).unwrap();
        let bytes = std::fs::read(f.keyring()).unwrap();
        assert!(keyring::unlock(
            &bytes,
            Profile::Default,
            &FamilyId(FAMILY),
            &Unlock::KeyFile(&kf),
            0,
            &UnlockOptions::default()
        )
        .is_ok());
        // Remove slot 1 unlocking with the new file; then the default
        // integrity.key no longer opens anything but the file is still there.
        let (code, _, err) = f.run(KeyCommand::RmSlot(KeyRmArgs {
            unlock_key_file: Some(new_key),
            ..rm_args(&f, 1)
        }));
        assert_eq!(code, 0, "{err}");
        assert_eq!(f.slot_ids(), vec![2]);
    }

    #[test]
    fn the_last_slot_and_unknown_slots_are_refused() {
        let f = fixture(CryptoMode::Integrity);
        let before = std::fs::read(f.keyring()).unwrap();
        let (code, _, err) = f.run(KeyCommand::RmSlot(rm_args(&f, 1)));
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("last slot"), "{err}");
        let (code, _, err) = f.run(KeyCommand::RmSlot(rm_args(&f, 9)));
        assert_eq!(code, 2, "{err}");
        assert_eq!(std::fs::read(f.keyring()).unwrap(), before);
    }

    #[test]
    fn a_wrong_credential_is_exit_6_and_changes_nothing() {
        let f = fixture(CryptoMode::Integrity);
        let before = std::fs::read(f.keyring()).unwrap();
        let other = f.tmp.path().join("other.key");
        KeyFile::generate().unwrap().write_new(&other).unwrap();
        let (code, _, err) = f.run(KeyCommand::AddSlot(KeyAddArgs {
            key_file: Some(f.tmp.path().join("third.key")),
            unlock_key_file: Some(other),
            ..add_args(&f)
        }));
        assert_eq!(code, 6, "{err}");
        assert_eq!(std::fs::read(f.keyring()).unwrap(), before);
        assert!(!f.tmp.path().join("third.key").exists());
        assert!(f.key_file.exists());
    }

    #[test]
    fn an_attached_volume_is_refused_with_exit_3() {
        let f = fixture(CryptoMode::Integrity);
        f.status(VolumeState::Attached);
        let before = std::fs::read(f.keyring()).unwrap();
        for cmd in [
            ls_args(&f, false),
            KeyCommand::AddSlot(KeyAddArgs {
                key_file: Some(f.tmp.path().join("k2")),
                ..add_args(&f)
            }),
            KeyCommand::RmSlot(rm_args(&f, 1)),
        ] {
            let (code, _, err) = f.run(cmd);
            assert_eq!(code, 3, "{err}");
            assert!(err.contains("attached"), "{err}");
        }
        assert_eq!(std::fs::read(f.keyring()).unwrap(), before);
    }

    #[test]
    fn a_held_volume_lock_is_exit_3_without_a_daemon() {
        let f = fixture(CryptoMode::Integrity);
        // No daemon answer: the status call is unreachable, the LOCK decides.
        f.server.respond(
            "VolumeStatus",
            Outcome::Err(CtlError::new(ErrorCode::NotFound, "no such volume")),
        );
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(f.vol_dir().join("LOCK"))
            .unwrap();
        held.try_lock().unwrap();
        let (code, _, err) = f.run(KeyCommand::AddSlot(KeyAddArgs {
            key_file: Some(f.tmp.path().join("k2")),
            ..add_args(&f)
        }));
        assert_eq!(code, 3, "{err}");
        assert_eq!(f.slot_ids(), vec![1]);
        drop(held);
    }

    #[test]
    fn an_unknown_volume_is_exit_4_and_bad_option_mixes_are_usage_errors() {
        let f = fixture(CryptoMode::Integrity);
        let (code, _, err) = f.run(KeyCommand::Ls(KeyLsArgs {
            name: "nope".into(),
            dir: Some(f.root()),
            json: false,
        }));
        assert_eq!(code, 4, "{err}");
        // Neither a passphrase nor a key file for the new slot.
        let (code, _, _) = f.run(KeyCommand::AddSlot(add_args(&f)));
        assert_eq!(code, 2);
        // Both.
        let (code, _, _) = f.run(KeyCommand::AddSlot(KeyAddArgs {
            pass: PassphraseArgs {
                passphrase: true,
                passphrase_fd: None,
            },
            key_file: Some(f.tmp.path().join("k2")),
            ..add_args(&f)
        }));
        assert_eq!(code, 2);
        // Both unlock options.
        let (code, _, _) = f.run(KeyCommand::RmSlot(KeyRmArgs {
            unlock_key_file: Some(f.key_file.clone()),
            unlock_passphrase_fd: Some(3),
            ..rm_args(&f, 1)
        }));
        assert_eq!(code, 2);
    }
}
