//! The ring files of every lane: naming, creation, per-file bookkeeping and
//! recycling. All blocking.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lunix_format::journal::{FileHeader, BLOCK};
use lunix_iface::{Error, Result};

use crate::aligned::AlignedBuf;
use crate::config::JournalConfig;
use crate::sys::{self, B};

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn file_name(lane: u16, ordinal: u32) -> String {
    format!("q{lane:02x}-{ordinal:08x}.j")
}

/// `q<lane:02x>-<ordinal:08x>.j` to `(lane, ordinal)`.
pub(crate) fn parse_name(name: &str) -> Option<(u16, u32)> {
    let rest = name.strip_prefix('q')?.strip_suffix(".j")?;
    let (l, o) = rest.split_once('-')?;
    if l.len() != 2 || o.len() != 8 {
        return None;
    }
    let lane = u16::from_str_radix(l, 16).ok()?;
    let ordinal = u32::from_str_radix(o, 16).ok()?;
    (ordinal >= 1).then_some((lane, ordinal))
}

pub(crate) fn io_err(e: io::Error) -> Error {
    if e.raw_os_error() == Some(libc::ENOSPC) {
        Error::NoSpace
    } else {
        Error::Io(e)
    }
}

/// What a ring file is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Role {
    /// Initialised and empty; the lane takes these in ordinal order.
    Free,
    /// The lane's current file.
    Active,
    /// Rolled away from; may hold records not yet reclaimed.
    Used,
    /// Being re-initialised by reclaim.
    Recycling,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SlotInfo {
    pub ordinal: u32,
    pub gen: u64,
    pub role: Role,
    /// The header failed its checks (or does not match the name): the file
    /// holds no records and is re-initialised before use.
    pub bad: bool,
    /// `bad` because the header is a valid header of this lane and volume
    /// with a higher ordinal than the name: a re-initialisation
    /// ([`Rings::reinit_slot`]) interrupted between its header write and its
    /// rename. The scan does not report such a file as damage when its body
    /// is zero (on-disk-formats §5.1).
    pub reinit: bool,
}

/// One physical file. The descriptor lives as long as the journal; a recycle
/// rewrites the same inode and renames it.
pub(crate) struct RingFile {
    pub file: File,
    /// Highest `write_seq` placed in the file (0: none, or all at or below
    /// the recovery floor of a skipped file).
    pub max_seq: AtomicU64,
    /// Sum of the encoded lengths of the records charged to the file.
    pub rec_bytes: AtomicU64,
    /// End of the highest byte that may be non-zero (a block multiple).
    pub hi_off: AtomicU64,
    /// Batch writes queued and not yet reaped.
    pub inflight: AtomicU32,
}

impl RingFile {
    fn new(file: File) -> Self {
        Self {
            file,
            max_seq: AtomicU64::new(0),
            rec_bytes: AtomicU64::new(0),
            hi_off: AtomicU64::new(B),
            inflight: AtomicU32::new(0),
        }
    }

    pub(crate) fn reset_counters(&self) {
        self.max_seq.store(0, Ordering::Relaxed);
        self.rec_bytes.store(0, Ordering::Relaxed);
        self.hi_off.store(B, Ordering::Relaxed);
    }
}

pub(crate) struct RingInner {
    pub slots: Vec<SlotInfo>,
    pub free: VecDeque<usize>,
    pub active: Option<usize>,
    /// Where the next batch of the active file starts (set by recovery and
    /// when a lane is dropped; a lane that opens takes it).
    pub resume_off: u64,
    /// The next ordinal to hand to a re-initialised file.
    pub next_ordinal: u32,
}

impl RingInner {
    pub(crate) fn slot_by_ordinal(&self, ordinal: u32) -> Option<usize> {
        self.slots.iter().position(|s| s.ordinal == ordinal)
    }

    pub(crate) fn take_ordinal(&mut self) -> u32 {
        let o = self.next_ordinal;
        self.next_ordinal += 1;
        o
    }
}

/// All files of one lane.
pub(crate) struct LaneRing {
    pub lane: u16,
    pub files: Vec<Arc<RingFile>>,
    pub inner: Mutex<RingInner>,
}

