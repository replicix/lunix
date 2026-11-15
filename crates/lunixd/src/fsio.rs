//! Durable small-file writes of the daemon's own files: the registry, the
//! keyring and the device symlink's directory (DESIGN §4: temporary file,
//! `fdatasync`, rename, directory `fsync`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rustix::fs::{renameat_with, RenameFlags, CWD};

fn tmp_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// `fsync` of a directory.
///
/// # Errors
/// The open or the sync.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

fn write_tmp(path: &Path, bytes: &[u8], mode: u32) -> io::Result<PathBuf> {
    let tmp = tmp_of(path);
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_data()?;
    Ok(tmp)
}

/// Replaces `path` atomically and durably with `bytes`.
///
/// # Errors
/// Any step's I/O error; the old file is then intact.
pub fn replace_durable(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let tmp = write_tmp(path, bytes, mode)?;
    fs::rename(&tmp, path)?;
    sync_dir(parent_of(path))
}

/// Creates `path` durably with `bytes`; fails with `AlreadyExists` if it
/// exists (the rename does not replace).
///
/// # Errors
/// `AlreadyExists`, or any step's I/O error.
pub fn create_durable(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let tmp = write_tmp(path, bytes, mode)?;
    if let Err(e) = renameat_with(CWD, &tmp, CWD, path, RenameFlags::NOREPLACE) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    sync_dir(parent_of(path))
}

/// Reads a file of at most `cap` bytes; `Ok(None)` when it does not exist.
///
/// # Errors
/// `InvalidData` for a larger file, or the read's error.
pub fn read_capped(path: &Path, cap: u64) -> io::Result<Option<Vec<u8>>> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut buf = Vec::new();
    f.take(cap.saturating_add(1)).read_to_end(&mut buf)?;
    if buf.len() as u64 > cap {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
    }
    Ok(Some(buf))
}

/// Creates every missing directory of `dir` and makes the new entries durable.
///
/// # Errors
/// The first failing step.
pub fn create_dir_durable(dir: &Path) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.as_os_str().is_empty() || d.exists() {
            break;
        }
        missing.push(d.to_path_buf());
        cur = d.parent();
    }
    fs::create_dir_all(dir)?;
    for d in missing.iter().rev() {
        sync_dir(parent_of(d))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_refuses_an_existing_file_and_replace_does_not() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a");
        create_durable(&p, b"one", 0o600).unwrap();
        let e = create_durable(&p, b"two", 0o600).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&p).unwrap(), b"one");
        replace_durable(&p, b"three", 0o600).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"three");
        assert!(!tmp_of(&p).exists());
    }

    #[test]
    fn read_capped_refuses_large_files_and_reports_missing() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("big");
        fs::write(&p, [0u8; 10]).unwrap();
        assert!(read_capped(&p, 9).is_err());
        assert_eq!(read_capped(&p, 10).unwrap().unwrap().len(), 10);
        assert!(read_capped(&t.path().join("none"), 10).unwrap().is_none());
    }
}
