//! Freeze item 2: journal file header, record, batch packing, the reader.

mod common;

use common::{golden, hex};

mod crc_helper {
    pub fn crc(b: &[u8]) -> u32 {
        lunix_format::bytes::crc32c(b)
    }
}

use lunix_format::journal::*;
use lunix_format::{FormatError, VolumeUuid, WriteSeq};
use proptest::prelude::*;

const GEN: u64 = 0x9e37_79b9_7f4a_7c15;
const UUID_B: [u8; 16] = common::VOLUME_B_UUID;

fn header_b() -> FileHeader {
    FileHeader {
        lane: 0,
        ordinal: 1,
        volume_uuid: VolumeUuid(UUID_B),
        generation: GEN,
    }
}

fn header_block() -> Vec<u8> {
    let mut v = Vec::new();
    header_b().encode(&mut v).unwrap();
    v
}

fn rt_header(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let h = FileHeader::decode(b)?;
    let mut v = Vec::new();
    h.encode(&mut v)?;
    Ok(v)
}

fn rt_record(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let r = Record::decode(b)?;
    let mut v = Vec::new();
    r.encode(&mut v)?;
    Ok(v)
}

fn rt_scan(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    scan_file(b, WriteSeq(0)).map(|_| b.to_vec())
}

/// Section 23.1 assembled by hand from the doc's field tables.
fn doc_file_header() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"LUNIXJNL");
    v.extend_from_slice(&hex("0100 0000 01000000"));
    v.extend_from_slice(&hex("b0b1b2b3b4b546b788b9babbbcbdbebf"));
    v.extend_from_slice(&hex("157c4a7fb979379e"));
    v.extend_from_slice(&[0; 20]);
    v.extend_from_slice(&hex("81809d89"));
    v.resize(4096, 0);
    v
}

fn doc_batch() -> Vec<u8> {
    let mut v = Vec::new();
    // record 1 at 0x1000
    v.extend_from_slice(&hex("4c584a52 01 01 0000"));
    v.extend_from_slice(&hex("0100000000000000"));
    v.extend_from_slice(&[0; 8]);
    v.extend_from_slice(&[0; 8]);
    v.extend_from_slice(&hex("04000000"));
    v.extend_from_slice(&hex("2218f7cf"));
    v.extend_from_slice(&hex("157c4a7fb979379e"));
    v.extend_from_slice(&hex("0010000000000000"));
    v.extend_from_slice(&[0; 8]);
    v.extend_from_slice(&[0x41; 16384]);
    // record 2 at 0x5040
    v.extend_from_slice(&hex("4c584a52 01 02 0000"));
    v.extend_from_slice(&hex("0200000000000000"));
    v.extend_from_slice(&[0; 8]);
    v.extend_from_slice(&hex("0001000000000000"));
    v.extend_from_slice(&hex("10000000"));
    v.extend_from_slice(&hex("cde86529"));
    v.extend_from_slice(&hex("157c4a7fb979379e"));
    v.extend_from_slice(&hex("4050000000000000"));
    v.extend_from_slice(&[0; 8]);
    v.resize(20480, 0);
    v
}

fn builder() -> BatchBuilder {
    BatchBuilder::new(GEN, 4096).unwrap()
}

fn file_of(blocks: &[&[u8]]) -> Vec<u8> {
    let mut f = header_block();
    for b in blocks {
        f.extend_from_slice(b);
    }
    f
}

#[test]
fn a0_file_header_matches_the_worked_example() {
    let doc = doc_file_header();
    assert_eq!(header_block(), doc);
    assert_eq!(crc_helper::crc(&doc[..60]), 0x899d_8081);
    let g = golden("a0-journal-file-header.bin", &doc);
    common::negative_suite("file header", &rt_header, &g);
    assert_eq!(FileHeader::decode(&g).unwrap(), header_b());
}

