//! Helpers shared by the integration tests of `lunix-journal`.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use lunix_format::journal::RecordKind;
use lunix_format::{VolumeUuid, WriteSeq};
use lunix_iface::journal::{
    Appended, Journal, JournalCompletion, JournalLane, LaneId, SlotOutcome,
};
use lunix_iface::wake::WakeFd;
use lunix_journal::{FileJournal, JournalConfig};

pub const T: Duration = Duration::from_secs(20);
pub const UUID: VolumeUuid = VolumeUuid([7; 16]);
/// Small ring files so a ring wraps cheaply (a header plus two maximal records).
pub const SMALL_FILE: u64 = 2 << 20;

/// A scratch directory under `LUNIX_JOURNAL_TEST_DIR`, else `LUNIX_SPIKE_DIR`,
/// else the system temporary directory.
pub fn scratch() -> tempfile::TempDir {
    let base = std::env::var_os("LUNIX_JOURNAL_TEST_DIR")
        .or_else(|| std::env::var_os("LUNIX_SPIKE_DIR"))
        .map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&base).expect("scratch base");
    tempfile::Builder::new()
        .prefix("lunix-journal-test-")
        .tempdir_in(base)
        .expect("tempdir")
}

pub fn small_cfg(dir: PathBuf, lanes: u16, files_per_lane: u64) -> JournalConfig {
    let mut c = JournalConfig::new(dir, UUID, lanes, 0);
    c.file_len = SMALL_FILE;
    c.journal_max = u64::from(lanes) * files_per_lane * SMALL_FILE;
    c.pool_buffers = 4;
    c
}

pub fn open_lane(j: &dyn Journal, lane: u16) -> Box<dyn JournalLane> {
    j.open_lane(LaneId(lane), WakeFd::new().expect("eventfd"))
        .expect("open lane")
}

/// The data of a record: every byte derives from the sequence number.
pub fn pattern(seq: u64, blocks: u32) -> Vec<u8> {
    let mut v = vec![0u8; blocks as usize * 4096];
    for (i, b) in v.iter_mut().enumerate() {
        *b = (seq as usize).wrapping_mul(31).wrapping_add(i) as u8;
    }
    v
}

/// Reserves and appends a write whose data derives from its sequence number.
pub fn write(lane: &mut dyn JournalLane, lba: u64, blocks: u32) -> Appended {
    match lane
        .try_reserve(RecordKind::Write, blocks)
        .expect("reserve")
    {
        SlotOutcome::Reserved(slot) => {
            let data = pattern(slot.write_seq.0, blocks);
            lane.append(slot, lba, &data).expect("append")
        }
        other => panic!("reservation refused: {other:?}"),
    }
}

pub fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !cond() {
        assert!(start.elapsed() < T, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Polls until `n` append completions arrived; returns their sequence numbers.
pub fn complete_appends(lane: &mut dyn JournalLane, n: usize) -> Vec<WriteSeq> {
    let mut done = Vec::new();
    wait_until("append completions", || {
        let mut out = Vec::new();
        lane.poll(&mut out);
        for c in out {
            if let JournalCompletion::Append { write_seq, status } = c {
                assert!(status.is_ok(), "append failed: {status:?}");
                done.push(write_seq);
            }
        }
        done.len() >= n
    });
    done
}

pub fn group_flush(j: &dyn Journal) {
    let (tx, rx) = mpsc::channel();
    j.submit_group_flush(
        j.last_issued(),
        Box::new(move |r| {
            let _ = tx.send(r.is_ok());
        }),
    )
    .expect("submit flush");
    assert!(rx.recv_timeout(T).expect("flush completion"), "flush failed");
}

pub fn recover_all(j: &dyn Journal) -> lunix_iface::journal::Recovered {
    j.recover(None, WriteSeq(0)).expect("recover")
}

pub fn open_journal(cfg: &JournalConfig) -> FileJournal {
    FileJournal::open(cfg.clone()).expect("open journal")
}

/// True when the scratch file system accepts `O_DIRECT`.
pub fn o_direct_supported(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    let p = dir.join("odirect-probe");
    let ok = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_DIRECT)
        .open(&p)
        .is_ok();
    let _ = std::fs::remove_file(&p);
    ok
}

/// A record as written, with where it lives.
#[derive(Clone, Copy, Debug)]
pub struct Written {
    pub seq: u64,
    pub lane: u16,
    pub ordinal: u32,
    /// File offset of the record header.
    pub start: u64,
    pub len: u64,
    pub blocks: u32,
    pub lba: u64,
}

/// Reserves and appends one write with a chosen `lba`; data derives from the
/// sequence number.
pub fn write_rec(lane: &mut dyn JournalLane, lba: u64, blocks: u32) -> Written {
    let l = lane.lane().0;
    match lane
        .try_reserve(RecordKind::Write, blocks)
        .expect("reserve")
    {
        SlotOutcome::Reserved(slot) => {
            let data = pattern(slot.write_seq.0, blocks);
            let a = lane.append(slot, lba, &data).expect("append");
            Written {
                seq: a.write_seq.0,
                lane: l,
                ordinal: a.loc.ordinal,
                start: a.loc.offset - 64,
                len: 64 + u64::from(blocks) * 4096,
                blocks,
                lba,
            }
        }
        other => panic!("reservation refused: {other:?}"),
    }
}

pub fn path_of(cfg: &JournalConfig, lane: u16, ordinal: u32) -> PathBuf {
    cfg.dir.join(format!("q{lane:02x}-{ordinal:08x}.j"))
}

pub fn ring_files(cfg: &JournalConfig) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(&cfg.dir)
        .expect("readdir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Overwrites `[off, off + len)` of a file with zeros (buffered I/O; the
/// journal must not be open).
pub fn zero_bytes(path: &std::path::Path, off: u64, len: u64) {
    use std::os::unix::fs::FileExt;
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open");
    f.write_all_at(&vec![0u8; len as usize], off).expect("zero");
    f.sync_all().expect("sync");
}

/// Flips every bit of the byte at `off`.
pub fn flip_byte(path: &std::path::Path, off: u64) {
    use std::os::unix::fs::FileExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open");
    let mut b = [0u8; 1];
    f.read_exact_at(&mut b, off).expect("read");
    b[0] ^= 0xff;
    f.write_all_at(&b, off).expect("write");
    f.sync_all().expect("sync");
}

pub fn write_at(path: &std::path::Path, off: u64, bytes: &[u8]) {
    use std::os::unix::fs::FileExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open");
    f.write_all_at(bytes, off).expect("write");
    f.sync_all().expect("sync");
}

pub fn read_at_path(path: &std::path::Path, off: u64, len: usize) -> Vec<u8> {
    use std::os::unix::fs::FileExt;
    let f = std::fs::File::open(path).expect("open");
    let mut v = vec![0u8; len];
    f.read_exact_at(&mut v, off).expect("read");
    v
}

pub fn seqs(r: &lunix_iface::journal::Recovered) -> Vec<u64> {
    r.records.iter().map(|x| x.record.write_seq.0).collect()
}
