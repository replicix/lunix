//! Running the socket subcommands.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;

use lunix_iface::ctl::{
    Attach, Credential, GcRequest, NameParam, Request, Resize, Response, SpaceRequest,
    VolumeCreate, VolumeStatusBody,
};
use serde::Serialize;

use crate::cli::{
    AttachArgs, Cli, Command, CreateArgs, CredentialMode, GcArgs, ListArgs, ResizeArgs, SpaceArgs,
    StatusArgs, VolumeCommand,
};
use crate::client::Client;
use crate::error::{attach_hint, CliError, EXIT_GENERIC};
use crate::fsck::run_fsck;
use crate::key::run_key;
use crate::render;

/// An error plus the advice that goes with it.
#[derive(Debug)]
pub struct Failure {
    /// The error.
    pub error: CliError,
    /// A second line of advice.
    pub hint: Option<String>,
}

impl From<CliError> for Failure {
    fn from(error: CliError) -> Self {
        Self { error, hint: None }
    }
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        CliError::from(e).into()
    }
}

/// Runs a parsed command line, writing results to `out` and diagnostics to
/// `err`, and returns the process exit code.
pub fn run(cli: Cli, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    let Cli { socket, command } = cli;
    let result = match command {
        Command::Fsck(mut a) => {
            a.socket = socket;
            return run_fsck(a);
        }
        Command::Key(mut a) => {
            a.socket = socket;
            return run_key(a);
        }
        other => dispatch(&socket, other, out, err),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            let code = f.error.exit_code();
            let _ = writeln!(err, "lunix: {}: {}", f.error.class(), f.error);
            if let Some(h) = f.hint {
                let _ = writeln!(err, "hint: {h}");
            }
            ExitCode::from(if code == 0 { EXIT_GENERIC } else { code })
        }
    }
}

fn dispatch(
    socket: &Path,
    command: Command,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let mut c = Client::connect(socket)?;
    match command {
        Command::Volume(v) => match v.command {
            VolumeCommand::Create(a) => create(&mut c, a, out, err),
            VolumeCommand::List(a) => list(&mut c, &a, out),
        },
        Command::Attach(a) => attach(&mut c, a, out, err),
        Command::Detach(a) => {
            expect_detached(c.call(Request::Detach(NameParam {
                name: a.name.clone(),
            }))?)?;
            writeln!(out, "detached {}", crate::error::sanitize(&a.name))?;
            Ok(())
        }
        Command::Status(a) => status(&mut c, &a, out),
        Command::Resize(a) => resize(&mut c, a, out),
        Command::Space(a) => space(&mut c, a, out),
        Command::Gc(a) => gc(&mut c, &a, out),
        Command::Fsck(_) | Command::Key(_) => Err(CliError::Other("unreachable".into()).into()),
    }
}

fn unexpected(what: &str) -> CliError {
    CliError::Protocol(format!("unexpected reply to {what}"))
}

fn expect_detached(r: Response) -> Result<(), CliError> {
    match r {
        Response::Detached => Ok(()),
        _ => Err(unexpected("detach")),
    }
}

fn json_out<T: Serialize>(out: &mut dyn Write, v: &T) -> Result<(), Failure> {
    let s = serde_json::to_string_pretty(v)
        .map_err(|_| CliError::Other("cannot encode the output".into()))?;
    writeln!(out, "{s}")?;
    Ok(())
}

/// Makes a path absolute (the daemon's working directory is not ours) and
/// checks it is valid UTF-8.
fn abs(p: &Path, what: &str) -> Result<String, CliError> {
    let a = std::path::absolute(p)
        .map_err(|e| CliError::Usage(format!("{what}: cannot make the path absolute: {e}")))?;
    a.into_os_string()
        .into_string()
        .map_err(|_| CliError::Usage(format!("{what}: the path is not valid UTF-8")))
}

fn create(
    c: &mut Client,
    a: CreateArgs,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let mode = a.mode.mode();
    let mut req = VolumeCreate {
        name: a.name,
        dir: abs(&a.dir, "--dir")?,
        size_bytes: a.size,
        chunk_size: a.chunk,
        mode,
        compress: a.compress,
        id_len: a.id_len,
        map_fanout: None,
        key_file: a
            .key_file
            .as_deref()
            .map(|p| abs(p, "--key-file"))
            .transpose()?,
        passphrase: None,
    };
    // Check everything but the secret before prompting for it.
    Request::VolumeCreate(req.clone())
        .validate()
        .map_err(|e| CliError::Usage(e.to_string()))?;
    if mode == lunix_iface::ctl::VolumeMode::Plain {
        writeln!(err, "{}", crate::warn::PLAIN_CREATE_WARNING)?;
    }
    req.passphrase = a.pass.read(true)?;
    match c.call(Request::VolumeCreate(req))? {
        Response::VolumeCreated(v) => {
            render::created(out, &v)?;
            if let Some(k) = &v.key_file {
                if let Some(w) = key_file_warning(Path::new(k)) {
                    writeln!(err, "{w}")?;
                }
            }
            Ok(())
        }
        _ => Err(unexpected("volume create").into()),
    }
}

/// A warning when the generated key file can be read by others. Silent when
/// the file cannot be inspected from here (another user's directory).
pub fn key_file_warning(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then(|| {
        format!(
            "warning: the key file {} has mode {mode:04o}; it should be 0600",
            crate::error::sanitize(&path.display().to_string())
        )
    })
}

