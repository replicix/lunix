//! `lunix fsck NAME`: the offline checker and repairer of `lunix-fsck`.
//!
//! The volume must be detached. The command finds the volume directory (with
//! `--dir`, by name in `<dir>/<uuid>/volume.json`; without it, from the
//! daemon), refuses a volume the daemon reports as attached, and hands the
//! directory to [`lunix_fsck::check`], which takes the volume `LOCK` itself.
//!
//! Exit codes are the library's for the outcome of a run: 0 clean, 1 warnings
//! only, 2 errors found. Two outcomes where nothing could be checked follow the
//! `lunix` table instead, so a monitor does not read them as damage: 3 when the
//! volume `LOCK` is held (the library's `busy` finding), 6 when no credential
//! unlocks the volume (`credential`, `keyring_rollback`). Failures before the
//! check: 3 attached, 4 not found; a bad option value is 64, as in the
//! `lunix-fsck` binary.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::Args;
use lunix_fsck::{Credentials, Options, Report};
use lunix_iface::ctl::{CtlError, ErrorCode, NameParam, Request, Response, VolumeState};

use crate::client::Client;
use crate::error::{sanitize, CliError};
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

    /// With --repair: delete unreferenced segments and packs (only after the
    /// whole volume resolved without errors).
    #[arg(long, requires = "repair")]
    pub delete_orphans: bool,

    /// With --repair: replace a rejected `state/pins` by a fresh file whose
    /// floor is the verified tip (gives up the recorded keyring and epoch
    /// floors and the pin; audited).
    #[arg(long, requires = "repair")]
    pub repair_pins: bool,

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

/// How long to wait for the daemon when asking whether a volume is attached.
const DAEMON_WAIT: Duration = Duration::from_secs(30);

/// The largest `volume.json` read while looking for a name.
const MAX_VOLUME_JSON: u64 = 1 << 20;

/// Exit code of a bad option value (the `lunix-fsck` binary's usage code).
const EXIT_FSCK_USAGE: u8 = 64;

/// Runs `lunix fsck`.
pub fn run_fsck(args: FsckArgs) -> ExitCode {
    run_fsck_to(
        args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// [`run_fsck`] with the output streams given.
pub(crate) fn run_fsck_to(args: FsckArgs, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    match fsck(&args, out, err) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let code = match &e {
                CliError::Usage(_) => EXIT_FSCK_USAGE,
                other => other.exit_code(),
            };
            let _ = writeln!(err, "lunix: {}: {}", e.class(), e);
            ExitCode::from(code)
        }
    }
}

fn fsck(args: &FsckArgs, out: &mut dyn Write, err: &mut dyn Write) -> Result<u8, CliError> {
    let dir = locate(&args.name, args.dir.as_deref(), &args.socket)?;
    ensure_detached(&args.name, &args.socket, err)?;
    let passphrase = args.pass.read(false)?;
    let opts = options(args, passphrase);
    let report = lunix_fsck::check(&dir, &opts);
    let code = exit_code(&report);
    if args.json {
        // The document's `exit` is the process exit code.
        let mut doc: serde_json::Value = serde_json::from_str(&report.to_json())
            .map_err(|_| CliError::Other("cannot encode the output".into()))?;
        doc["exit"] = serde_json::Value::from(code);
        let text = serde_json::to_string_pretty(&doc)
            .map_err(|_| CliError::Other("cannot encode the output".into()))?;
        writeln!(out, "{text}")?;
    } else {
        write!(out, "{}", report.render_text())?;
        for h in hints(&report, &args.name) {
            writeln!(out, "hint: {h}")?;
        }
    }
    Ok(code)
}

/// The process exit code of a report: the library's, except that a run that
/// could not look (volume locked, no working credential) is not "errors found".
fn exit_code(report: &Report) -> u8 {
    if report.has("busy") {
        3
    } else if report.has("credential") || report.has("keyring_rollback") {
        6
    } else {
        u8::try_from(report.exit_code()).unwrap_or(2)
    }
}

