//! Bucket and local paths (freeze item 12, on-disk-formats.md section 22).
//!
//! One function per path. The functions in the top level build
//! **bucket-relative names** (the names inside every metadata AAD, D9);
//! [`local`] builds paths relative to the volume directory; [`local_to_bucket`]
//! maps a local path back to its bucket-relative name.

use crate::error::{FormatError, Result};
use crate::ids::{hex_lower, CommitSeq, FamilyId, ObjId, VolumeUuid};

const WHAT: &str = "path";

/// Longest backend id.
pub const MAX_BACKEND_ID: usize = 64;
/// Longest snapshot name.
pub const MAX_SNAPSHOT_NAME: usize = 128;
/// Longest reader host.
pub const MAX_HOST: usize = 64;

/// Bucket prefix of a volume: `<prefix>/lunix/v1/<volume-uuid>/`.
pub fn bucket_volume_prefix(prefix: &str, uuid: &VolumeUuid) -> String {
    let p = prefix.trim_matches('/');
    if p.is_empty() {
        format!("lunix/v1/{uuid}/")
    } else {
        format!("{p}/lunix/v1/{uuid}/")
    }
}

/// Bucket key of a family keyring: `<prefix>/lunix/v1/families/<id>/keyring`.
pub fn bucket_keyring_key(prefix: &str, family: &FamilyId) -> String {
    let p = prefix.trim_matches('/');
    if p.is_empty() {
        format!("lunix/v1/{}", keyring(family))
    } else {
        format!("{p}/lunix/v1/{}", keyring(family))
    }
}

/// Local directory of a volume: `<dir>/<volume-uuid>`.
pub fn local_volume_dir(dir: &str, uuid: &VolumeUuid) -> String {
    format!("{}/{uuid}", dir.trim_end_matches('/'))
}

/// Local keyring path: `<dir>/families/<family-id>/keyring`.
pub fn local_keyring_path(dir: &str, family: &FamilyId) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), keyring(family))
}

fn x16(v: u64) -> String {
    format!("{v:016x}")
}

fn charset_ok(s: &str, max: usize, allow: impl Fn(char) -> bool) -> bool {
    !s.is_empty() && s.len() <= max && s.chars().all(allow)
}

/// A backend id: 1 ..= 64 of `[a-z0-9._-]`.
pub fn valid_backend_id(s: &str) -> bool {
    charset_ok(s, MAX_BACKEND_ID, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
    })
}

/// A snapshot name: 1 ..= 128 of `[A-Za-z0-9._-]`, not starting with `.`.
pub fn valid_snapshot_name(s: &str) -> bool {
    charset_ok(s, MAX_SNAPSHOT_NAME, |c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
    }) && !s.starts_with('.')
}

/// A reader host: 1 ..= 64 of `[a-z0-9.-]`.
pub fn valid_host(s: &str) -> bool {
    charset_ok(s, MAX_HOST, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-')
    })
}

fn checked(ok: bool, what_field: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(FormatError::BadString {
            what: WHAT,
            field: what_field,
        })
    }
}

/// `volume`.
pub fn volume() -> String {
    "volume".into()
}
/// `lease`.
pub fn lease() -> String {
    "lease".into()
}
/// `head` (advisory, never trusted).
pub fn head() -> String {
    "head".into()
}
/// `commits/<commit_seq:016x>`.
pub fn commit(seq: CommitSeq) -> String {
    format!("commits/{}", x16(seq.0))
}
/// `segments/<seg:016x>`.
pub fn segment(id: ObjId) -> String {
    format!("segments/{}", x16(id.raw()))
}
/// `map/<pack:016x>`.
pub fn map_pack(id: ObjId) -> String {
    format!("map/{}", x16(id.raw()))
}
/// `index/<pack:016x>`.
pub fn index_run(id: ObjId) -> String {
    format!("index/{}", x16(id.raw()))
}
/// `heat/<commit_seq:016x>`.
pub fn heat(seq: CommitSeq) -> String {
    format!("heat/{}", x16(seq.0))
}
/// `dicts/<id:08x>`.
pub fn dict(id: u32) -> String {
    format!("dicts/{id:08x}")
}
/// `snapshots/<name>`.
pub fn snapshot(name: &str) -> Result<String> {
    checked(valid_snapshot_name(name), "snapshot name")?;
    Ok(format!("snapshots/{name}"))
}
/// `readers/<host>-<id>`, `id` rendered as 16 hex digits.
pub fn reader(host: &str, id: u64) -> Result<String> {
    checked(valid_host(host), "host")?;
    Ok(format!("readers/{host}-{}", x16(id)))
}
/// `holds/<child-uuid>`.
pub fn hold(child: &VolumeUuid) -> String {
    format!("holds/{child}")
}
/// `gc/<pass:016x>`; `pass` is at least 1.
pub fn gc_pass(pass: u64) -> Result<String> {
    if pass == 0 {
        return Err(FormatError::range(WHAT, "gc pass", 0));
    }
    Ok(format!("gc/{}", x16(pass)))
}
/// `gc/space/<commit_seq:016x>` (reserved).
pub fn gc_space(seq: CommitSeq) -> String {
    format!("gc/space/{}", x16(seq.0))
}
/// `sync/<backend-id>/progress`.
pub fn sync_progress(backend: &str) -> Result<String> {
    checked(valid_backend_id(backend), "backend id")?;
    Ok(format!("sync/{backend}/progress"))
}
/// `families/<family-id>/keyring`, relative to `<prefix>/lunix/v1/`.
pub fn keyring(family: &FamilyId) -> String {
    format!("families/{}/keyring", hex_lower(&family.0))
}