pub(crate) struct Rings {
    pub dir: PathBuf,
    pub lanes: Vec<LaneRing>,
    pub file_len: u64,
}

impl Rings {
    pub(crate) fn path(&self, lane: u16, ordinal: u32) -> PathBuf {
        self.dir.join(file_name(lane, ordinal))
    }

    /// The file holding `(lane, ordinal)` if it is in use (not free, not being
    /// recycled).
    pub(crate) fn find_in_use(&self, lane: u16, ordinal: u32) -> Option<Arc<RingFile>> {
        let ring = self.lanes.get(usize::from(lane))?;
        let inner = lock(&ring.inner);
        let slot = inner.slot_by_ordinal(ordinal)?;
        match inner.slots[slot].role {
            Role::Active | Role::Used => Some(Arc::clone(&ring.files[slot])),
            Role::Free | Role::Recycling => None,
        }
    }
}

/// Creates a new, physically zero-filled ring file: temporary name, zero fill,
/// `fdatasync`, header, `fdatasync`, rename. The directory is synced by the
/// caller once for all files it created.
fn create_file(
    cfg: &JournalConfig,
    rings_dir: &Path,
    lane: u16,
    ordinal: u32,
    zero: &AlignedBuf,
) -> io::Result<(File, u64)> {
    let final_path = rings_dir.join(file_name(lane, ordinal));
    let tmp = rings_dir.join(format!("{}.tmp", file_name(lane, ordinal)));
    let _ = std::fs::remove_file(&tmp);
    let file = sys::open_file(&tmp, cfg.direct, true)?;
    sys::set_nocow(&file);
    match rustix::fs::fallocate(&file, rustix::fs::FallocateFlags::empty(), 0, cfg.file_len) {
        Ok(()) => {}
        Err(e) if e == rustix::io::Errno::OPNOTSUPP => {}
        Err(e) => return Err(e.into()),
    }
    sys::zero_range(&file, B, cfg.file_len, zero)?;
    file.sync_data()?;
    let gen = sys::random_nonzero()?;
    let hdr = FileHeader {
        lane,
        ordinal,
        volume_uuid: cfg.volume_uuid,
        generation: gen,
    };
    file.write_all_at(sys::header_block(&hdr)?.as_slice(), 0)?;
    file.sync_data()?;
    std::fs::rename(&tmp, &final_path)?;
    Ok((file, gen))
}