#[test]
fn a0_batch_matches_the_worked_example() {
    let mut b = builder();
    b.push_write(WriteSeq(1), 0, 0, &[0x41; 16384]).unwrap();
    b.push_hole(WriteSeq(2), 0, 256, 16).unwrap();
    assert_eq!(b.len(), 16512);
    let batch = b.finish();
    assert_eq!(batch, doc_batch());
    let file = file_of(&[&batch]);
    assert_eq!(file.len(), 6 * 4096);
    let g = golden("a0-journal-batch-two-records.bin", &file);
    common::stream_suite("batch file", &rt_scan, &g);
    let scan = scan_file(&g, WriteSeq(0)).unwrap();
    assert_eq!(scan.records.len(), 2);
    assert!(scan.damaged.is_empty());
    assert_eq!(scan.records[0].header.off, 0x1000);
    assert_eq!(scan.records[1].header.off, 0x5040);
    assert_eq!(scan.records[1].header.kind, RecordKind::Hole);
    assert_eq!(scan.records[1].header.lba, 256);
}

#[test]
fn single_records_have_goldens() {
    let mut w = Vec::new();
    Record {
        header: RecordHeader {
            kind: RecordKind::Write,
            write_seq: WriteSeq(7),
            epoch: 0,
            lba: 12,
            len: 1,
            gen: GEN,
            off: 4096,
            flushed: WriteSeq(0),
        },
        data: &[0x5a; 4096],
    }
    .encode(&mut w)
    .unwrap();
    assert_eq!(w.len(), 64 + 4096);
    let g = golden("02-journal-record-write-1-block.bin", &w);
    common::negative_suite("write record", &rt_record, &g);

    let mut h = Vec::new();
    Record {
        header: RecordHeader {
            kind: RecordKind::Hole,
            write_seq: WriteSeq(8),
            epoch: 0,
            lba: 1 << 20,
            len: u32::MAX,
            gen: GEN,
            off: 8192,
            flushed: WriteSeq(0),
        },
        data: &[],
    }
    .encode(&mut h)
    .unwrap();
    assert_eq!(h.len(), 64);
    let g = golden("02-journal-record-hole.bin", &h);
    common::negative_suite("hole record", &rt_record, &g);
}

fn three_block_records() -> Vec<u8> {
    let mut b = builder();
    for i in 1..=3u64 {
        b.push_write(WriteSeq(i), 0, i * 8, &vec![i as u8; 4096])
            .unwrap();
    }
    b.finish()
}

#[test]
fn torn_batch_loses_only_the_torn_record() {
    let mut batch = three_block_records();
    assert_eq!(batch.len(), 16384);
    let n = batch.len();
    batch[n - 4096..].fill(0);
    let file = file_of(&[&batch]);
    let g = golden("02-journal-torn-batch.bin", &file);
    common::stream_suite("torn batch", &rt_scan, &g);
    let scan = scan_file(&g, WriteSeq(0)).unwrap();
    let seqs: Vec<u64> = scan.records.iter().map(|r| r.header.write_seq.0).collect();
    assert_eq!(seqs, [1, 2]);
    assert_eq!(scan.damaged.len(), 1);
    assert_eq!(scan.damaged[0].start, 4096 + 2 * 4160);
}

#[test]
fn reader_resyncs_past_a_damaged_record() {
    let mut b = builder();
    for i in 1..=4u64 {
        b.push_write(WriteSeq(i), 0, i, &vec![i as u8; 4096])
            .unwrap();
    }
    let mut batch = b.finish();
    // damage record 2's data
    batch[4160 + 64 + 10] ^= 0xff;
    let file = file_of(&[&batch]);
    let g = golden("02-journal-resync.bin", &file);
    common::stream_suite("resync", &rt_scan, &g);
    let scan = scan_file(&g, WriteSeq(0)).unwrap();
    let seqs: Vec<u64> = scan.records.iter().map(|r| r.header.write_seq.0).collect();
    assert_eq!(seqs, [1, 3, 4]);
    assert_eq!(scan.damaged.len(), 1);
    assert_eq!(scan.damaged[0].start, 4096 + 4160);
    assert_eq!(scan.damaged[0].end, 4096 + 2 * 4160);
}

