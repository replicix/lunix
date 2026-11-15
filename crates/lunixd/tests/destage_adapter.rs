//! M9 re-review R1 (decided: classify once): the adapter copies M9's
//! `ErrorClass` into the engine's `Retry` and never re-classifies. For every
//! `ErrorKind` (and the host `EIO`/`EROFS` cases), a real `destage_cut` fails
//! on a segment store that refuses with that error, and the `Retry` the
//! engine receives is the one M9's table prescribes; the engine's own table
//! (`Retry::classify`, for the stand-in destager) agrees. Also the outcome
//! mapping (segments sealed, index inserts in order, counters).

use std::sync::{Arc, Mutex};

use lunix_destage::{DestageConfig, DestageParams};
use lunix_engine::{ChunkReader, DestageCtx, Destager, Retry};
use lunix_format::journal::RecordKind;
use lunix_format::node::DataEntry;
use lunix_format::volume::CodecPolicy;
use lunix_format::{ChunkId, Hint, IdLen, SegId, TimeMs, WriteSeq};
use lunix_iface::clock::ManualClock;
use lunix_iface::crypto::ExtentCodec;
use lunix_iface::journal::{Journal, LaneId, SlotOutcome};
use lunix_iface::overlay::{BlockRange, Overlay};
use lunix_iface::store::{ExtentLocation, PackRead, SegmentInfo, SegmentStore, SegmentWriter};
use lunix_iface::wake::WakeFd;
use lunix_iface::{Error, ErrorKind, Result};
use lunix_testkit::journal::MIN_FILE_BYTES;
use lunix_testkit::{
    FlatMap, FlatOverlay, MemIndex, MemJournal, MemJournalConfig, MemSpace, MemSpaceConfig,
    MemStore, MemStoreConfig, PlainCodec,
};
use lunixd::stack::PipelineDestager;

const CS: u32 = 16 << 10;

/// A segment store whose `open_segment` fails with a chosen error.
struct Refusing {
    inner: MemStore,
    fail: Mutex<Option<Error>>,
}

impl SegmentStore for Refusing {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        match self.fail.lock().unwrap().take() {
            Some(e) => Err(e),
            None => self.inner.open_segment(),
        }
    }
    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
        self.inner.read_extent(hint, out)
    }
    fn locate(&self, hint: &Hint) -> Result<ExtentLocation> {
        self.inner.locate(hint)
    }
    fn footer(&self, seg: SegId) -> Result<lunix_format::segment::FooterBody> {
        self.inner.footer(seg)
    }
    fn list(&self) -> Result<Vec<SegmentInfo>> {
        SegmentStore::list(&self.inner)
    }
    fn delete(&self, seg: SegId) -> Result<u64> {
        SegmentStore::delete(&self.inner, seg)
    }
}

struct NoReader;

impl ChunkReader for NoReader {
    fn read_entry(&self, _: &DataEntry, _: &mut Vec<u8>) -> Result<()> {
        Err(Error::invalid(
            "the adapter must not use DestageCtx::reader",
        ))
    }
    fn confirm(&self, _: &Hint, _: &ChunkId) -> Result<bool> {
        Err(Error::invalid(
            "the adapter must not use DestageCtx::reader",
        ))
    }
}

fn data(seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..CS as usize)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Runs the adapter over a two-chunk cut (one chunk repeated) with `fail`
/// injected; returns the engine-side result.
fn run(
    fail: Option<Error>,
) -> std::result::Result<lunix_engine::DestageOutcome, lunix_engine::DestageFailure> {
    let codec: Arc<dyn ExtentCodec> = Arc::new(PlainCodec::new());
    let store = MemStore::with_config(MemStoreConfig {
        mode: codec.mode(),
        ..MemStoreConfig::default()
    });
    let index = MemIndex::new(store.mem_ids());
    let map = FlatMap::new(
        Arc::clone(&codec),
        Arc::new(store.clone()) as Arc<dyn PackRead>,
        64,
        IdLen::Sixteen,
    );
    let journal = MemJournal::new(MemJournalConfig {
        lanes: 1,
        file_bytes: (8 << 20).max(MIN_FILE_BYTES),
        max_files: 64,
        ..MemJournalConfig::default()
    });
    journal.recover(None, WriteSeq(0)).unwrap();
    let mut lane = journal
        .open_lane(LaneId(0), WakeFd::new().unwrap())
        .unwrap();
    let overlay = FlatOverlay::new(u64::from(CS) / 4096);
    for (chunk, seed) in [(0u64, 1u64), (1, 2), (5, 1)] {
        let SlotOutcome::Reserved(slot) = lane.try_reserve(RecordKind::Write, CS / 4096).unwrap()
        else {
            panic!("journal full");
        };
        let lba = chunk * u64::from(CS) / 4096;
        let ap = lane.append(slot, lba, &data(seed)).unwrap();
        overlay.insert_write(
            BlockRange {
                lba,
                blocks: u64::from(CS / 4096),
            },
            ap.loc,
            ap.write_seq,
        );
    }
    let cut = overlay.cut(journal.last_issued());
    let space = MemSpace::new(
        MemSpaceConfig::new(1 << 40),
        Arc::new(ManualClock::new(TimeMs(1))),
    );
    let segments = Refusing {
        inner: store,
        fail: Mutex::new(fail),
    };
    let ctx = DestageCtx {
        journal: &journal,
        map: &map,
        index: &index,
        segments: &segments,
        codec: &*codec,
        reader: &NoReader,
        space: &*space,
        chunk_size: CS,
        id_len: IdLen::Sixteen,
        writes: None,
    };
    let d = PipelineDestager::new(
        DestageParams {
            chunk_size: CS,
            id_len: IdLen::Sixteen,
            policy: CodecPolicy::None,
            level: 0,
        },
        DestageConfig {
            workers: 1,
            result_window: 1,
            ..DestageConfig::default()
        },
    );
    d.destage(&cut, &ctx)
}