/// Local paths, relative to the volume directory `<dir>/<volume-uuid>/`
/// unless stated.
pub mod local {
    use super::{checked, valid_host, valid_snapshot_name, x16};
    use crate::error::{FormatError, Result};
    use crate::ids::{CommitSeq, ObjId, VolumeUuid};

    /// `volume` (the exact descriptor bytes).
    pub fn volume() -> String {
        "volume".into()
    }
    /// `volume.json`.
    pub fn volume_json() -> String {
        "volume.json".into()
    }
    /// `LOCK`.
    pub fn lock() -> String {
        "LOCK".into()
    }
    /// `state/commits/<commit_seq:016x>`.
    pub fn commit(seq: CommitSeq) -> String {
        format!("state/commits/{}", x16(seq.0))
    }
    /// `segments/<seg:016x>.seg` (sealed).
    pub fn segment_sealed(id: ObjId) -> String {
        format!("segments/{}.seg", x16(id.raw()))
    }
    /// `segments/<seg:016x>.open` (being written).
    pub fn segment_open(id: ObjId) -> String {
        format!("segments/{}.open", x16(id.raw()))
    }
    /// `map/<pack:016x>` (sealed).
    pub fn map_pack_sealed(id: ObjId) -> String {
        format!("map/{}", x16(id.raw()))
    }
    /// `map/<pack:016x>.open`.
    pub fn map_pack_open(id: ObjId) -> String {
        format!("map/{}.open", x16(id.raw()))
    }
    /// `index/<pack:016x>`.
    pub fn index_run(id: ObjId) -> String {
        format!("index/{}", x16(id.raw()))
    }
    /// `index/manifest` (local only; not frozen, M6).
    pub fn index_manifest() -> String {
        "index/manifest".into()
    }
    /// `heat/<commit_seq:016x>`.
    pub fn heat(seq: CommitSeq) -> String {
        format!("heat/{}", x16(seq.0))
    }
    /// `snapshots/<name>`.
    pub fn snapshot(name: &str) -> Result<String> {
        checked(valid_snapshot_name(name), "snapshot name")?;
        Ok(format!("snapshots/{name}"))
    }
    /// `readers/<host>-<id>`.
    pub fn reader(host: &str, id: u64) -> Result<String> {
        checked(valid_host(host), "host")?;
        Ok(format!("readers/{host}-{}", x16(id)))
    }
    /// `holds/<child-uuid>`.
    pub fn hold(child: &VolumeUuid) -> String {
        format!("holds/{child}")
    }
    /// `gc/<pass:016x>`.
    pub fn gc_pass(pass: u64) -> Result<String> {
        if pass == 0 {
            return Err(FormatError::range("path", "gc pass", 0));
        }
        Ok(format!("gc/{}", x16(pass)))
    }
    /// `gc/space/<commit_seq:016x>`.
    pub fn gc_space(seq: CommitSeq) -> String {
        format!("gc/space/{}", x16(seq.0))
    }
    /// `dicts/<id:08x>`.
    pub fn dict(id: u32) -> String {
        format!("dicts/{id:08x}")
    }
    /// `journal/q<lane:02x>-<ordinal:08x>.j`.
    pub fn journal_file(lane: u8, ordinal: u32) -> String {
        format!("journal/q{lane:02x}-{ordinal:08x}.j")
    }
    /// `cache/` (local only).
    pub fn cache_dir() -> String {
        "cache/".into()
    }
    /// `state/checkpoint`.
    pub fn state_checkpoint() -> String {
        "state/checkpoint".into()
    }
    /// `state/device`.
    pub fn state_device() -> String {
        "state/device".into()
    }
    /// `state/pins`.
    pub fn state_pins() -> String {
        "state/pins".into()
    }
    /// `state/gc/condemned`.
    pub fn state_gc_condemned() -> String {
        "state/gc/condemned".into()
    }
    /// `integrity.key`, the auto-created key file (0600, never uploaded).
    pub fn integrity_key() -> String {
        "integrity.key".into()
    }
}

