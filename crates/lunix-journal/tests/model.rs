//! Model test (M3 gate): random appends across 1-8 lanes with flushes, then a
//! simulated crash that keeps every byte up to the last completed flush and
//! replaces the unflushed records by a mix of intact, torn and missing ones.
//! Recovery must return exactly a `write_seq` prefix that contains every
//! flushed record and no damaged record, with the right next sequence number;
//! a second incarnation then appends, flushes and is recovered again, which
//! catches stale records beyond the stop point coming back to life.

mod common;

use std::collections::HashMap;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use common::{
    complete_appends, group_flush, open_journal, open_lane, pattern, recover_all, scratch,
    small_cfg, write, UUID,
};
use lunix_format::WriteSeq;
use lunix_iface::journal::{Journal, JournalLane, SlotOutcome};
use lunix_journal::{FileJournal, JournalConfig};
use proptest::prelude::*;

/// A little over one maximal record: a file holds about sixteen 64 KiB records.
const FILES_PER_LANE: u64 = 3;

fn cases() -> u32 {
    std::env::var("LUNIX_MODEL_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000)
}

static TEMPLATES: Mutex<Option<(tempfile::TempDir, HashMap<u16, PathBuf>)>> = Mutex::new(None);

/// A pre-initialised journal directory for `lanes`, built once.
fn template(lanes: u16) -> PathBuf {
    let mut g = TEMPLATES.lock().unwrap_or_else(|p| p.into_inner());
    let (dir, map) = g.get_or_insert_with(|| (scratch(), HashMap::new()));
    map.entry(lanes)
        .or_insert_with(|| {
            let p = dir.path().join(format!("tpl-{lanes}"));
            let cfg = small_cfg(p.clone(), lanes, FILES_PER_LANE);
            drop(open_journal(&cfg));
            p
        })
        .clone()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    // Fresh ring files are a header block followed by zeros: copy the header
    // and let the rest be a hole, which reads as zero.
    for e in std::fs::read_dir(from).expect("readdir") {
        let e = e.expect("entry");
        let src = std::fs::File::open(e.path()).expect("open template");
        let len = src.metadata().expect("meta").len();
        let mut head = vec![0u8; 4096];
        src.read_exact_at(&mut head, 0).expect("read header");
        let dst = std::fs::File::create(to.join(e.file_name())).expect("create");
        dst.set_len(len).expect("set_len");
        dst.write_all_at(&head, 0).expect("write header");
    }
}

#[derive(Clone, Debug)]
enum Op {
    Append { lane: usize, blocks: u32 },
    Flush,
}

#[derive(Clone, Debug)]
struct Scenario {
    lanes: u16,
    ops: Vec<Op>,
    fates: Vec<u64>,
    second: Vec<(usize, u32)>,
}

fn blocks() -> impl Strategy<Value = u32> {
    prop_oneof![Just(1u32), Just(4u32), Just(16u32)]
}

fn scenario(lanes: std::ops::RangeInclusive<u16>) -> impl Strategy<Value = Scenario> {
    (
        lanes,
        proptest::collection::vec(
            prop_oneof![
                8 => (any::<usize>(), blocks()).prop_map(|(lane, blocks)| Op::Append { lane, blocks }),
                2 => Just(Op::Flush),
            ],
            1..48,
        ),
        proptest::collection::vec(any::<u64>(), 64),
        proptest::collection::vec((any::<usize>(), blocks()), 0..8),
    )
        .prop_map(|(lanes, ops, fates, second)| Scenario {
            lanes,
            ops,
            fates,
            second,
        })
}

struct Rec {
    seq: u64,
    ordinal: u32,
    lane: u16,
    start: u64,
    len: u64,
    blocks: u32,
}

fn file_of(cfg: &JournalConfig, lane: u16, ordinal: u32) -> PathBuf {
    cfg.dir.join(format!("q{lane:02x}-{ordinal:08x}.j"))
}

fn check_recovered(
    j: &dyn Journal,
    r: &lunix_iface::journal::Recovered,
    expect: &[(u64, u32)],
) -> Result<(), TestCaseError> {
    let got: Vec<u64> = r.records.iter().map(|x| x.record.write_seq.0).collect();
    let want: Vec<u64> = expect.iter().map(|e| e.0).collect();
    prop_assert_eq!(&got, &want);
    prop_assert_eq!(r.next_seq, WriteSeq(want.len() as u64 + 1));
    prop_assert_eq!(r.ignored_after_gap.is_some(), r.gap_at.is_some());
    if let Some(g) = r.gap_at {
        prop_assert_eq!(g, r.next_seq);
    }
    for (rec, &(seq, blocks)) in r.records.iter().zip(expect) {
        prop_assert_eq!(rec.record.blocks, blocks);
        let mut buf = vec![0u8; blocks as usize * 4096];
        j.read_at(rec.loc, &mut buf).expect("read_at");
        prop_assert!(buf == pattern(seq, blocks), "data of {} differs", seq);
    }
    Ok(())
}

fn run(s: &Scenario) -> Result<(), TestCaseError> {
    let t0 = std::time::Instant::now();
    let tm = |what: &str| if std::env::var_os("MODEL_TIMING").is_some() { eprintln!("{what}: {:?}", t0.elapsed()); };
    let dir = scratch();
    let jdir = dir.path().join("journal");
    copy_dir(&template(s.lanes), &jdir);
    let cfg = small_cfg(jdir, s.lanes, FILES_PER_LANE);
    tm("copied");

    // Incarnation 1.
    let mut recs: Vec<Rec> = Vec::new();
    let mut flushed = 0u64;
    {
        let j = open_journal(&cfg);
        recover_all(&j);
        let mut lanes: Vec<Box<dyn JournalLane>> =
            (0..s.lanes).map(|l| open_lane(&j, l)).collect();
        let mut waiting = vec![0usize; usize::from(s.lanes)];
        for op in &s.ops {
            match *op {
                Op::Append { lane, blocks } => {
                    let l = lane % usize::from(s.lanes);
                    match lanes[l]
                        .try_reserve(lunix_format::journal::RecordKind::Write, blocks)
                        .expect("reserve")
                    {
                        SlotOutcome::Reserved(slot) => {
                            let data = pattern(slot.write_seq.0, blocks);
                            let a = lanes[l]
                                .append(slot, 8 * recs.len() as u64, &data)
                                .expect("append");
                            waiting[l] += 1;
                            recs.push(Rec {
                                seq: a.write_seq.0,
                                ordinal: a.loc.ordinal,
                                lane: l as u16,
                                start: a.loc.offset - 64,
                                len: 64 + u64::from(blocks) * 4096,
                                blocks,
                            });
                        }
                        SlotOutcome::Full | SlotOutcome::NoSpace => {}
                    }
                }
                Op::Flush => {
                    for (l, w) in waiting.iter_mut().enumerate() {
                        complete_appends(&mut *lanes[l], *w);
                        *w = 0;
                    }
                    group_flush(&j);
                    flushed = j.last_issued().0;
                }
            }
        }
        // Dropping the lanes waits for their writes; the files then hold every
        // record, flushed or not.
    }

    tm("inc1");
    // The crash: unflushed records become intact, torn or missing.
    for r in recs.iter().filter(|r| r.seq > flushed) {
        let path = file_of(&cfg, r.lane, r.ordinal);
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open for damage");
        let fate = s.fates[r.seq as usize % s.fates.len()];
        let keep = match fate % 4 {
            0 | 1 => r.len,
            2 => 0,
            _ => (fate >> 8) % r.len,
        };
        if keep < r.len {
            let zeros = vec![0u8; (r.len - keep) as usize];
            f.write_all_at(&zeros, r.start + keep).expect("damage");
        }
    }
    // What is intact now: the flushed prefix and whatever survived whole. A
    // torn record whose lost tail was zero anyway is intact too, so compare
    // with the original bytes.
    let mut intact: Vec<bool> = Vec::new();
    for r in &recs {
        let f = std::fs::File::open(file_of(&cfg, r.lane, r.ordinal)).expect("open");
        let mut got = vec![0u8; (r.len - 64) as usize];
        f.read_exact_at(&mut got, r.start + 64).expect("read");
        let mut hdr = [0u8; 4];
        f.read_exact_at(&mut hdr, r.start).expect("read");
        // The CRC covers header and data, so a record is intact iff its data
        // equals the pattern and its header magic is present; any change to
        // the header region is caught by the CRC test below instead.
        intact.push(&hdr == b"LXJR" && got == pattern(r.seq, r.blocks));
    }
    let mut by_seq: Vec<&Rec> = recs.iter().collect();
    by_seq.sort_by_key(|r| r.seq);
    let mut k = 0u64;
    let mut expect: Vec<(u64, u32)> = Vec::new();
    for r in &by_seq {
        // Header damage (a torn prefix shorter than 64 bytes) is not visible
        // above, so a record counts only when the whole record reads back.
        let whole = {
            let f = std::fs::File::open(file_of(&cfg, r.lane, r.ordinal)).expect("open");
            let mut head = [0u8; 64];
            f.read_exact_at(&mut head, r.start).expect("read");
            let idx = recs.iter().position(|x| x.seq == r.seq).expect("rec");
            intact[idx] && lunix_format::journal::Record::parse(&{
                let mut all = vec![0u8; r.len as usize];
                f.read_exact_at(&mut all, r.start).expect("read");
                all
            })
            .is_ok()
        };
        if whole && r.seq == k + 1 {
            k += 1;
            expect.push((r.seq, r.blocks));
        } else {
            break;
        }
    }
    prop_assert!(k >= flushed, "model: flushed records must be intact");

    tm("crash");
    // Incarnation 2: recover, check, append more, flush, crash cleanly, recover.
    let mut total = expect.clone();
    {
        let j = open_journal(&cfg);
        let r = recover_all(&j);
        check_recovered(&j, &r, &expect)?;
        if let Some(range) = r.ignored_after_gap {
            prop_assert!(range.first.0 > k && range.last >= range.first);
        }
        let mut lanes: Vec<Box<dyn JournalLane>> =
            (0..s.lanes).map(|l| open_lane(&j, l)).collect();
        let mut waiting = vec![0usize; usize::from(s.lanes)];
        for &(lane, blocks) in &s.second {
            let l = lane % usize::from(s.lanes);
            let a = write(&mut *lanes[l], 1000, blocks);
            prop_assert_eq!(a.write_seq.0, total.len() as u64 + 1);
            total.push((a.write_seq.0, blocks));
            waiting[l] += 1;
        }
        for (l, w) in waiting.iter().enumerate() {
            complete_appends(&mut *lanes[l], *w);
        }
        group_flush(&j);
    }
    tm("inc2");
    let j = open_journal(&cfg);
    let r = recover_all(&j);
    check_recovered(&j, &r, &total)?;
    tm("final");
    prop_assert!(r.ignored_after_gap.is_none(), "stale records came back");
    drop(j);
    let _ = UUID;
    Ok(())
}

macro_rules! model_test {
    ($name:ident, $lanes:expr) => {
        proptest! {
            #![proptest_config(ProptestConfig {
                cases: cases() / 4,
                max_shrink_iters: 64,
                failure_persistence: None,
                ..ProptestConfig::default()
            })]

            #[test]
            fn $name(s in scenario($lanes)) {
                run(&s)?;
            }
        }
    };
}

// Four tests so the cases run on four threads; together they cover 1-8 lanes.
model_test!(crash_recovery_prefix_lanes_1_2, 1..=2);
model_test!(crash_recovery_prefix_lanes_3_4, 3..=4);
model_test!(crash_recovery_prefix_lanes_5_6, 5..=6);
model_test!(crash_recovery_prefix_lanes_7_8, 7..=8);

#[allow(dead_code)]
fn _types(_: &FileJournal) {}