/// The library options for the command line.
fn options(args: &FsckArgs, passphrase: Option<lunix_iface::ctl::Secret>) -> Options {
    Options {
        deep: args.deep,
        rebuild_index: args.rebuild_index,
        repair: args.repair,
        delete_orphans: args.delete_orphans,
        gc: args.gc,
        truncate_tip: args.truncate_tip,
        reset_floor: args.reset_floor,
        repair_pins: args.repair_pins,
        credentials: Credentials {
            key_file: args.key_file.clone(),
            passphrase,
        },
        ..Options::default()
    }
}

/// Second lines of advice for the findings that have a standard answer.
fn hints(report: &Report, name: &str) -> Vec<String> {
    let mut v = Vec::new();
    if report.has("credential") {
        v.push(
            "give the key with --key-file FILE, or --passphrase (prompt) or --passphrase-fd N"
                .to_owned(),
        );
    }
    if report.has("rollback") {
        v.push(format!(
            "this host recorded a newer state than this copy: restore the newer state; if you verified that this copy is the one \
             you trust, accept it with `lunix fsck {name} --repair --reset-floor`"
        ));
    }
    if report.has("fork") {
        v.push(
            "the history diverged from the one this host recorded; decide which copy to keep before any repair".to_owned(),
        );
    }
    if report.has("state_pins") {
        v.push(format!(
            "state/pins is rejected: `lunix fsck {name} --repair --repair-pins` replaces it from the verified tip (it gives up the recorded floors and pin)"
        ));
    }
    if report.has("index_lost") {
        v.push(format!(
            "`lunix fsck {name} --repair --rebuild-index` rewrites the chunk index from the segment footers"
        ));
    }
    if report.has("gc_unchecked") {
        v.push("GC's checks did not run, so `--repair --gc` will refuse; fix the errors above and run again".to_owned());
    }
    if report.has("busy") {
        v.push(
            "another process holds the volume lock (a daemon, another fsck, or `lunix key`)"
                .to_owned(),
        );
    }
    if report.has("tip_auth") {
        v.push(format!(
            "`lunix fsck {name} --repair --truncate-tip` makes the previous commit the tip (it drops the damaged one)"
        ));
    }
    v
}

fn not_found(msg: String) -> CliError {
    CliError::Daemon(CtlError::new(ErrorCode::NotFound, msg))
}

/// Finds the directory of the volume called `name`.
///
/// With `dir`, that directory is searched: its children `<uuid>/volume.json`
/// (the `--dir` of `volume create`), or the directory itself when it is a
/// volume directory. Without it the daemon is asked.
///
/// # Errors
/// `Usage` for a bad name or when the daemon cannot say; `NotFound` (exit 4)
/// when no volume has the name; `Other` for an ambiguous name or an I/O error.
pub(crate) fn locate(name: &str, dir: Option<&Path>, socket: &Path) -> Result<PathBuf, CliError> {
    Request::VolumeStatus(NameParam {
        name: name.to_owned(),
    })
    .validate()
    .map_err(|e| CliError::Usage(e.to_string()))?;
    match dir {
        Some(d) => scan(name, d),
        None => {
            let mut c = Client::connect_with_timeout(socket, DAEMON_WAIT).map_err(|e| {
                CliError::Usage(format!(
                    "give --dir (the directory the volume was created in): the daemon cannot say where {} is ({e})",
                    sanitize(name)
                ))
            })?;
            match c.call(Request::VolumeList)? {
                Response::VolumeList { volumes } => volumes
                    .into_iter()
                    .find(|v| v.name == name)
                    .map(|v| PathBuf::from(v.dir))
                    .ok_or_else(|| not_found(format!("no volume named {}", sanitize(name)))),
                _ => Err(CliError::Protocol(
                    "unexpected reply to volume list".to_owned(),
                )),
            }
        }
    }
}