fn hex16(s: &str) -> Option<u64> {
    if s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        u64::from_str_radix(s, 16).ok()
    } else {
        None
    }
}

fn bad(field: &'static str) -> FormatError {
    FormatError::BadString { what: WHAT, field }
}

/// Maps a local path (relative to the volume directory, or
/// `families/<id>/keyring` relative to `<dir>`) to its bucket-relative name.
///
/// Returns `Ok(None)` for local-only paths (`volume.json`, `LOCK`, journal
/// files, `cache/`, `state/checkpoint`, `state/device`, `state/pins`,
/// `state/gc/condemned`, `integrity.key`, `index/manifest`, `.open` and
/// `.tmp` files) and an error for a path in a mapped directory whose name is
/// malformed.
pub fn local_to_bucket(rel: &str) -> Result<Option<String>> {
    if rel.ends_with(".tmp") {
        return Ok(None);
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let id16 = |s: &str, f: &'static str| hex16(s).ok_or_else(|| bad(f));
    match parts.as_slice() {
        ["volume"] => Ok(Some("volume".into())),
        ["volume.json" | "LOCK" | "integrity.key"] => Ok(None),
        ["state", "commits", n] => Ok(Some(format!("commits/{:016x}", id16(n, "commit name")?))),
        ["state", "checkpoint" | "device" | "pins"] => Ok(None),
        ["state", "gc", "condemned"] => Ok(None),
        ["segments", n] => match n.strip_suffix(".seg") {
            Some(h) => Ok(Some(format!("segments/{:016x}", id16(h, "segment name")?))),
            None => match n.strip_suffix(".open") {
                Some(h) => id16(h, "segment name").map(|_| None),
                None => Err(bad("segment name")),
            },
        },
        ["map", n] => match n.strip_suffix(".open") {
            Some(h) => id16(h, "pack name").map(|_| None),
            None => Ok(Some(format!("map/{:016x}", id16(n, "pack name")?))),
        },
        ["index", "manifest"] => Ok(None),
        ["index", n] => Ok(Some(format!("index/{:016x}", id16(n, "run name")?))),
        ["heat", n] => Ok(Some(format!("heat/{:016x}", id16(n, "heat name")?))),
        ["snapshots", n] => Ok(Some(snapshot(n)?)),
        ["readers", n] => {
            let (host, id) = n.rsplit_once('-').ok_or_else(|| bad("reader name"))?;
            let id = id16(id, "reader id")?;
            Ok(Some(reader(host, id)?))
        }
        ["holds", n] => {
            let u: VolumeUuid = n.parse().map_err(|_| bad("hold name"))?;
            Ok(Some(hold(&u)))
        }
        ["gc", "space", n] => Ok(Some(format!("gc/space/{:016x}", id16(n, "gc space name")?))),
        ["gc", n] => Ok(Some(gc_pass(id16(n, "gc pass name")?)?)),
        ["dicts", n] => {
            if n.len() == 8
                && n.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                Ok(Some(format!("dicts/{n}")))
            } else {
                Err(bad("dict name"))
            }
        }
        ["journal", n] => {
            let ok = n.len() == 3 + 1 + 8 + 2
                && n.starts_with('q')
                && n.ends_with(".j")
                && n.as_bytes()[3] == b'-';
            if ok {
                Ok(None)
            } else {
                Err(bad("journal name"))
            }
        }
        ["cache", ..] => Ok(None),
        ["families", f, "keyring"] => {
            let fam: FamilyId = f.parse().map_err(|_| bad("family id"))?;
            Ok(Some(keyring(&fam)))
        }
        _ => Err(bad("local path")),
    }
}
