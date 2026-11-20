//! The ring files of every lane and of the mark ring: naming, creation,
//! per-file bookkeeping and recycling. All blocking.

use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lunix_format::journal::{FileHeader, BLOCK, MARK_FILE_LEN};
use lunix_iface::{Error, Result};

use crate::aligned::AlignedBuf;
use crate::config::JournalConfig;
use crate::sys::{self, B};

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Mark ring files kept by a journal (on-disk-formats section 5.1).
pub(crate) const MARK_FILES: usize = 4;

/// Which ring a file belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RingKind {
    /// A queue lane's ring: `q<lane:02x>-<ordinal:08x>.j`.
    Queue,
    /// The flush coordinator's mark ring: `m00-<ordinal:08x>.j`, header lane 0.
    Mark,
}

pub(crate) fn file_name(kind: RingKind, lane: u16, ordinal: u32) -> String {
    match kind {
        RingKind::Queue => format!("q{lane:02x}-{ordinal:08x}.j"),
        RingKind::Mark => format!("m00-{ordinal:08x}.j"),
    }
}

/// `q<lane:02x>-<ordinal:08x>.j` to `(Queue, lane, ordinal)` and
/// `m00-<ordinal:08x>.j` to `(Mark, 0, ordinal)`.
pub(crate) fn parse_name(name: &str) -> Option<(RingKind, u16, u32)> {
    let (kind, rest) = match name.strip_prefix('q') {
        Some(r) => (RingKind::Queue, r),
        None => (RingKind::Mark, name.strip_prefix('m')?),
    };
    let rest = rest.strip_suffix(".j")?;
    let (l, o) = rest.split_once('-')?;
    if l.len() != 2 || o.len() != 8 {
        return None;
    }
    let lane = u16::from_str_radix(l, 16).ok()?;
    if kind == RingKind::Mark && lane != 0 {
        return None;
    }
    let ordinal = u32::from_str_radix(o, 16).ok()?;
    (ordinal >= 1).then_some((kind, lane, ordinal))
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

/// All files of one lane, or of the mark ring.
pub(crate) struct LaneRing {
    pub kind: RingKind,
    /// The lane in the file names and headers (0 for the mark ring).
    pub lane: u16,
    /// Length of each of its files.
    pub file_len: u64,
    pub files: Vec<Arc<RingFile>>,
    pub inner: Mutex<RingInner>,
}

pub(crate) struct Rings {
    pub dir: PathBuf,
    /// The queue lanes, indexed by lane.
    pub lanes: Vec<LaneRing>,
    /// The flush coordinator's mark ring.
    pub marks: LaneRing,
    /// Length of a queue lane's file.
    pub file_len: u64,
    /// `FALLOC_FL_ZERO_RANGE` is not used: configured off, or the file system
    /// refused it.
    pub zero_range_off: AtomicBool,
    /// Files re-initialised by `ZERO_RANGE` and by rewriting zeros.
    pub zeroed_by_range: AtomicU64,
    pub zeroed_by_rewrite: AtomicU64,
    /// `ZERO_RANGE` failures in a row that were not "unsupported"; at
    /// [`ZERO_RANGE_MAX_FAILURES`] it is turned off.
    pub zero_failures: AtomicU32,
    /// The errnos of failed `ZERO_RANGE` calls already logged.
    pub zero_logged: Mutex<BTreeSet<i32>>,
    /// `ZERO_RANGE` calls made, and the errno the next ones fail with (0:
    /// none; test hook).
    pub zero_attempts: AtomicU64,
    pub zero_inject_errno: AtomicI32,
}

/// `ZERO_RANGE` failures in a row (other than "unsupported") after which it is
/// not tried again: a file system that answers `ENOSPC` or `EIO` to every
/// call would otherwise cost a failed call and a log line per freed file.
pub(crate) const ZERO_RANGE_MAX_FAILURES: u32 = 3;

impl Rings {
    pub(crate) fn path(&self, ring: &LaneRing, ordinal: u32) -> PathBuf {
        self.dir.join(file_name(ring.kind, ring.lane, ordinal))
    }

    /// The queue lanes, then the mark ring.
    pub(crate) fn all(&self) -> impl Iterator<Item = &LaneRing> {
        self.lanes.iter().chain(std::iter::once(&self.marks))
    }

    /// The file holding `(lane, ordinal)` if it is in use (not free, not being
    /// recycled).
    pub(crate) fn find_in_use(&self, lane: u16, ordinal: u32) -> Option<Arc<RingFile>> {
        self.find_in_use_gen(lane, ordinal).map(|(f, _)| f)
    }

    /// [`Self::find_in_use`] with the file's generation (the `gen` its
    /// records carry).
    pub(crate) fn find_in_use_gen(&self, lane: u16, ordinal: u32) -> Option<(Arc<RingFile>, u64)> {
        let ring = self.lanes.get(usize::from(lane))?;
        let inner = lock(&ring.inner);
        let slot = inner.slot_by_ordinal(ordinal)?;
        match inner.slots[slot].role {
            Role::Active | Role::Used => {
                Some((Arc::clone(&ring.files[slot]), inner.slots[slot].gen))
            }
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
    (kind, lane, ordinal): (RingKind, u16, u32),
    file_len: u64,
    zero: &AlignedBuf,
) -> io::Result<(File, u64)> {
    let final_path = rings_dir.join(file_name(kind, lane, ordinal));
    let tmp = rings_dir.join(format!("{}.tmp", file_name(kind, lane, ordinal)));
    let _ = std::fs::remove_file(&tmp);
    let file = sys::open_file(&tmp, cfg.direct, true)?;
    sys::set_nocow(&file);
    match rustix::fs::fallocate(&file, rustix::fs::FallocateFlags::empty(), 0, file_len) {
        Ok(()) => {}
        Err(e) if e == rustix::io::Errno::OPNOTSUPP => {}
        Err(e) => return Err(e.into()),
    }
    sys::zero_range(&file, B, file_len, zero)?;
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
/// ones (queue lanes and the mark ring). Existing files are never modified.
pub(crate) fn open_rings(cfg: &JournalConfig, zero: &AlignedBuf) -> Result<Rings> {
    let existed = cfg.dir.is_dir();
    std::fs::create_dir_all(&cfg.dir).map_err(io_err)?;
    if !existed {
        if let Some(parent) = cfg.dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            sys::fsync_dir(parent).map_err(io_err)?;
        }
    }
    let mut found: Vec<Vec<u32>> = vec![Vec::new(); usize::from(cfg.lanes)];
    let mut marks_found: Vec<u32> = Vec::new();
    for entry in std::fs::read_dir(&cfg.dir).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if (name.starts_with('q') || name.starts_with('m')) && name.ends_with(".j.tmp") {
            // An interrupted creation or re-initialisation: nothing in it is
            // referenced by anything.
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        match parse_name(name) {
            Some((RingKind::Mark, _, ordinal)) => marks_found.push(ordinal),
            Some((RingKind::Queue, lane, ordinal)) => match found.get_mut(usize::from(lane)) {
                Some(v) => v.push(ordinal),
                None => {
                    return Err(Error::Unsupported {
                        what: "journal files of a lane beyond the configured lane count",
                    })
                }
            },
            None => {}
        }
    }
    let mut created_any = false;
    let mut lanes = Vec::with_capacity(found.len());
    for (lane_idx, ordinals) in found.into_iter().enumerate() {
        let lane = u16::try_from(lane_idx).map_err(|_| Error::invalid("lane overflow"))?;
        lanes.push(load_ring(
            cfg,
            (RingKind::Queue, lane),
            ordinals,
            (cfg.files_per_lane(), cfg.file_len),
            zero,
            &mut created_any,
        )?);
    }
    let marks = load_ring(
        cfg,
        (RingKind::Mark, 0),
        marks_found,
        (MARK_FILES, MARK_FILE_LEN as u64),
        zero,
        &mut created_any,
    )?;
    if created_any {
        sys::fsync_dir(&cfg.dir).map_err(io_err)?;
    }
    Ok(Rings {
        dir: cfg.dir.clone(),
        lanes,
        marks,
        file_len: cfg.file_len,
        zero_range_off: AtomicBool::new(!cfg.zero_range),
        zeroed_by_range: AtomicU64::new(0),
        zeroed_by_rewrite: AtomicU64::new(0),
        zero_failures: AtomicU32::new(0),
        zero_logged: Mutex::new(BTreeSet::new()),
        zero_attempts: AtomicU64::new(0),
        zero_inject_errno: AtomicI32::new(0),
    })
}

/// Opens the existing files of one ring (`ordinals`) and creates the missing
/// ones up to `want` files of `file_len` bytes.
fn load_ring(
    cfg: &JournalConfig,
    (kind, lane): (RingKind, u16),
    mut ordinals: Vec<u32>,
    (want, file_len): (usize, u64),
    zero: &AlignedBuf,
    created_any: &mut bool,
) -> Result<LaneRing> {
    ordinals.sort_unstable();
    let mut files = Vec::new();
    let mut slots = Vec::new();
    for &ordinal in &ordinals {
        let path = cfg.dir.join(file_name(kind, lane, ordinal));
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
        let (file, gen) = create_file(cfg, &cfg.dir, (kind, lane, next_ordinal), file_len, zero)
            .map_err(io_err)?;
        *created_any = true;
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
    Ok(LaneRing {
        kind,
        lane,
        file_len,
        files,
        inner: Mutex::new(RingInner {
            slots,
            free: VecDeque::new(),
            active: None,
            resume_off: B,
            next_ordinal,
        }),
    })
}

/// What [`Rings::reinit_slot`] is asked to do.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reinit {
    pub old_ordinal: u32,
    pub new_ordinal: u32,
    /// End of the region that may hold non-zero bytes.
    pub zero_to: u64,
}

impl Rings {
    /// Zeroes `[4096, to)` of a file being re-initialised: with
    /// `FALLOC_FL_ZERO_RANGE` when the file system does it (a metadata
    /// operation on ext4 and xfs: nothing is written), else by writing zeros.
    /// Either way the range reads as zeros once the caller's `fdatasync`
    /// returned (on-disk-formats §5.1).
    fn zero_body(&self, file: &std::fs::File, to: u64, zero: &AlignedBuf) -> io::Result<()> {
        let to = sys::round4k(to);
        if !self.zero_range_off.load(Ordering::Relaxed) {
            self.zero_attempts.fetch_add(1, Ordering::Relaxed);
            let injected = self.zero_inject_errno.load(Ordering::Relaxed);
            let res = if injected != 0 {
                Err(io::Error::from_raw_os_error(injected))
            } else {
                sys::fallocate_zero(file, B, to)
            };
            match res {
                Ok(()) => {
                    self.zero_failures.store(0, Ordering::Relaxed);
                    self.zeroed_by_range.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
                Err(e) if sys::zero_range_unsupported(&e) => {
                    tracing::info!(
                        error = %e,
                        "the journal's file system does not do FALLOC_FL_ZERO_RANGE: freed ring files are rewritten with zeros"
                    );
                    self.zero_range_off.store(true, Ordering::Relaxed);
                }
                // Any other failure (ENOSPC on a nearly full copy-on-write or
                // thin file system, EIO, EPERM): this file is rewritten, the
                // error is logged once per errno, and after a few in a row
                // the range is not tried again. A call that failed half way
                // (xfs punches, then allocates) may have left holes in the
                // ring file: the range is allocated again before the rewrite,
                // so the file never stays sparse (a hole record must find its
                // room on a full volume, ADR-32).
                Err(e) => {
                    let errno = e.raw_os_error().unwrap_or(0);
                    if lock(&self.zero_logged).insert(errno) {
                        tracing::warn!(error = %e, "FALLOC_FL_ZERO_RANGE of a freed ring file failed: rewriting it with zeros (logged once per error)");
                    }
                    let _ = sys::fallocate_keep(file, B, to);
                    if self.zero_failures.fetch_add(1, Ordering::Relaxed) + 1
                        >= ZERO_RANGE_MAX_FAILURES
                    {
                        tracing::warn!(
                            failures = ZERO_RANGE_MAX_FAILURES,
                            "FALLOC_FL_ZERO_RANGE failed in a row: freed ring files are rewritten with zeros from now on"
                        );
                        self.zero_range_off.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
        self.zeroed_by_rewrite.fetch_add(1, Ordering::Relaxed);
        sys::zero_range(file, B, to, zero)
    }

    /// Re-initialises one slot as a free file with a new ordinal and
    /// generation: zero `[4096, hi)`, `fdatasync`, new header, `fdatasync`,
    /// rename. The caller syncs the directory. Returns the new generation.
    pub(crate) fn reinit_slot(
        &self,
        cfg: &JournalConfig,
        ring: &LaneRing,
        file: &RingFile,
        r: Reinit,
        zero: &AlignedBuf,
    ) -> io::Result<u64> {
        let Reinit {
            old_ordinal,
            new_ordinal,
            zero_to,
        } = r;
        let len = file.file.metadata()?.len();
        if len != ring.file_len {
            file.file.set_len(ring.file_len)?;
        }
        self.zero_body(&file.file, zero_to.min(ring.file_len), zero)?;
        file.file.sync_all()?;
        let gen = sys::random_nonzero()?;
        let hdr = FileHeader {
            lane: ring.lane,
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
        std::fs::rename(self.path(ring, old_ordinal), self.path(ring, new_ordinal))?;
        Ok(gen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        use RingKind::{Mark, Queue};
        assert_eq!(file_name(Queue, 0, 1), "q00-00000001.j");
        assert_eq!(file_name(Queue, 255, 0xabcd), "qff-0000abcd.j");
        assert_eq!(file_name(Mark, 0, 7), "m00-00000007.j");
        assert_eq!(parse_name("q00-00000001.j"), Some((Queue, 0, 1)));
        assert_eq!(parse_name("qff-0000abcd.j"), Some((Queue, 255, 0xabcd)));
        assert_eq!(parse_name("m00-00000007.j"), Some((Mark, 0, 7)));
        for bad in [
            "q00-00000000.j",
            "q0-00000001.j",
            "q00-1.j",
            "q00-00000001.j.tmp",
            "x00-00000001.j",
            "q00_00000001.j",
            "q00-0000000g.j",
            "m01-00000001.j",
            "m00-00000000.j",
        ] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
    }
}