/// The volume directory under (or equal to) `root` whose `volume.json` names
/// `name`. Directories that are not uuids or have no valid `volume.json` are
/// skipped, as the daemon's registry rebuild does.
fn scan(name: &str, root: &Path) -> Result<PathBuf, CliError> {
    let root = std::path::absolute(root)
        .map_err(|e| CliError::Usage(format!("--dir: cannot make the path absolute: {e}")))?;
    let mut candidates = Vec::new();
    if root.join("volume.json").is_file() {
        candidates.push(root.clone());
    }
    let rd = std::fs::read_dir(&root)
        .map_err(|e| CliError::Other(format!("cannot read {}: {e}", root.display())))?;
    let mut children: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    children.sort();
    candidates.extend(children);
    let mut found: Vec<PathBuf> = Vec::new();
    for d in candidates {
        let is_uuid = d
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.parse::<lunix_format::VolumeUuid>().is_ok());
        if !is_uuid {
            continue;
        }
        let Some(json) = read_volume_json(&d) else {
            continue;
        };
        if json.name == name && !found.contains(&d) {
            found.push(d);
        }
    }
    match found.len() {
        0 => Err(not_found(format!(
            "no volume named {} under {}",
            sanitize(name),
            root.display()
        ))),
        1 => Ok(found.remove(0)),
        _ => Err(CliError::Other(format!(
            "{} volumes are named {} under {}",
            found.len(),
            sanitize(name),
            root.display()
        ))),
    }
}

fn read_volume_json(dir: &Path) -> Option<lunix_format::local::VolumeJson> {
    let mut text = String::new();
    std::fs::File::open(dir.join("volume.json"))
        .ok()?
        .take(MAX_VOLUME_JSON)
        .read_to_string(&mut text)
        .ok()?;
    lunix_format::local::VolumeJson::from_json(&text).ok()
}

/// Refuses a volume the daemon reports as attached, recovering or read-only.
/// Any other outcome (no daemon, an unknown name, a daemon that hangs up, times
/// out or fails) is not an answer: a warning goes to `err` and the volume
/// `LOCK`, which the library (and `lunix key`) takes, is the real guard. A broken
/// daemon must not block the offline tools.
///
/// # Errors
/// `Busy` (exit 3) when the daemon reports the volume as not detached.
pub(crate) fn ensure_detached(
    name: &str,
    socket: &Path,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let Ok(mut c) = Client::connect_with_timeout(socket, DAEMON_WAIT) else {
        return Ok(());
    };
    match c.call(Request::VolumeStatus(NameParam {
        name: name.to_owned(),
    })) {
        Ok(Response::VolumeStatus(s)) if s.state == VolumeState::Detached => Ok(()),
        Ok(Response::VolumeStatus(_)) => Err(CliError::Daemon(CtlError::new(
            ErrorCode::Busy,
            format!(
                "volume {} is attached; detach it first (`lunix detach {}`): this command works on a detached volume only",
                sanitize(name),
                sanitize(name)
            ),
        ))),
        Err(CliError::Daemon(e)) if e.code == ErrorCode::NotFound => Ok(()),
        Ok(_) | Err(_) => {
            let _ = writeln!(
                err,
                "warning: the daemon gave no usable answer about {}; continuing, the volume lock decides",
                sanitize(name)
            );
            Ok(())
        }
    }
}

