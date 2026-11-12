//! The few host file operations whose order matters, behind a trait so a test
//! can record them, plus small helpers shared by every store.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use lunix_iface::Error;
use rustix::fs::{renameat_with, RenameFlags, CWD};

use crate::volume::Inner;

/// The durability-relevant calls. `RealFs` is the only production
/// implementation; the fsync accounting test wraps it to record the order.
///
/// Data goes to files through ordinary `write`; only the steps that make it
/// durable or visible pass through here.
pub trait FsOps: Send + Sync + fmt::Debug {
    /// `fdatasync(2)` on an open file.
    ///
    /// # Errors
    /// The host error.
    fn fdatasync(&self, file: &File) -> io::Result<()>;

    /// Renames `from` to `to`. With `noreplace` an existing `to` is an
    /// `AlreadyExists` error and nothing changes.
    ///
    /// # Errors
    /// The host error.
    fn rename(&self, from: &Path, to: &Path, noreplace: bool) -> io::Result<()>;

    /// `fsync(2)` on a directory, making created, renamed and removed entries
    /// durable.
    ///
    /// # Errors
    /// The host error.
    fn fsync_dir(&self, dir: &Path) -> io::Result<()>;

    /// Removes a file. A missing file is `NotFound`.
    ///
    /// # Errors
    /// The host error.
    fn unlink(&self, path: &Path) -> io::Result<()>;
}

/// The host file system.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealFs;

impl FsOps for RealFs {
    fn fdatasync(&self, file: &File) -> io::Result<()> {
        file.sync_data()
    }

    fn rename(&self, from: &Path, to: &Path, noreplace: bool) -> io::Result<()> {
        if !noreplace {
            return std::fs::rename(from, to);
        }
        match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
            Ok(()) => Ok(()),
            // A file system without RENAME_NOREPLACE: link then unlink is the
            // same create-only guarantee (link fails with EEXIST).
            Err(
                rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS | rustix::io::Errno::OPNOTSUPP,
            ) => {
                std::fs::hard_link(from, to)?;
                std::fs::remove_file(from)
            }
            Err(e) => Err(io::Error::from(e)),
        }
    }

    fn fsync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(dir)?.sync_all()
    }

    fn unlink(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }
}

/// Maps a host error: a full disk or quota is `NoSpace`, anything else `Io`.
pub(crate) fn io_err(e: io::Error) -> Error {
    match e.raw_os_error() {
        Some(n)
            if n == rustix::io::Errno::NOSPC.raw_os_error()
                || n == rustix::io::Errno::DQUOT.raw_os_error() =>
        {
            Error::NoSpace
        }
        _ => Error::Io(e),
    }
}

/// Maps `NotFound` to the typed error for `what`, everything else as `io_err`.
pub(crate) fn io_or_missing(e: io::Error, what: &'static str) -> Error {
    if e.kind() == io::ErrorKind::NotFound {
        Error::not_found(what)
    } else {
        io_err(e)
    }
}

/// The four durable steps of "tmp, fdatasync, rename, directory fsync", each a
/// named crash point.
#[derive(Clone, Copy)]
pub(crate) struct Points {
    pub before_fdatasync: fn(),
    pub after_fdatasync: fn(),
    pub after_rename: fn(),
    pub after_dirsync: fn(),
}

/// Declares a [`Points`] constant from four crash-point names.
macro_rules! crash_points {
    ($name:ident = $b:literal, $a:literal, $r:literal, $d:literal) => {
        pub(crate) const $name: $crate::fsops::Points = {
            fn before_fdatasync() {
                lunix_iface::crashpoint!($b);
            }
            fn after_fdatasync() {
                lunix_iface::crashpoint!($a);
            }
            fn after_rename() {
                lunix_iface::crashpoint!($r);
            }
            fn after_dirsync() {
                lunix_iface::crashpoint!($d);
            }
            $crate::fsops::Points {
                before_fdatasync,
                after_fdatasync,
                after_rename,
                after_dirsync,
            }
        };
    };
}

crash_points!(
    SEGMENT_POINTS = "store.segment.before_fdatasync",
    "store.segment.after_fdatasync",
    "store.segment.after_rename",
    "store.segment.after_dirsync"
);
crash_points!(
    PACK_POINTS = "store.pack.before_fdatasync",
    "store.pack.after_fdatasync",
    "store.pack.after_rename",
    "store.pack.after_dirsync"
);
crash_points!(
    COMMIT_POINTS = "store.commit.before_fdatasync",
    "store.commit.after_fdatasync",
    "store.commit.after_rename",
    "store.commit.after_dirsync"
);
crash_points!(
    EVIDENCE_POINTS = "store.evidence.before_fdatasync",
    "store.evidence.after_fdatasync",
    "store.evidence.after_rename",
    "store.evidence.after_dirsync"
);
crash_points!(
    STATE_POINTS = "store.state.before_fdatasync",
    "store.state.after_fdatasync",
    "store.state.after_rename",
    "store.state.after_dirsync"
);