#[test]
fn the_outcome_maps_segments_inserts_and_counters() {
    let o = run(None).expect("destage");
    assert_eq!(o.segments.len(), 1);
    assert_eq!(o.chunks_total, 3);
    assert_eq!(o.dedup_hits, 1, "chunk 5 equals chunk 0 within the cut");
    assert_eq!(o.index_inserts.len(), 2, "two distinct extents");
    assert_eq!(o.stored_ulen, 2 * u64::from(CS));
    assert!(o.stored_clen >= o.stored_ulen, "stored raw");
    assert!(o
        .index_inserts
        .iter()
        .all(|(_, h)| h.seg == o.segments[0].seg));
    assert!(
        o.index_inserts[0].1.off < o.index_inserts[1].1.off,
        "append order"
    );
}

fn io(code: i32) -> Error {
    Error::Io(std::io::Error::from_raw_os_error(code))
}

#[test]
fn every_error_kind_reaches_the_engine_with_m9s_class() {
    // (error, kind, the Retry of M9's table)
    let cases: Vec<(Error, ErrorKind, Retry)> = vec![
        (
            Error::Io(std::io::Error::other("transient")),
            ErrorKind::Io,
            Retry::Later,
        ),
        (io(5), ErrorKind::Io, Retry::Stop),
        (io(30), ErrorKind::Io, Retry::Stop),
        (
            Error::NotFound { what: "x" },
            ErrorKind::NotFound,
            Retry::Never,
        ),
        (
            Error::AlreadyExists { what: "x" },
            ErrorKind::AlreadyExists,
            Retry::Never,
        ),
        (
            Error::Corrupt {
                what: "x",
                detail: "y",
            },
            ErrorKind::Corrupt,
            Retry::Never,
        ),
        (Error::Auth { what: "x" }, ErrorKind::Auth, Retry::Never),
        (Error::NoSpace, ErrorKind::NoSpace, Retry::AfterSpace),
        (Error::ReadOnly, ErrorKind::ReadOnly, Retry::Stop),
        (Error::MapFull, ErrorKind::MapFull, Retry::CutTooLarge),
        (
            Error::Unsupported { what: "x" },
            ErrorKind::Unsupported,
            Retry::Never,
        ),
        (
            Error::Refused { why: "x" },
            ErrorKind::Refused,
            Retry::Never,
        ),
        (Error::Busy { what: "x" }, ErrorKind::Busy, Retry::Later),
        (
            Error::Invalid { why: "x" },
            ErrorKind::Invalid,
            Retry::Never,
        ),
        (Error::Closed, ErrorKind::Closed, Retry::Stop),
    ];
    let mut kinds = std::collections::HashSet::new();
    for (e, kind, want) in cases {
        assert_eq!(e.kind(), kind);
        kinds.insert(kind);
        let engine_table = Retry::classify(&e, false);
        let f = run(Some(e)).expect_err("the injected error fails the cut");
        assert_eq!(f.error.kind(), kind, "the error is passed on unchanged");
        assert_eq!(f.retry, want, "{kind:?}: M9's class reaches the engine");
        assert_eq!(engine_table, want, "{kind:?}: the engine's table agrees");
    }
    assert_eq!(kinds.len(), 13, "every ErrorKind");
}
