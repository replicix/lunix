//! The daemon's configuration (`/etc/lunix/lunixd.toml`, plan M12a step 1).
//!
//! The file is a flat list of `key = value` lines (a strict subset of TOML:
//! strings in double quotes, integers, `true`/`false`, `#` comments; no
//! tables). The workspace has no TOML crate, and a flat file is all the
//! daemon needs; an unknown key, a section header or a malformed value is an
//! error, never ignored.
//!
//! | key | default | meaning |
//! |---|---|---|
//! | `state_dir` | `/var/lib/lunix` | the registry `volumes.json` and the audit log |
//! | `socket` | `/run/lunix/ctl.sock` | the control socket (mode 0600, root) |
//! | `socket_group` | none | a gid (number or `/etc/group` name): mode 0660 with that group |
//! | `metrics` | none | metrics address; deferred in phase 1 (logged and ignored) |
//! | `log_level` | `info` | `error`, `warn`, `info`, `debug`, `trace` |
//! | `device` | `ublk` | `ublk`, or `none` for tests without root (no block device) |
//! | `on_journal_damage` | `wipe` | `wipe` or `refuse` (DESIGN §4.4 step 7) |
//! | `harden` | `true` | `mlockall`, no core dumps, I/O flusher, own mount namespace |
//! | `queues` | `min(8, cpus / 4)` | queues of a new device when the attach names none |
//! | `cpus` | none | default CPU list of the queue threads |
//! | `call_timeout_ms` | 600000 | per-call timeout of the control server |
//! | `quiesce_timeout_ms` | 30000 | `QUIESCE_DEV` wait per device |
//! | `detach_busy_retry_ms` | 3000 | how long a detach retries `TRY_STOP_DEV` (udev's probe) |
//! | `journal_max` | `volume.json` default | `journal_max` of new volumes |
//! | `journal_resident_max` | the volume's `journal_max` | cap on the resident journal window per attached volume (ADR-33): the bytes the journal keeps in memory of what it wrote until the ring file is reclaimed; plaintext, not locked memory; at least 2097152, a larger value than the volume's `journal_max` counts as `journal_max` |
//! | `cut_target_ms` | 1000 | the longest a commit's cut should take (ADR-33): the coordinator cuts at most this long at the drain rate of its last cuts, in journal bytes, never less than one ring file of journal bytes and than what frees a ring file of the fullest lane, never more than half the ring; 10 ..= 60000 |
//! | `commit_interval_ms` | `volume.json` default | commit interval `T` of new volumes |
//! | `argon2_m_kib`, `argon2_t` | 65536, 3 | Argon2id cost of new passphrase slots |
//! | `dev_dir` | `/dev/lunix` | where the `<name>` symlinks go |
//! | `volume_roots` | none | comma-separated directories to scan when the registry is rebuilt |
//! | `max_locked_buffers` | 4 GiB | cap on queues × depth × `max_io_buf_bytes` of one attach |
//! | `idle_trim_ms` | 10000 | once every volume has been idle this long, empty the read caches and trim the heap (0: never) |
//! | `conn_idle_timeout_ms` | 60000 | close a control connection idle this long |
//! | `full_timeout_ms` | 60000 | the wait at the limit of a volume whose `space.full_timeout_ms` is null (ADR-32); 1000 ..= 3600000, a warning above 100000 (the kernel's hung-task threshold is 120 s) |
//! | `store_max_percent` | 200 | `space.cache_max_percent` of new volumes (ADR-32 M2, FREEZE F2); 0 writes no cap, else 100 ..= 10000 |
//! | `throttle` | `true` | the paced admission band below the limit (ADR-32 T3); `false` turns it off, the bounded stop and the latch stay |
//! | `throttle_soft_percent` | 15 | the soft mark, percent of a volume's limit; 1 ..= 50 |
//! | `throttle_soft_min_bytes` | 1073741824 | lower clamp of the soft mark (it stays at most half the limit); at least 67108864 |
//! | `throttle_soft_max_bytes` | 68719476736 | upper clamp of the soft mark; at least `throttle_soft_min_bytes` |
//! | `throttle_min_rate` | 4194304 | `r_min`: the slowest admission rate inside the band, bytes per second per volume; at least 65536 |
//! | `throttle_max_delay_ms` | 10000 | the longest one request is delayed by pacing; 10 ..= `full_timeout_ms` |
//! | `gc_pressure_rate_factor` | 4 | the compaction rate's multiple while writes are slowed (the budget is always 4 times, the grace the floor); 1 ..= 64 |
//!
//! The band's middle rate `r_mid` is `max(gc_rate, 16 MiB/s)` of each volume
//! (`volume.json`), not a daemon key. The per-volume policy keys
//! (`space.cache_max`, `on_full`, `full_timeout_ms`, `gc_rate`) stay in
//! `volume.json`; see `docs/reference/gc.md` and DESIGN section 2.
use std::path::{Path, PathBuf};
use std::time::Duration;