#[test]
fn zero_magic_at_a_block_boundary_ends_the_file() {
    // a second batch after a gap of one zero block is never read
    let mut b1 = builder();
    b1.push_write(WriteSeq(1), 0, 0, &[1; 4096]).unwrap();
    let b1 = b1.finish(); // 2 blocks
    let mut b2 = BatchBuilder::new(GEN, 4096 + 3 * 4096).unwrap();
    b2.push_write(WriteSeq(2), 0, 1, &[2; 4096]).unwrap();
    let b2 = b2.finish();
    let file = file_of(&[&b1, &[0; 4096], &b2]);
    let scan = scan_file(&file, WriteSeq(0)).unwrap();
    assert_eq!(scan.records.len(), 1);
    assert!(scan.damaged.is_empty());
}

#[test]
fn write_seq_must_rise() {
    let mut b = builder();
    b.push_write(WriteSeq(5), 0, 0, &[1; 4096]).unwrap();
    b.push_write(WriteSeq(5), 0, 1, &[2; 4096]).unwrap();
    let file = file_of(&[&b.finish()]);
    let scan = scan_file(&file, WriteSeq(0)).unwrap();
    assert_eq!(scan.records.len(), 1);
    assert_eq!(scan.damaged.len(), 1);
    // and a previous file's last sequence raises the floor
    let scan = scan_file(&file, WriteSeq(5)).unwrap();
    assert!(scan.records.is_empty());
}

#[test]
fn records_of_another_generation_or_offset_are_rejected() {
    let mut other = BatchBuilder::new(GEN + 1, 4096).unwrap();
    other.push_write(WriteSeq(1), 0, 0, &[1; 4096]).unwrap();
    let file = file_of(&[&other.finish()]);
    assert!(scan_file(&file, WriteSeq(0)).unwrap().records.is_empty());
    // a record copied to another offset: same bytes, wrong `off` (and a
    // sequence that does not rise), so the copy is skipped
    let mut b = builder();
    b.push_write(WriteSeq(1), 0, 0, &[1; 4096]).unwrap();
    let mut batch = b.finish();
    let copy = batch[..64 + 4096].to_vec();
    batch.truncate(64 + 4096);
    batch.extend_from_slice(&copy);
    batch.resize(3 * 4096, 0);
    let file = file_of(&[&batch]);
    let scan = scan_file(&file, WriteSeq(0)).unwrap();
    assert_eq!(scan.records.len(), 1);
    assert_eq!(scan.damaged.len(), 1);
}

#[test]
fn file_header_rules() {
    let good = doc_file_header();
    let fix = |mut b: Vec<u8>| {
        let c = crc_helper::crc(&b[..60]);
        b[60..64].copy_from_slice(&c.to_le_bytes());
        b
    };
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut b = good.clone();
    b[0] = b'X';
    cases.push(("magic", b));
    let mut b = good.clone();
    b[8] = 2;
    cases.push(("version", fix(b)));
    let mut b = good.clone();
    b[10] = 0;
    b[11] = 1; // lane 256
    cases.push(("lane", fix(b)));
    let mut b = good.clone();
    b[12..16].fill(0);
    cases.push(("ordinal", fix(b)));
    let mut b = good.clone();
    b[32..40].fill(0);
    cases.push(("generation", fix(b)));
    let mut b = good.clone();
    b[45] = 1;
    cases.push(("reserved", fix(b)));
    let mut b = good.clone();
    b[100] = 1;
    cases.push(("padding", b));
    let mut b = good.clone();
    b[20] ^= 1;
    cases.push(("crc", b));
    for (name, b) in cases {
        common::assert_rejects(name, &rt_header, &b);
    }
    let mut h = header_b();
    h.generation = 0;
    assert!(h.encode(&mut Vec::new()).is_err());
    h = header_b();
    h.lane = 256;
    assert!(h.encode(&mut Vec::new()).is_err());
}