/// How [`publish`] treats an existing destination.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Publish {
    /// Refuse (`AlreadyExists`); the destination is never replaced.
    CreateOnly(&'static str),
    /// Replace atomically.
    Replace,
}

/// Opens `path` for a fresh write (truncating a stale temp file of a crashed
/// run).
pub(crate) fn create_tmp(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(path)
}

/// Writes `bytes` as `dir/name` durably and atomically: temp file, `fdatasync`,
/// rename, directory `fsync`. A crash leaves the old state or the new one.
pub(crate) fn publish(
    inner: &Inner,
    dir: &Path,
    name: &str,
    bytes: &[u8],
    how: Publish,
    pts: &Points,
) -> Result<(), Error> {
    let tmp = dir.join(format!("{name}.tmp"));
    let dest = dir.join(name);
    let result = publish_inner(inner, dir, &tmp, &dest, bytes, how, pts);
    if result.is_err() {
        // Best effort: a leftover temp is deleted by the open-time sweep anyway.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn publish_inner(
    inner: &Inner,
    dir: &Path,
    tmp: &Path,
    dest: &Path,
    bytes: &[u8],
    how: Publish,
    pts: &Points,
) -> Result<(), Error> {
    let mut f = create_tmp(tmp).map_err(io_err)?;
    f.write_all(bytes).map_err(io_err)?;
    (pts.before_fdatasync)();
    inner.fs.fdatasync(&f).map_err(io_err)?;
    (pts.after_fdatasync)();
    drop(f);
    let noreplace = matches!(how, Publish::CreateOnly(_));
    match inner.fs.rename(tmp, dest, noreplace) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if let Publish::CreateOnly(what) = how {
                return Err(Error::AlreadyExists { what });
            }
            return Err(io_err(e));
        }
        Err(e) => return Err(io_err(e)),
    }
    (pts.after_rename)();
    inner.fs.fsync_dir(dir).map_err(io_err)?;
    (pts.after_dirsync)();
    Ok(())
}

/// Removes `path` durably: unlink, then directory fsync. Returns whether a
/// file was removed. Crash points `store.delete.*`.
pub(crate) fn unlink_durable(inner: &Inner, path: &Path) -> Result<bool, Error> {
    lunix_iface::crashpoint!("store.delete.before_unlink");
    let removed = match inner.fs.unlink(path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(io_err(e)),
    };
    lunix_iface::crashpoint!("store.delete.after_unlink");
    // Also when nothing was there: an earlier delete may have died between
    // the unlink and the directory fsync.
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    inner.fs.fsync_dir(parent).map_err(io_err)?;
    lunix_iface::crashpoint!("store.delete.after_dirsync");
    Ok(removed)
}

/// Reads exactly `buf.len()` bytes at `offset` from an open fd.
pub(crate) fn pread_exact(fd: &OwnedFd, buf: &mut [u8], offset: u64) -> io::Result<()> {
    let mut done = 0usize;
    while done < buf.len() {
        let at = offset
            .checked_add(done as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        match rustix::io::pread(fd, &mut buf[done..], at) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => done += n,
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(io::Error::from(e)),
        }
    }
    Ok(())
}

/// Entries of `dir` as `(file name, size)`. A missing directory is empty.
pub(crate) fn read_dir_names(dir: &Path) -> io::Result<Vec<(String, u64)>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // A file removed between listing and stat is simply gone.
        match entry.metadata() {
            Ok(m) => out.push((name, m.len())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    out.sort();
    Ok(out)
}

/// `dir/<16 lowercase hex><suffix>` entries as `(id value, size)`, ascending.
pub(crate) fn list_hex_ids(dir: &Path, suffix: &str) -> io::Result<Vec<(u64, u64)>> {
    let mut out = Vec::new();
    for (name, size) in read_dir_names(dir)? {
        let Some(stem) = name.strip_suffix(suffix) else {
            continue;
        };
        if let Some(v) = parse_hex16(stem) {
            out.push((v, size));
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// 16 lowercase hex digits as a `u64`.
pub(crate) fn parse_hex16(s: &str) -> Option<u64> {
    (s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| u64::from_str_radix(s, 16).ok())
    .flatten()
}