use lunix_engine::OnJournalDamage;

/// Smallest `journal_resident_max`: two maximal journal batches.
pub const MIN_RESIDENT_MAX: u64 = 2 << 20;

/// The range of `cut_target_ms`, milliseconds.
pub const MIN_CUT_TARGET_MS: u64 = 10;
/// See [`MIN_CUT_TARGET_MS`].
pub const MAX_CUT_TARGET_MS: u64 = 60_000;

/// Default config path.
pub const DEFAULT_CONFIG: &str = "/etc/lunix/lunixd.toml";

/// How devices are put in front of an engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceKind {
    /// A real ublk device (root, `ublk_drv`).
    Ublk,
    /// No block device: the engine runs, lanes open, nothing serves I/O.
    /// For the control-plane tests that run without root.
    None,
}

/// A parsed configuration.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    /// State directory.
    pub state_dir: PathBuf,
    /// Control socket path.
    pub socket: PathBuf,
    /// Group allowed on the socket.
    pub socket_group: Option<u32>,
    /// Metrics address (deferred).
    pub metrics: Option<String>,
    /// Log level.
    pub log_level: tracing::Level,
    /// Device backend.
    pub device: DeviceKind,
    /// Journal damage policy.
    pub on_journal_damage: OnJournalDamage,
    /// Process hardening.
    pub harden: bool,
    /// Default queues.
    pub queues: Option<u16>,
    /// Default queue CPU list.
    pub cpus: Option<String>,
    /// Per-call timeout.
    pub call_timeout: Duration,
    /// Quiesce timeout per device.
    pub quiesce_timeout: Duration,
    /// Bounded retry of a busy detach.
    pub detach_busy_retry: Duration,
    /// `journal_max` of new volumes.
    pub journal_max: Option<u64>,
    /// Cap on the resident journal window of an attached volume (`None`: its
    /// `journal_max`).
    pub journal_resident_max: Option<u64>,
    /// The cut target of the attached volumes (ADR-33).
    pub cut_target: Duration,
    /// Commit interval of new volumes.
    pub commit_interval_ms: Option<u64>,
    /// Argon2id memory of new passphrase slots (KiB).
    pub argon2_m_kib: u32,
    /// Argon2id iterations.
    pub argon2_t: u32,
    /// Symlink directory.
    pub dev_dir: PathBuf,
    /// Directories scanned to rebuild the registry.
    pub volume_roots: Vec<PathBuf>,
    /// Cap on the locked tag buffers of one attach.
    pub max_locked_buffers: u64,
    /// Idle time before the heap is trimmed (`None`: never).
    pub idle_trim: Option<Duration>,
    /// Idle control connections are closed after this.
    pub conn_idle_timeout: Duration,
    /// The wait at the limit when a volume's `space.full_timeout_ms` is null.
    pub full_timeout: Duration,
    /// `space.cache_max_percent` of new volumes (0: no cap).
    pub store_max_percent: u32,
    /// The paced band (ADR-32); `mid_rate` is set per volume from its
    /// `gc_rate`.
    pub throttle: lunix_space::ThrottleConfig,
    /// The compaction rate's multiple while writes are slowed.
    pub gc_pressure_rate_factor: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            state_dir: PathBuf::from("/var/lib/lunix"),
            socket: PathBuf::from("/run/lunix/ctl.sock"),
            socket_group: None,
            metrics: None,
            log_level: tracing::Level::INFO,
            device: DeviceKind::Ublk,
            on_journal_damage: OnJournalDamage::Wipe,
            harden: true,
            queues: None,
            cpus: None,
            call_timeout: Duration::from_secs(600),
            quiesce_timeout: Duration::from_secs(30),
            detach_busy_retry: Duration::from_secs(3),
            journal_max: None,
            journal_resident_max: None,
            cut_target: Duration::from_secs(1),
            commit_interval_ms: None,
            argon2_m_kib: 65_536,
            argon2_t: 3,
            dev_dir: PathBuf::from("/dev/lunix"),
            volume_roots: Vec::new(),
            max_locked_buffers: 4 << 30,
            idle_trim: Some(Duration::from_secs(10)),
            conn_idle_timeout: Duration::from_secs(60),
            full_timeout: lunix_space::DEFAULT_FULL_TIMEOUT,
            store_max_percent: 200,
            throttle: lunix_space::ThrottleConfig::default(),
            gc_pressure_rate_factor: 4,
        }
    }
}