#[test]
fn record_rules() {
    let mut r = Vec::new();
    Record {
        header: RecordHeader {
            kind: RecordKind::Hole,
            write_seq: WriteSeq(3),
            epoch: 9,
            lba: 4,
            len: 2,
            gen: GEN,
            off: 4096,
            flushed: WriteSeq(0),
        },
        data: &[],
    }
    .encode(&mut r)
    .unwrap();
    // the epoch is carried as stored
    assert_eq!(Record::decode(&r).unwrap().header.epoch, 9);
    let fix = |mut b: Vec<u8>| {
        b[36..40].fill(0);
        let c = crc_helper::crc(&b);
        b[36..40].copy_from_slice(&c.to_le_bytes());
        b
    };
    let mut cases = Vec::new();
    let mut b = r.clone();
    b[4] = 2;
    cases.push(("version", fix(b)));
    let mut b = r.clone();
    b[5] = 3;
    cases.push(("kind", fix(b)));
    let mut b = r.clone();
    b[6] = 1;
    cases.push(("reserved 6", fix(b)));
    let mut b = r.clone();
    let seq = b[8..16].to_vec();
    b[56..64].copy_from_slice(&seq);
    cases.push(("flushed watermark not below write_seq", fix(b)));
    let mut b = r.clone();
    b[8..16].fill(0);
    cases.push(("write_seq 0", fix(b)));
    let mut b = r.clone();
    b[32..36].fill(0);
    cases.push(("hole len 0", fix(b)));
    let mut b = r.clone();
    b[5] = 1;
    b[32..36].copy_from_slice(&257u32.to_le_bytes());
    cases.push(("write len 257", fix(b)));
    let mut b = r.clone();
    b[40] ^= 1;
    cases.push(("crc", b));
    for (name, b) in cases {
        common::assert_rejects(name, &rt_record, &b);
    }
    // encode-side validation
    let bad = |kind, len, data: &[u8]| {
        Record {
            header: RecordHeader {
                kind,
                write_seq: WriteSeq(1),
                epoch: 0,
                lba: 0,
                len,
                gen: 1,
                off: 4096,
                flushed: WriteSeq(0),
            },
            data,
        }
        .encode(&mut Vec::new())
        .is_err()
    };
    assert!(bad(RecordKind::Write, 2, &[0; 4096]));
    assert!(bad(RecordKind::Write, 257, &vec![0; 257 * 4096]));
    assert!(bad(RecordKind::Hole, 1, &[0; 4096]));
    assert!(bad(RecordKind::Hole, 0, &[]));
}