/// Opens the journal directory: validates existing files, creates the missing
/// ones. Existing files are never modified.
pub(crate) fn open_rings(cfg: &JournalConfig, zero: &AlignedBuf) -> Result<Rings> {
    let existed = cfg.dir.is_dir();
    std::fs::create_dir_all(&cfg.dir).map_err(io_err)?;
    if !existed {
        if let Some(parent) = cfg.dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            sys::fsync_dir(parent).map_err(io_err)?;
        }
    }
    let want = cfg.files_per_lane();
    let mut found: Vec<Vec<u32>> = vec![Vec::new(); usize::from(cfg.lanes)];
    for entry in std::fs::read_dir(&cfg.dir).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('q') && name.ends_with(".j.tmp") {
            // An interrupted creation or re-initialisation: nothing in it is
            // referenced by anything.
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        let Some((lane, ordinal)) = parse_name(name) else {
            continue;
        };
        match found.get_mut(usize::from(lane)) {
            Some(v) => v.push(ordinal),
            None => {
                return Err(Error::Unsupported {
                    what: "journal files of a lane beyond the configured lane count",
                })
            }
        }
    }
    let mut created_any = false;
    let mut lanes = Vec::with_capacity(found.len());
    for (lane_idx, mut ordinals) in found.into_iter().enumerate() {
        let lane = u16::try_from(lane_idx).map_err(|_| Error::invalid("lane overflow"))?;
        ordinals.sort_unstable();
        let mut files = Vec::new();
        let mut slots = Vec::new();
        for &ordinal in &ordinals {
            let path = cfg.dir.join(file_name(lane, ordinal));
            let file = sys::open_file(&path, cfg.direct, false).map_err(io_err)?;
            let head = sys::read_aligned(&file, 0, BLOCK)
                .ok()
                .and_then(|b| FileHeader::decode(b.as_slice()).ok());
            let reinit = head.as_ref().is_some_and(|h| {
                h.lane == lane && h.ordinal > ordinal && h.volume_uuid == cfg.volume_uuid
            });
            let (gen, bad) = match head {
                Some(h) if h.lane == lane && h.ordinal == ordinal => {
                    if h.volume_uuid != cfg.volume_uuid {
                        // Re-initialising it would destroy another volume's
                        // records: refuse instead.
                        return Err(Error::Corrupt {
                            what: "journal file",
                            detail: "belongs to another volume",
                        });
                    }
                    (h.generation, false)
                }
                _ => (0, true),
            };
            files.push(Arc::new(RingFile::new(file)));
            slots.push(SlotInfo {
                ordinal,
                gen,
                role: Role::Used,
                bad,
                reinit,
            });
        }
        let mut next_ordinal = ordinals.last().map_or(1, |o| o + 1);
        while files.len() < want {
            let (file, gen) =
                create_file(cfg, &cfg.dir, lane, next_ordinal, zero).map_err(io_err)?;
            created_any = true;
            files.push(Arc::new(RingFile::new(file)));
            slots.push(SlotInfo {
                ordinal: next_ordinal,
                gen,
                role: Role::Free,
                bad: false,
                reinit: false,
            });
            next_ordinal += 1;
        }
        lanes.push(LaneRing {
            lane,
            files,
            inner: Mutex::new(RingInner {
                slots,
                free: VecDeque::new(),
                active: None,
                resume_off: B,
                next_ordinal,
            }),
        });
    }
    if created_any {
        sys::fsync_dir(&cfg.dir).map_err(io_err)?;
    }
    Ok(Rings {
        dir: cfg.dir.clone(),
        lanes,
        file_len: cfg.file_len,
    })
}

/// What [`Rings::reinit_slot`] is asked to do.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reinit {
    pub lane: u16,
    pub old_ordinal: u32,
    pub new_ordinal: u32,
    /// End of the region that may hold non-zero bytes.
    pub zero_to: u64,
}

impl Rings {
    /// Re-initialises one slot as a free file with a new ordinal and
    /// generation: zero `[4096, hi)`, `fdatasync`, new header, `fdatasync`,
    /// rename. The caller syncs the directory. Returns the new generation.
    pub(crate) fn reinit_slot(
        &self,
        cfg: &JournalConfig,
        file: &RingFile,
        r: Reinit,
        zero: &AlignedBuf,
    ) -> io::Result<u64> {
        let Reinit {
            lane,
            old_ordinal,
            new_ordinal,
            zero_to,
        } = r;
        let len = file.file.metadata()?.len();
        if len != self.file_len {
            file.file.set_len(self.file_len)?;
        }
        sys::zero_range(&file.file, B, zero_to.min(self.file_len), zero)?;
        file.file.sync_data()?;
        let gen = sys::random_nonzero()?;
        let hdr = FileHeader {
            lane,
            ordinal: new_ordinal,
            volume_uuid: cfg.volume_uuid,
            generation: gen,
        };
        file.file
            .write_all_at(sys::header_block(&hdr)?.as_slice(), 0)?;
        file.file.sync_data()?;
        // The file now has a valid header that does not match its name; the
        // scan recognises the state (`SlotInfo::reinit`).
        lunix_iface::crashpoint!("journal.reinit.before_rename");
        std::fs::rename(self.path(lane, old_ordinal), self.path(lane, new_ordinal))?;
        Ok(gen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        assert_eq!(file_name(0, 1), "q00-00000001.j");
        assert_eq!(file_name(255, 0xabcd), "qff-0000abcd.j");
        assert_eq!(parse_name("q00-00000001.j"), Some((0, 1)));
        assert_eq!(parse_name("qff-0000abcd.j"), Some((255, 0xabcd)));
        for bad in [
            "q00-00000000.j",
            "q0-00000001.j",
            "q00-1.j",
            "q00-00000001.j.tmp",
            "x00-00000001.j",
            "q00_00000001.j",
            "q00-0000000g.j",
        ] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
    }
}