/// Why a configuration was refused.
#[derive(Debug, thiserror::Error)]
#[error("config line {line}: {why}")]
pub struct ConfigError {
    /// 1-based line number (0 for the file as a whole).
    pub line: usize,
    /// What is wrong.
    pub why: String,
}

enum Value {
    Str(String),
    Int(u64),
    Bool(bool),
}

fn parse_value(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('"') {
        let body = rest.strip_suffix('"').ok_or("unterminated string")?;
        if body.contains('"') || body.contains('\\') {
            return Err("escapes and inner quotes are not supported".into());
        }
        return Ok(Value::Str(body.to_owned()));
    }
    match raw {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    let digits: String = raw.chars().filter(|c| *c != '_').collect();
    digits
        .parse::<u64>()
        .map(Value::Int)
        .map_err(|_| format!("cannot parse value {raw:?}"))
}

fn strip_comment(line: &str) -> &str {
    // A `#` inside a string ends nothing: strings here never contain one in
    // practice, but be exact anyway.
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

/// A gid from a number or a name in `/etc/group`.
fn resolve_group(s: &str) -> Result<u32, String> {
    if let Ok(n) = s.parse::<u32>() {
        return Ok(n);
    }
    let text = std::fs::read_to_string("/etc/group").map_err(|e| format!("/etc/group: {e}"))?;
    for line in text.lines() {
        let mut f = line.split(':');
        if f.next() == Some(s) {
            let gid = f.nth(1).and_then(|g| g.parse::<u32>().ok());
            return gid.ok_or_else(|| format!("group {s:?} has no numeric gid"));
        }
    }
    Err(format!("no group {s:?}"))
}

impl DaemonConfig {
    /// Parses a configuration text.
    ///
    /// # Errors
    /// [`ConfigError`] for any line that is not a known `key = value`.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut c = Self::default();
        let mut delay_set = false;
        for (i, line) in text.lines().enumerate() {
            let line_no = i + 1;
            let err = |why: String| ConfigError { line: line_no, why };
            let line = strip_comment(line).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                return Err(err("tables are not supported".into()));
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| err("expected key = value".into()))?;
            let key = k.trim();
            delay_set |= key == "throttle_max_delay_ms";
            let val = parse_value(v).map_err(err)?;
            c.set(key, val).map_err(err)?;
        }
        c.check(delay_set)
            .map_err(|why| ConfigError { line: 0, why })?;
        Ok(c)
    }

    /// The rules that span keys.
    fn check(&self, delay_set: bool) -> Result<(), String> {
        let t = &self.throttle;
        if t.soft_max < t.soft_min {
            return Err("throttle_soft_max_bytes is below throttle_soft_min_bytes".into());
        }
        // The default delay cap yields to a shorter full timeout (the gate
        // takes the smaller); one that is set must fit under it.
        if delay_set && t.max_delay > self.full_timeout {
            return Err("throttle_max_delay_ms is above full_timeout_ms".into());
        }
        Ok(())
    }

    fn set(&mut self, key: &str, v: Value) -> Result<(), String> {
        let s = |v: Value| match v {
            Value::Str(s) => Ok(s),
            _ => Err(format!("{key} must be a string")),
        };
        let n = |v: Value| match v {
            Value::Int(n) => Ok(n),
            _ => Err(format!("{key} must be an integer")),
        };
        let b = |v: Value| match v {
            Value::Bool(b) => Ok(b),
            _ => Err(format!("{key} must be true or false")),
        };
        let ms = |v: Value| n(v).map(Duration::from_millis);
        match key {
            "state_dir" => self.state_dir = abs(&s(v)?, key)?,
            "socket" => self.socket = abs(&s(v)?, key)?,
            "socket_group" => {
                self.socket_group = Some(match v {
                    Value::Int(g) => u32::try_from(g).map_err(|_| "gid too large".to_string())?,
                    Value::Str(name) => resolve_group(&name)?,
                    Value::Bool(_) => return Err("socket_group must be a gid or a name".into()),
                });
            }
            "metrics" => self.metrics = Some(s(v)?),
            "log_level" => {
                self.log_level = match s(v)?.as_str() {
                    "error" => tracing::Level::ERROR,
                    "warn" => tracing::Level::WARN,
                    "info" => tracing::Level::INFO,
                    "debug" => tracing::Level::DEBUG,
                    "trace" => tracing::Level::TRACE,
                    other => return Err(format!("unknown log level {other:?}")),
                }
            }
            "device" => {
                self.device = match s(v)?.as_str() {
                    "ublk" => DeviceKind::Ublk,
                    "none" => DeviceKind::None,
                    other => return Err(format!("unknown device kind {other:?}")),
                }
            }
            "on_journal_damage" => {
                self.on_journal_damage = match s(v)?.as_str() {
                    "wipe" => OnJournalDamage::Wipe,
                    "refuse" => OnJournalDamage::Refuse,
                    other => return Err(format!("unknown on_journal_damage {other:?}")),
                }
            }
            "harden" => self.harden = b(v)?,
            "queues" => {
                let q = n(v)?;
                if !(1..=16).contains(&q) {
                    return Err("queues must be 1 to 16".into());
                }
                self.queues = u16::try_from(q).ok();
            }
            "cpus" => self.cpus = Some(s(v)?),
            "call_timeout_ms" => self.call_timeout = ms(v)?,
            "quiesce_timeout_ms" => self.quiesce_timeout = ms(v)?,
            "detach_busy_retry_ms" => self.detach_busy_retry = ms(v)?,
            "journal_max" => self.journal_max = Some(n(v)?),
            "journal_resident_max" => {
                let m = n(v)?;
                if m < MIN_RESIDENT_MAX {
                    return Err(format!(
                        "journal_resident_max must be at least {MIN_RESIDENT_MAX}"
                    ));
                }
                self.journal_resident_max = Some(m);
            }
            "cut_target_ms" => {
                let t = n(v)?;
                if !(MIN_CUT_TARGET_MS..=MAX_CUT_TARGET_MS).contains(&t) {
                    return Err(format!(
                        "cut_target_ms must be {MIN_CUT_TARGET_MS} to {MAX_CUT_TARGET_MS}"
                    ));
                }
                self.cut_target = Duration::from_millis(t);
            }
            "commit_interval_ms" => self.commit_interval_ms = Some(n(v)?),
            "argon2_m_kib" => {
                self.argon2_m_kib = u32::try_from(n(v)?).map_err(|_| "too large".to_string())?;
            }
            "argon2_t" => {
                self.argon2_t = u32::try_from(n(v)?).map_err(|_| "too large".to_string())?;
            }
            "dev_dir" => self.dev_dir = abs(&s(v)?, key)?,
            "volume_roots" => {
                self.volume_roots = s(v)?
                    .split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(|p| abs(p, key))
                    .collect::<Result<_, _>>()?;
            }
            "max_locked_buffers" => self.max_locked_buffers = n(v)?,
            "idle_trim_ms" => {
                let t = n(v)?;
                self.idle_trim = (t > 0).then(|| Duration::from_millis(t));
            }
            "conn_idle_timeout_ms" => self.conn_idle_timeout = ms(v)?,
            "full_timeout_ms" => {
                let t = n(v)?;
                if !(1000..=3_600_000).contains(&t) {
                    return Err("full_timeout_ms must be 1000 to 3600000".into());
                }
                self.full_timeout = Duration::from_millis(t);
            }
            "store_max_percent" => {
                let p = n(v)?;
                if p != 0 && !(100..=10_000).contains(&p) {
                    return Err("store_max_percent must be 0, or 100 to 10000".into());
                }
                self.store_max_percent = u32::try_from(p).map_err(|_| "too large".to_string())?;
            }
            "throttle" => self.throttle.enabled = b(v)?,
            "throttle_soft_percent" => {
                let p = n(v)?;
                if !(1..=50).contains(&p) {
                    return Err("throttle_soft_percent must be 1 to 50".into());
                }
                self.throttle.soft_percent =
                    u8::try_from(p).map_err(|_| "too large".to_string())?;
            }
            "throttle_soft_min_bytes" => {
                let m = n(v)?;
                if m < 64 << 20 {
                    return Err("throttle_soft_min_bytes must be at least 67108864".into());
                }
                self.throttle.soft_min = m;
            }
            "throttle_soft_max_bytes" => self.throttle.soft_max = n(v)?,
            "throttle_min_rate" => {
                let r = n(v)?;
                if r < 65_536 {
                    return Err("throttle_min_rate must be at least 65536".into());
                }
                self.throttle.min_rate = r;
            }
            "throttle_max_delay_ms" => {
                let t = n(v)?;
                if t < 10 {
                    return Err("throttle_max_delay_ms must be at least 10".into());
                }
                self.throttle.max_delay = Duration::from_millis(t);
            }
            "gc_pressure_rate_factor" => {
                let f = n(v)?;
                if !(1..=64).contains(&f) {
                    return Err("gc_pressure_rate_factor must be 1 to 64".into());
                }
                self.gc_pressure_rate_factor = f;
            }
            other => return Err(format!("unknown key {other:?}")),
        }
        Ok(())
    }

    /// Reads `path`; a missing file at the default path gives the defaults.
    ///
    /// # Errors
    /// An unreadable or invalid file.
    pub fn load(path: &Path, explicit: bool) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(t) => Self::parse(&t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(Self::default()),
            Err(e) => Err(ConfigError {
                line: 0,
                why: format!("{}: {e}", path.display()),
            }),
        }
    }

    /// The audit log, one JSON object per mutating call.
    pub fn audit_log(&self) -> PathBuf {
        self.state_dir.join("audit.log")
    }

    /// The registry.
    pub fn registry(&self) -> PathBuf {
        self.state_dir.join("volumes.json")
    }
}

