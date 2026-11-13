//! The audit log: one JSON object per mutating control call (who, what,
//! outcome), appended to `<state_dir>/audit.log` (mode 0600) and logged.
//!
//! Only the method, the volume name and the outcome are recorded, never the
//! parameters: a passphrase or a key never reaches this file (the response
//! messages it copies never contain one either).

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

use lunix_iface::ctl::{ErrorCode, Request};
use serde::Serialize;

/// Who made a call (`SO_PEERCRED`).
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Peer {
    /// User id.
    pub uid: Option<u32>,
    /// Group id.
    pub gid: Option<u32>,
    /// Process id.
    pub pid: Option<i32>,
}

#[derive(Serialize)]
struct AuditLine<'a> {
    ts_ms: u128,
    #[serde(flatten)]
    peer: Peer,
    method: &'a str,
    volume: Option<&'a str>,
    outcome: &'a str,
    code: Option<ErrorCode>,
    message: Option<&'a str>,
}

/// The method name of a request (snake case, as on the wire).
pub fn method(req: &Request) -> &'static str {
    match req {
        Request::Ping => "ping",
        Request::VolumeCreate(_) => "volume_create",
        Request::VolumeList => "volume_list",
        Request::VolumeStatus(_) => "volume_status",
        Request::Attach(_) => "attach",
        Request::Detach(_) => "detach",
        Request::Resize(_) => "resize",
        Request::Space(_) => "space",
        Request::QuiesceAll => "quiesce_all",
        Request::Gc(_) => "gc",
        Request::GcStatus(_) => "gc_status",
    }
}

/// The volume a request names, if any.
pub fn volume(req: &Request) -> Option<&str> {
    match req {
        Request::Ping | Request::VolumeList | Request::QuiesceAll => None,
        Request::VolumeCreate(c) => Some(&c.name),
        Request::VolumeStatus(p) | Request::Detach(p) | Request::GcStatus(p) => Some(&p.name),
        Request::Attach(a) => Some(&a.name),
        Request::Resize(r) => Some(&r.name),
        Request::Space(s) => Some(&s.name),
        Request::Gc(g) => Some(&g.name),
    }
}

/// True for the calls that change something (and are audit-logged). A
/// `space` call that only queries is not.
pub fn is_mutating(req: &Request) -> bool {
    match req {
        Request::Ping | Request::VolumeList | Request::VolumeStatus(_) | Request::GcStatus(_) => {
            false
        }
        Request::Space(s) => {
            s.free_floor.is_some()
                || s.on_full.is_some()
                || s.full_timeout_ms.is_some()
                || s.cache_max.is_some()
        }
        Request::Gc(g) => !g.dry_run,
        _ => true,
    }
}

/// The audit sink.
#[derive(Debug)]
pub struct Audit {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Audit {
    /// An audit log at `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// Records one call. Failures to write are logged, never fatal.
    pub fn record(&self, peer: Peer, req: &Request, result: Result<(), (ErrorCode, &str)>) {
        let (outcome, code, message) = match result {
            Ok(()) => ("ok", None, None),
            Err((c, m)) => ("error", Some(c), Some(m)),
        };
        let line = AuditLine {
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
            peer,
            method: method(req),
            volume: volume(req),
            outcome,
            code,
            message,
        };
        let Ok(mut text) = serde_json::to_string(&line) else {
            return;
        };
        tracing::info!(target: "lunixd::audit", "{text}");
        text.push('\n');
        let _g = crate::lock(&self.lock);
        let res = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = res {
            tracing::warn!(error = %e, path = %self.path.display(), "audit log not written");
        }
    }
}