#[test]
fn delta_region_round_trip_and_rules() {
    let d1 = [1u8; 4096];
    let recs = [
        Record {
            header: RecordHeader {
                kind: RecordKind::Write,
                write_seq: WriteSeq(10),
                epoch: 0,
                lba: 5,
                len: 1,
                gen: 99,
                off: 99,
                flushed: WriteSeq(0),
            },
            data: &d1,
        },
        Record {
            header: RecordHeader {
                kind: RecordKind::Hole,
                write_seq: WriteSeq(11),
                epoch: 0,
                lba: 9,
                len: 3,
                gen: 0,
                off: 0,
                flushed: WriteSeq(0),
            },
            data: &[],
        },
    ];
    let mut region = Vec::new();
    encode_delta_region(&recs, &mut region).unwrap();
    assert_eq!(region.len(), 64 + 4096 + 64);
    let back = decode_delta_region(&region).unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].header.gen, 0);
    assert_eq!(back[1].header.off, 64 + 4096);
    assert_eq!(back[0].data, &d1);
    // a trailing byte, or a journal-style record (gen != 0), is refused
    let mut extra = region.clone();
    extra.push(0);
    assert!(decode_delta_region(&extra).is_err());
    let mut with_gen = Vec::new();
    recs[0].encode(&mut with_gen).unwrap();
    assert!(decode_delta_region(&with_gen).is_err());
    assert!(decode_delta_region(&[]).unwrap().is_empty());
    // sequence must rise
    let swapped = [recs[1], recs[0]];
    assert!(encode_delta_region(&swapped, &mut Vec::new()).is_err());
    common::mutations_are_safe(
        "delta region",
        &|b| {
            let r = decode_delta_region(b)?;
            let mut v = Vec::new();
            encode_delta_region(&r, &mut v)?;
            Ok(v)
        },
        &region,
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(common::CASES))]

    #[test]
    fn batch_scan_round_trip(
        ops in proptest::collection::vec((any::<bool>(), 0u64..1000, 1u32..3, any::<u8>(), 0u64..3), 1..6),
        start in 1u64..1000,
    ) {
        let mut b = builder();
        let mut want = Vec::new();
        for (i, (is_write, lba, len, byte, epoch)) in ops.iter().enumerate() {
            let seq = WriteSeq(start + i as u64);
            if *is_write {
                let data = vec![*byte; *len as usize * 4096];
                b.push_write(seq, *epoch, *lba, &data).unwrap();
                want.push((seq, *epoch, *lba, *len, Some(data)));
            } else {
                b.push_hole(seq, *epoch, *lba, *len).unwrap();
                want.push((seq, *epoch, *lba, *len, None));
            }
        }
        let batch = b.finish();
        prop_assert_eq!(batch.len() % 4096, 0);
        let file = file_of(&[&batch]);
        let scan = scan_file(&file, WriteSeq(start - 1)).unwrap();
        prop_assert!(scan.damaged.is_empty());
        prop_assert_eq!(scan.records.len(), want.len());
        for (rec, (seq, epoch, lba, len, data)) in scan.records.iter().zip(&want) {
            prop_assert_eq!(rec.header.write_seq, *seq);
            prop_assert_eq!(rec.header.epoch, *epoch);
            prop_assert_eq!(rec.header.lba, *lba);
            prop_assert_eq!(rec.header.len, *len);
            prop_assert_eq!(rec.data, data.as_deref().unwrap_or(&[]));
            let mut again = Vec::new();
            rec.encode(&mut again).unwrap();
            prop_assert_eq!(Record::decode(&again).unwrap(), *rec);
        }
    }

    #[test]
    fn header_round_trip(lane in 0u16..=255, ordinal in 1u32.., generation in 1u64.., uuid in any::<[u8; 16]>()) {
        let h = FileHeader { lane, ordinal, volume_uuid: VolumeUuid(uuid), generation };
        let mut v = Vec::new();
        h.encode(&mut v).unwrap();
        prop_assert_eq!(v.len(), 4096);
        prop_assert_eq!(FileHeader::decode(&v).unwrap(), h);
    }
}

#[test]
fn the_flushed_watermark_round_trips_and_zero_is_the_v1_reserved_field() {
    let data = vec![7u8; 4096];
    let mk = |flushed: u64| {
        let mut out = Vec::new();
        Record {
            header: RecordHeader {
                kind: RecordKind::Write,
                write_seq: WriteSeq(10),
                epoch: 0,
                lba: 3,
                len: 1,
                gen: 0x1234,
                off: 4096,
                flushed: WriteSeq(flushed),
            },
            data: &data,
        }
        .encode(&mut out)
        .unwrap();
        out
    };
    let zero = mk(0);
    assert!(
        zero[56..64].iter().all(|b| *b == 0),
        "watermark 0 is the old reserved zeros"
    );
    let nine = mk(9);
    assert_eq!(u64::from_le_bytes(nine[56..64].try_into().unwrap()), 9);
    assert_eq!(Record::decode(&nine).unwrap().header.flushed, WriteSeq(9));
    let mut bad = Vec::new();
    let r = Record {
        header: RecordHeader {
            kind: RecordKind::Hole,
            write_seq: WriteSeq(5),
            epoch: 0,
            lba: 0,
            len: 1,
            gen: 1,
            off: 4096,
            flushed: WriteSeq(5),
        },
        data: &[],
    };
    assert!(
        r.encode(&mut bad).is_err(),
        "a record cannot witness its own flush"
    );
}