fn abs(s: &str, key: &str) -> Result<PathBuf, String> {
    if s.starts_with('/') && !s.contains('\0') {
        Ok(PathBuf::from(s))
    } else {
        Err(format!("{key} must be an absolute path"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_resident_max_is_validated() {
        assert_eq!(DaemonConfig::default().journal_resident_max, None);
        assert!(DaemonConfig::parse("journal_resident_max = 2097152").is_ok());
        assert!(DaemonConfig::parse("journal_resident_max = 2097151").is_err());
        assert!(DaemonConfig::parse("journal_resident_max = 0").is_err());
        assert!(DaemonConfig::parse("journal_resident_max = \"big\"").is_err());
    }

    #[test]
    fn cut_target_ms_is_validated() {
        assert_eq!(DaemonConfig::default().cut_target, Duration::from_secs(1));
        let c = DaemonConfig::parse("cut_target_ms = 250").unwrap();
        assert_eq!(c.cut_target, Duration::from_millis(250));
        assert!(DaemonConfig::parse("cut_target_ms = 10").is_ok());
        assert!(DaemonConfig::parse("cut_target_ms = 60000").is_ok());
        assert!(DaemonConfig::parse("cut_target_ms = 9").is_err());
        assert!(DaemonConfig::parse("cut_target_ms = 60001").is_err());
        assert!(DaemonConfig::parse("cut_target_ms = 0").is_err());
        assert!(DaemonConfig::parse("cut_target_ms = \"1s\"").is_err());
    }

    #[test]
    fn a_full_file_parses() {
        let c = DaemonConfig::parse(
            r#"
# comment
state_dir = "/tmp/st"   # trailing
socket = "/tmp/st/ctl.sock"
socket_group = 0
log_level = "debug"
device = "none"
on_journal_damage = "refuse"
harden = false
queues = 4
call_timeout_ms = 1_000
journal_max = 268435456
journal_resident_max = 4194304
cut_target_ms = 500
volume_roots = "/a, /b"
"#,
        )
        .unwrap();
        assert_eq!(c.state_dir, PathBuf::from("/tmp/st"));
        assert_eq!(c.socket_group, Some(0));
        assert_eq!(c.log_level, tracing::Level::DEBUG);
        assert_eq!(c.device, DeviceKind::None);
        assert_eq!(c.on_journal_damage, OnJournalDamage::Refuse);
        assert!(!c.harden);
        assert_eq!(c.queues, Some(4));
        assert_eq!(c.call_timeout, Duration::from_secs(1));
        assert_eq!(c.journal_max, Some(256 << 20));
        assert_eq!(c.journal_resident_max, Some(4 << 20));
        assert_eq!(c.cut_target, Duration::from_millis(500));
        assert_eq!(
            c.volume_roots,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn unknown_keys_tables_and_bad_values_are_refused() {
        for bad in [
            "nope = 1",
            "[section]",
            "state_dir = relative",
            "state_dir = \"relative\"",
            "queues = 17",
            "harden = 1",
            "log_level = \"loud\"",
            "socket = \"/a",
            "just text",
        ] {
            assert!(DaemonConfig::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_throttle_keys_parse_and_are_validated() {
        let d = DaemonConfig::default();
        assert!(d.throttle.enabled);
        assert_eq!(d.throttle.soft_percent, 15);
        assert_eq!(d.throttle.soft_min, 1 << 30);
        assert_eq!(d.throttle.soft_max, 64 << 30);
        assert_eq!(d.throttle.min_rate, 4 << 20);
        assert_eq!(d.throttle.max_delay, Duration::from_secs(10));
        assert_eq!(d.gc_pressure_rate_factor, 4);
        let c = DaemonConfig::parse(
            "throttle = false\nthrottle_soft_percent = 30\nthrottle_soft_min_bytes = 134217728\n\
             throttle_soft_max_bytes = 1073741824\nthrottle_min_rate = 1_000_000\n\
             throttle_max_delay_ms = 500\ngc_pressure_rate_factor = 8\n",
        )
        .unwrap();
        assert!(!c.throttle.enabled);
        assert_eq!(c.throttle.soft_percent, 30);
        assert_eq!(c.throttle.soft_min, 128 << 20);
        assert_eq!(c.throttle.soft_max, 1 << 30);
        assert_eq!(c.throttle.min_rate, 1_000_000);
        assert_eq!(c.throttle.max_delay, Duration::from_millis(500));
        assert_eq!(c.gc_pressure_rate_factor, 8);
        for bad in [
            "throttle_soft_percent = 0",
            "throttle_soft_percent = 51",
            "throttle_soft_min_bytes = 1000",
            "throttle_soft_max_bytes = 1000\n",
            "throttle_min_rate = 65535",
            "throttle_max_delay_ms = 9",
            "throttle_max_delay_ms = 61000",
            "throttle_max_delay_ms = 6000\nfull_timeout_ms = 5000",
            "gc_pressure_rate_factor = 0",
            "gc_pressure_rate_factor = 65",
            "throttle = 1",
        ] {
            assert!(DaemonConfig::parse(bad).is_err(), "{bad}");
        }
        // The default delay cap yields to a shorter full timeout.
        assert!(DaemonConfig::parse("full_timeout_ms = 5000").is_ok());
        // The delay cap follows a longer full timeout; order does not matter.
        assert!(
            DaemonConfig::parse("throttle_max_delay_ms = 61000\nfull_timeout_ms = 120000").is_ok()
        );
    }

    #[test]
    fn a_missing_default_file_gives_defaults_but_an_explicit_one_fails() {
        let p = Path::new("/nonexistent/lunixd.toml");
        assert!(DaemonConfig::load(p, false).is_ok());
        assert!(DaemonConfig::load(p, true).is_err());
    }
}
