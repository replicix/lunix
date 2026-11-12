//! Blocking file helpers shared by creation, recovery and reclaim. Nothing
//! here runs on a lane thread.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

use lunix_format::journal::{FileHeader, BLOCK};

use crate::aligned::AlignedBuf;

pub(crate) const B: u64 = BLOCK as u64;

pub(crate) fn round4k(x: u64) -> u64 {
    x.saturating_add(B - 1) & !(B - 1)
}

pub(crate) fn floor4k(x: u64) -> u64 {
    x & !(B - 1)
}

/// Bytes of the shared zero buffer used for zero fills (1 MiB).
pub(crate) const ZERO_LEN: usize = 1 << 20;

/// Opens (or creates) a ring file for read and write.
pub(crate) fn open_file(path: &Path, direct: bool, create_new: bool) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).mode(0o600);
    if create_new {
        o.create_new(true);
    }
    let mut flags = libc::O_CLOEXEC;
    if direct {
        flags |= libc::O_DIRECT;
    }
    o.custom_flags(flags);
    o.open(path)
}

/// Best effort `chattr +C` on a freshly created, still empty file. Only btrfs
/// honours it; other file systems refuse and the error is ignored.
pub(crate) fn set_nocow(file: &File) {
    use std::os::fd::AsRawFd;
    const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
    const FS_IOC_SETFLAGS: libc::c_ulong = 0x4008_6602;
    const FS_NOCOW_FL: libc::c_int = 0x0080_0000;
    let fd = file.as_raw_fd();
    let mut flags: libc::c_int = 0;
    // SAFETY: FS_IOC_GETFLAGS writes one c_int through the pointer, which
    // points at a live local; the fd is open for the whole call.
    let rc = unsafe { libc::ioctl(fd, FS_IOC_GETFLAGS as _, &mut flags as *mut libc::c_int) };
    if rc != 0 {
        return;
    }
    flags |= FS_NOCOW_FL;
    // SAFETY: FS_IOC_SETFLAGS reads one c_int through the pointer to a live
    // local; the fd is open for the whole call.
    let _ = unsafe { libc::ioctl(fd, FS_IOC_SETFLAGS as _, &flags as *const libc::c_int) };
}

/// A fresh non-zero random 64-bit value (file generations).
pub(crate) fn random_nonzero() -> io::Result<u64> {
    loop {
        let mut v = 0u64;
        // SAFETY: getrandom writes at most 8 bytes into the live local `v`.
        let n = unsafe { libc::getrandom((&mut v as *mut u64).cast(), 8, 0) };
        if n == 8 {
            if v != 0 {
                return Ok(v);
            }
        } else if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}

/// `fsync` of a directory.
pub(crate) fn fsync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Reads `len` bytes at `off` (both block multiples) into a fresh buffer.
pub(crate) fn read_aligned(file: &File, off: u64, len: usize) -> io::Result<AlignedBuf> {
    let mut buf = AlignedBuf::zeroed(len)?;
    file.read_exact_at(buf.as_mut_slice(), off)?;
    Ok(buf)
}

/// Encodes a file header block into an aligned buffer.
pub(crate) fn header_block(h: &FileHeader) -> io::Result<AlignedBuf> {
    let mut v = Vec::with_capacity(BLOCK);
    h.encode(&mut v)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut buf = AlignedBuf::zeroed(BLOCK)?;
    buf.as_mut_slice().copy_from_slice(&v);
    Ok(buf)
}

/// Zeroes bytes `[from, to)` of `file`: an unaligned start is a
/// read-modify-write of its block, the rest are block writes. `to` is rounded
/// up to a block.
pub(crate) fn zero_range(file: &File, from: u64, to: u64, zero: &AlignedBuf) -> io::Result<()> {
    let to = round4k(to);
    if from >= to {
        return Ok(());
    }
    let mut pos = from;
    if !pos.is_multiple_of(B) {
        let base = floor4k(pos);
        let mut blk = read_aligned(file, base, BLOCK)?;
        let keep = (pos - base) as usize;
        // Never rewrite a block whose tail is already zero: it may hold the end
        // of an acknowledged record, and a clean restart must write nothing.
        if !is_zero(&blk.as_slice()[keep..]) {
            blk.as_mut_slice()[keep..].fill(0);
            file.write_all_at(blk.as_slice(), base)?;
        }
        pos = base + B;
    }
    while pos < to {
        let n = ((to - pos) as usize).min(zero.len());
        file.write_all_at(&zero.as_slice()[..n], pos)?;
        pos += n as u64;
    }
    Ok(())
}

/// True when every byte of `block` is zero.
pub(crate) fn is_zero(block: &[u8]) -> bool {
    block.iter().all(|&b| b == 0)
}

/// The end (a block multiple) of the last block of `data` holding a non-zero
/// byte, or `B` (just the header block) when there is none.
pub(crate) fn nonzero_end(data: &[u8]) -> u64 {
    let blocks = data.len() / BLOCK;
    for i in (1..blocks).rev() {
        if !is_zero(&data[i * BLOCK..(i + 1) * BLOCK]) {
            return ((i + 1) * BLOCK) as u64;
        }
    }
    B
}