/// A status reply for tests of the commands that ask whether a volume is
/// attached.
#[cfg(test)]
pub(crate) fn status_for_test(
    name: &str,
    state: VolumeState,
) -> lunix_iface::ctl::VolumeStatusBody {
    use lunix_iface::ctl::{SpaceFigures, VolumeMode, VolumeStatusBody};
    use lunix_iface::stats::EngineStats;
    VolumeStatusBody {
        name: name.to_owned(),
        uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
        state,
        size_bytes: 1 << 30,
        chunk_size: 65536,
        mode: VolumeMode::Integrity,
        device: None,
        stats: EngineStats::default(),
        space: SpaceFigures::default(),
        throttle: Default::default(),
        journal_used: 0,
        journal_capacity: 1 << 30,
        journal_marks_written: 0,
        journal_marks_skipped: 0,
        locked_bytes: 0,
        last_commit_seq: 1,
        last_commit_ms: None,
        dedup_ratio: None,
        compression_ratio: None,
        rollback_floor: None,
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use lunix_crypto::keyring::{self, NewSlot};
    use lunix_crypto::seal_descriptor;
    use lunix_format::keyring::RootKind;
    use lunix_format::local::VolumeJson;
    use lunix_format::paths::local_keyring_path;
    use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
    use lunix_format::{CryptoMode, FamilyId, IdLen, Profile, TimeMs, VolumeUuid};
    use lunix_iface::ctl::{Outcome, VolumeMode, VolumeSummary};
    use lunix_testkit::FakeCtlServer;

    use super::*;

    const CANARY: &[u8] = b"canary-fsck-5be0d417";
    const UUID: [u8; 16] = [0x31; 16];
    const FAMILY: [u8; 16] = [0x52; 16];

    struct Env {
        // The server drops first (its shutdown connects to the socket path).
        server: FakeCtlServer,
        tmp: tempfile::TempDir,
    }

    impl Env {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let server = FakeCtlServer::start(&tmp.path().join("ctl.sock")).unwrap();
            Self { server, tmp }
        }
        fn root(&self) -> PathBuf {
            self.tmp.path().join("vols")
        }
        fn vol_dir(&self) -> PathBuf {
            self.root().join(VolumeUuid(UUID).to_string())
        }
        fn args(&self, name: &str) -> FsckArgs {
            FsckArgs {
                name: name.to_owned(),
                dir: Some(self.root()),
                deep: false,
                rebuild_index: false,
                repair: false,
                gc: false,
                truncate_tip: false,
                reset_floor: false,
                delete_orphans: false,
                repair_pins: false,
                key_file: None,
                pass: PassphraseArgs::default(),
                json: false,
                socket: self.server.path().to_owned(),
            }
        }
        fn run(&self, args: FsckArgs) -> (u8, String, String) {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_fsck_to(args, &mut out, &mut err);
            (
                exit_of(code),
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        }
        fn state(&self, state: VolumeState) {
            self.server.respond(
                "VolumeStatus",
                Outcome::Ok(Response::VolumeStatus(Box::new(status_for_test(
                    "v1", state,
                )))),
            );
        }
        /// A volume directory with `volume.json` and the given `volume` bytes.
        fn dir_with(&self, volume: Option<&[u8]>) {
            let dir = self.vol_dir();
            std::fs::create_dir_all(&dir).unwrap();
            let json = VolumeJson::new("v1", &dir.to_string_lossy());
            std::fs::write(dir.join("volume.json"), json.to_json_pretty().unwrap()).unwrap();
            if let Some(v) = volume {
                std::fs::write(dir.join("volume"), v).unwrap();
            }
        }
        /// A keyed (integrity) volume whose descriptor and keyring are real
        /// but which has no store layout behind it.
        fn keyed_descriptor_only(&self) {
            let kf = lunix_crypto::KeyFile::generate().unwrap();
            let created = keyring::create(
                Profile::Default,
                FamilyId(FAMILY),
                RootKind::Ik,
                &NewSlot::KeyFile(&kf),
            )
            .unwrap();
            let mut desc = VolumeDescriptor {
                crypto_mode: CryptoMode::Integrity,
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
            let bytes = seal_descriptor(&mut desc, Some(&created.root)).unwrap();
            self.dir_with(Some(&bytes));
            let kr = PathBuf::from(local_keyring_path(
                &self.root().to_string_lossy(),
                &FamilyId(FAMILY),
            ));
            std::fs::create_dir_all(kr.parent().unwrap()).unwrap();
            std::fs::write(kr, created.bytes).unwrap();
        }
    }

    fn exit_of(c: ExitCode) -> u8 {
        let text = format!("{c:?}");
        text.rsplit(|ch: char| !ch.is_ascii_digit())
            .find(|t| !t.is_empty())
            .and_then(|t| t.parse().ok())
            .unwrap_or(255)
    }

    #[test]
    fn every_flag_reaches_the_library_options() {
        let env = Env::new();
        let mut a = env.args("v1");
        a.deep = true;
        a.rebuild_index = true;
        a.repair = true;
        a.gc = true;
        a.truncate_tip = true;
        a.reset_floor = true;
        a.delete_orphans = true;
        a.repair_pins = true;
        a.key_file = Some(PathBuf::from("/k"));
        let o = options(&a, Some(lunix_iface::ctl::Secret::new(CANARY.to_vec())));
        assert!(o.deep && o.rebuild_index && o.repair && o.gc);
        assert!(o.truncate_tip && o.reset_floor && o.delete_orphans && o.repair_pins);
        assert_eq!(o.credentials.key_file, Some(PathBuf::from("/k")));
        assert_eq!(o.credentials.passphrase.as_ref().unwrap().expose(), CANARY);
        // The default run changes nothing.
        let o = options(&env.args("v1"), None);
        assert!(!o.deep && !o.repair && !o.rebuild_index && !o.delete_orphans);
        assert!(o.credentials.passphrase.is_none());
        // Debug of the options never shows the passphrase.
        let dbg = format!(
            "{:?}",
            options(&a, Some(lunix_iface::ctl::Secret::new(CANARY.to_vec())))
        );
        assert!(!dbg.contains("canary"));
    }

    #[test]
    fn repair_flags_need_repair_on_the_command_line() {
        use clap::Parser;
        let parse = |extra: &[&str]| {
            let mut v = vec!["lunix", "fsck", "v1"];
            v.extend_from_slice(extra);
            crate::Cli::try_parse_from(v)
        };
        assert!(parse(&["--repair", "--gc", "--truncate-tip"]).is_ok());
        assert!(parse(&[
            "--repair",
            "--reset-floor",
            "--delete-orphans",
            "--repair-pins"
        ])
        .is_ok());
        for flag in [
            "--gc",
            "--truncate-tip",
            "--reset-floor",
            "--delete-orphans",
            "--repair-pins",
        ] {
            assert!(parse(&[flag]).is_err(), "{flag} without --repair");
        }
        assert!(parse(&["--rebuild-index", "--deep", "--json"]).is_ok());
    }

    #[test]
    fn an_unknown_name_is_exit_4() {
        let env = Env::new();
        env.dir_with(None);
        let (code, out, err) = env.run(env.args("other"));
        assert_eq!(code, 4, "{err}");
        assert!(out.is_empty());
        assert!(err.contains("not_found"), "{err}");
    }

    #[test]
    fn a_bad_name_is_a_usage_error() {
        let env = Env::new();
        let (code, _, err) = env.run(env.args("bad name/../x"));
        assert_eq!(code, 64, "{err}");
    }

    #[test]
    fn an_attached_volume_is_refused_with_exit_3_and_nothing_is_checked() {
        let env = Env::new();
        env.dir_with(Some(b"not a descriptor"));
        for s in [
            VolumeState::Attached,
            VolumeState::Recovering,
            VolumeState::ReadOnly,
        ] {
            env.state(s);
            let mut a = env.args("v1");
            a.repair = true;
            let (code, out, err) = env.run(a);
            assert_eq!(code, 3, "{err}");
            assert!(out.is_empty(), "{out}");
            assert!(err.contains("detach"), "{err}");
        }
    }

    #[test]
    fn without_dir_the_daemon_says_where_the_volume_is() {
        let env = Env::new();
        env.dir_with(Some(b"not a descriptor"));
        env.state(VolumeState::Detached);
        env.server.respond(
            "VolumeList",
            Outcome::Ok(Response::VolumeList {
                volumes: vec![VolumeSummary {
                    name: "v1".to_owned(),
                    uuid: VolumeUuid(UUID).to_string(),
                    dir: env.vol_dir().to_string_lossy().into_owned(),
                    size_bytes: 0,
                    mode: VolumeMode::Integrity,
                    device: None,
                }],
            }),
        );
        let mut a = env.args("v1");
        a.dir = None;
        let (code, out, _) = env.run(a);
        assert_eq!(code, 2, "a damaged descriptor is an error finding");
        assert!(out.contains("volume_parse"), "{out}");
        // A name the daemon does not know is exit 4.
        let mut a = env.args("zzz");
        a.dir = None;
        let (code, _, err) = env.run(a);
        assert_eq!(code, 4, "{err}");
    }

    #[test]
    fn without_dir_and_without_a_daemon_it_asks_for_dir() {
        let env = Env::new();
        let mut a = env.args("v1");
        a.dir = None;
        a.socket = env.tmp.path().join("no-such.sock");
        let (code, _, err) = env.run(a);
        assert_eq!(code, 64, "{err}");
        assert!(err.contains("--dir"), "{err}");
    }

    #[test]
    fn a_daemon_that_fails_does_not_block_fsck() {
        let env = Env::new();
        env.dir_with(Some(&[0u8; 64]));
        env.server.respond(
            "VolumeStatus",
            Outcome::Err(CtlError::new(ErrorCode::Internal, "broken")),
        );
        let (code, out, err) = env.run(env.args("v1"));
        assert_eq!(code, 2, "{err}");
        assert!(out.contains("volume_parse"), "{out}");
        assert!(
            err.contains("warning: the daemon gave no usable answer"),
            "{err}"
        );
    }

    #[test]
    fn a_damaged_volume_is_exit_2_in_text_and_json() {
        let env = Env::new();
        env.dir_with(Some(&[0u8; 64]));
        env.state(VolumeState::Detached);
        let (code, out, err) = env.run(env.args("v1"));
        assert_eq!(code, 2, "{err}");
        assert!(out.contains("volume_parse"), "{out}");
        assert!(out.contains("result: errors"), "{out}");
        let mut a = env.args("v1");
        a.json = true;
        let (code, out, _) = env.run(a);
        assert_eq!(code, 2);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["exit"], 2);
        assert!(v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "volume_parse"));
    }

    #[test]
    fn a_keyed_volume_without_a_credential_says_how_to_give_one() {
        let env = Env::new();
        env.keyed_descriptor_only();
        env.state(VolumeState::Detached);
        let (code, out, _) = env.run(env.args("v1"));
        assert_eq!(
            code, 6,
            "no working credential is exit 6, not 'errors found'"
        );
        assert!(out.contains("credential"), "{out}");
        assert!(out.contains("hint: give the key with --key-file"), "{out}");
    }

    #[test]
    fn a_passphrase_from_a_descriptor_never_reaches_any_output() {
        let env = Env::new();
        env.keyed_descriptor_only();
        env.state(VolumeState::Detached);
        let path = env.tmp.path().join("pw");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&[CANARY, b"\n"].concat())
            .unwrap();
        let held = std::fs::File::open(&path).unwrap();
        let fd = u32::try_from(held.as_raw_fd()).unwrap();
        for json in [false, true] {
            let mut a = env.args("v1");
            a.json = json;
            a.pass = PassphraseArgs {
                passphrase: false,
                passphrase_fd: Some(fd),
            };
            let (code, out, err) = env.run(a);
            assert_eq!(code, 6, "{out}{err}");
            if json {
                let v: serde_json::Value = serde_json::from_str(&out).unwrap();
                assert_eq!(
                    v["exit"], 6,
                    "the document says what the process exits with"
                );
            }
            let hay = |h: &[u8]| h.windows(CANARY.len()).any(|w| w == CANARY);
            assert!(!hay(out.as_bytes()) && !hay(err.as_bytes()));
        }
        drop(held);
    }

    #[test]
    fn name_lookup_skips_other_names_and_junk_and_flags_duplicates() {
        let env = Env::new();
        env.dir_with(None);
        // A junk directory and a second volume with another name.
        std::fs::create_dir_all(env.root().join("not-a-uuid")).unwrap();
        let other = env.root().join(VolumeUuid([0x77; 16]).to_string());
        std::fs::create_dir_all(&other).unwrap();
        let j = VolumeJson::new("other", &other.to_string_lossy());
        std::fs::write(other.join("volume.json"), j.to_json_pretty().unwrap()).unwrap();
        assert_eq!(scan("v1", &env.root()).unwrap(), env.vol_dir());
        assert_eq!(scan("other", &env.root()).unwrap(), other);
        // The volume directory itself is accepted as --dir.
        assert_eq!(scan("v1", &env.vol_dir()).unwrap(), env.vol_dir());
        // Two volumes with one name are ambiguous.
        let j = VolumeJson::new("v1", &other.to_string_lossy());
        std::fs::write(other.join("volume.json"), j.to_json_pretty().unwrap()).unwrap();
        let e = scan("v1", &env.root()).unwrap_err();
        assert_eq!(e.exit_code(), 1);
    }
}