fn list(c: &mut Client, a: &ListArgs, out: &mut dyn Write) -> Result<(), Failure> {
    match c.call(Request::VolumeList)? {
        Response::VolumeList { volumes } => {
            if a.json {
                json_out(out, &volumes)
            } else {
                render::volume_list(out, &volumes)?;
                Ok(())
            }
        }
        _ => Err(unexpected("volume list").into()),
    }
}

fn attach(
    c: &mut Client,
    a: AttachArgs,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let name = a.name.clone();
    let key_file = a
        .key_file
        .as_deref()
        .map(|p| abs(p, "--key-file"))
        .transpose()?;
    let mut req = Attach {
        name: a.name,
        queues: a.queues,
        depth: a.depth,
        cpus: a.cpus,
        max_io_buf_bytes: a.max_io_buf,
        credential: None,
        on_journal_damage: a.on_journal_damage.map(Into::into),
        allow_unverifiable_fork: a.allow_unverifiable_fork,
    };
    let wants_cred = key_file.is_some()
        || a.pass.wanted()
        || a.fingerprint.is_some()
        || a.credential_mode.is_some();
    let cred = |passphrase| Credential {
        passphrase,
        key_file: key_file.clone(),
        fingerprint: a.fingerprint.clone(),
        mode: a.credential_mode.map(|m| match m {
            CredentialMode::Encrypted => lunix_iface::ctl::VolumeMode::Encrypted,
            CredentialMode::Integrity => lunix_iface::ctl::VolumeMode::Integrity,
        }),
    };
    if wants_cred {
        req.credential = Some(cred(None));
    }
    Request::Attach(req.clone())
        .validate()
        .map_err(|e| CliError::Usage(e.to_string()))?;
    if wants_cred {
        req.credential = Some(cred(a.pass.read(false)?));
    }
    match c.call(Request::Attach(req)) {
        Ok(Response::Attached(at)) => {
            render::attached(out, &name, &at)?;
            if at.first_attach {
                writeln!(err, "{}", render::FIRST_ATTACH_NOTE)?;
            }
            Ok(())
        }
        Ok(_) => Err(unexpected("attach").into()),
        Err(CliError::Daemon(e)) => {
            let hint = attach_hint(e.code, &name);
            Err(Failure {
                error: CliError::Daemon(e),
                hint,
            })
        }
        Err(e) => Err(e.into()),
    }
}

fn one_status(c: &mut Client, name: &str) -> Result<VolumeStatusBody, CliError> {
    match c.call(Request::VolumeStatus(NameParam {
        name: name.to_owned(),
    }))? {
        Response::VolumeStatus(s) => Ok(*s),
        _ => Err(unexpected("status")),
    }
}

fn status(c: &mut Client, a: &StatusArgs, out: &mut dyn Write) -> Result<(), Failure> {
    if let Some(name) = &a.name {
        let s = one_status(c, name)?;
        return if a.json {
            json_out(out, &s)
        } else {
            render::status(out, &s)?;
            Ok(())
        };
    }
    let volumes = match c.call(Request::VolumeList)? {
        Response::VolumeList { volumes } => volumes,
        _ => return Err(unexpected("volume list").into()),
    };
    let mut all = Vec::with_capacity(volumes.len());
    for v in &volumes {
        all.push(one_status(c, &v.name)?);
    }
    if a.json {
        return json_out(out, &all);
    }
    if all.is_empty() {
        writeln!(out, "no volumes")?;
    }
    for (i, s) in all.iter().enumerate() {
        if i > 0 {
            writeln!(out)?;
        }
        render::status(out, s)?;
    }
    Ok(())
}

fn resize(c: &mut Client, a: ResizeArgs, out: &mut dyn Write) -> Result<(), Failure> {
    match c.call(Request::Resize(Resize {
        name: a.name.clone(),
        size_bytes: a.size,
    }))? {
        Response::Resized { size_bytes } => {
            writeln!(
                out,
                "resized {} to {}",
                crate::error::sanitize(&a.name),
                crate::parse::format_bytes(size_bytes)
            )?;
            Ok(())
        }
        _ => Err(unexpected("resize").into()),
    }
}

fn space(c: &mut Client, a: SpaceArgs, out: &mut dyn Write) -> Result<(), Failure> {
    let req = SpaceRequest {
        name: a.name.clone(),
        free_floor: a.floor,
        on_full: a.on_full.map(Into::into),
        full_timeout_ms: a.full_timeout,
        cache_max: a.cache_max,
    };
    match c.call(Request::Space(req))? {
        Response::Space(b) => {
            if a.json {
                json_out(out, &b)
            } else {
                render::space(out, &a.name, &b)?;
                Ok(())
            }
        }
        _ => Err(unexpected("space").into()),
    }
}

fn gc(c: &mut Client, a: &GcArgs, out: &mut dyn Write) -> Result<(), Failure> {
    if a.status {
        return match c.call(Request::GcStatus(NameParam {
            name: a.name.clone(),
        }))? {
            Response::GcStatus(g) => {
                if a.json {
                    json_out(out, &g)
                } else {
                    render::gc_status(out, &a.name, &g)?;
                    Ok(())
                }
            }
            _ => Err(unexpected("gc status").into()),
        };
    }
    match c.call(Request::Gc(GcRequest {
        name: a.name.clone(),
        dry_run: a.dry_run,
    }))? {
        Response::Gc(g) => {
            if a.json {
                json_out(out, &g)
            } else {
                render::gc_summary(out, &a.name, &g)?;
                Ok(())
            }
        }
        _ => Err(unexpected("gc").into()),
    }
}
