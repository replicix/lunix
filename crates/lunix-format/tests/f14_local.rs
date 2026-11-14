//! Freeze item 14: local-only files.
mod common;

use common::*;
use lunix_format::local::*;
use lunix_format::{CryptoMode, FamilyId, FormatError, VolumeUuid};
use proptest::prelude::*;

fn slot(seq: u64, lanes: usize, stats: usize) -> CheckpointSlot {
    CheckpointSlot {
        slot_seq: seq,
        volume_uuid: VolumeUuid(VOLUME_B_UUID),
        last_commit_seq: 12 + seq,
        id_high: 0x0000_0100_0000_0007,
        lane_tail_ordinals: (0..lanes as u32).map(|i| 1 + i).collect(),
        stats: (0..stats as u64).map(|i| i * 1000 + seq).collect(),
    }
}

fn ckp_bytes(a: &CheckpointSlot, b: &CheckpointSlot) -> Vec<u8> {
    let mut o = Vec::new();
    CheckpointFile::encode_pair(a, b, &mut o).unwrap();
    o
}

fn ckp_rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let [a, c] = CheckpointFile::decode(b)?.slots;
    let mut o = Vec::new();
    CheckpointFile::encode_pair(&a?, &c?, &mut o)?;
    Ok(o)
}

#[test]
fn checkpoint_golden_and_slot_choice() {
    let (a, b) = (slot(4, 4, 5), slot(5, 4, 5));
    let bytes = ckp_bytes(&a, &b);
    assert_eq!(bytes.len(), 8192);
    let g = golden("14-checkpoint.bin", &bytes);
    let f = CheckpointFile::decode(&g).unwrap();
    assert_eq!(f.newest_valid().map(|(i, s)| (i, s.slot_seq)), Some((1, 5)));
    assert_eq!(f.next_slot_to_write(), 0);
    negative_suite("checkpoint", &ckp_rt, &g);

    // Swapped sequence numbers: slot 0 is newest.
    let f = CheckpointFile::decode(&ckp_bytes(&slot(9, 1, 0), &slot(8, 1, 0))).unwrap();
    assert_eq!(f.newest_valid().map(|(i, _)| i), Some(0));
    assert_eq!(f.next_slot_to_write(), 1);

    // Torn second slot: the first is used, and the next write replaces the torn one.
    let mut torn = g.clone();
    torn[4096 + 100] ^= 0xff;
    let f = CheckpointFile::decode(&torn).unwrap();
    assert!(f.slots[1].is_err());
    assert_eq!(f.newest_valid().map(|(i, s)| (i, s.slot_seq)), Some((0, 4)));
    assert_eq!(f.next_slot_to_write(), 1);
    // Torn first slot.
    let mut torn = g.clone();
    torn[7] ^= 1;
    let f = CheckpointFile::decode(&torn).unwrap();
    assert_eq!(f.newest_valid().map(|(i, _)| i), Some(1));
    assert_eq!(f.next_slot_to_write(), 0);
    // Both torn: ignored.
    torn[4096 + 7] ^= 1;
    let f = CheckpointFile::decode(&torn).unwrap();
    assert!(f.newest_valid().is_none());
    assert_eq!(f.next_slot_to_write(), 0);
    // Wrong file length is an error.
    assert!(CheckpointFile::decode(&g[..8191]).is_err());
}

fn fix_crc(b: &mut [u8]) {
    let c = lunix_format::bytes::crc32c(&b[..4092]).to_le_bytes();
    b[4092..4096].copy_from_slice(&c);
}

#[test]
fn checkpoint_slot_rejections() {
    let mut o = Vec::new();
    slot(1, 2, 3).encode(&mut o).unwrap();
    let dec = |b: &[u8]| CheckpointSlot::decode(b);
    assert!(dec(&o).is_ok());
    let patch = |off: usize, v: &[u8]| {
        let mut b = o.clone();
        b[off..off + v.len()].copy_from_slice(v);
        fix_crc(&mut b);
        b
    };
    assert!(dec(&patch(0, b"LUNIXCKX")).is_err(), "magic");
    assert!(dec(&patch(8, &[2, 0])).is_err(), "ver");
    assert!(dec(&patch(10, &[0, 0])).is_err(), "lanes 0");
    assert!(
        dec(&patch(12, &193u16.to_le_bytes())).is_err(),
        "n_stats 193"
    );
    assert!(dec(&patch(14, &[1])).is_err(), "reserved");
    assert!(dec(&patch(16, &0u64.to_le_bytes())).is_err(), "slot_seq 0");
    assert!(dec(&patch(56, &[1])).is_err(), "reserved 8");
    assert!(dec(&patch(64 + 4, &[1])).is_err(), "lane tail reserved");
    assert!(dec(&patch(4000, &[1])).is_err(), "nonzero fill");
    // CRC mismatch
    let mut b = o.clone();
    b[100] ^= 1;
    assert!(dec(&b).is_err(), "crc");
    // encode limits
    assert!(slot(1, 0, 0).encode(&mut Vec::new()).is_err());
    assert!(slot(1, 257, 0).encode(&mut Vec::new()).is_err());
    assert!(slot(1, 1, 193).encode(&mut Vec::new()).is_err());
    assert!(slot(0, 1, 0).encode(&mut Vec::new()).is_err());
    assert!(slot(1, 256, 192).encode(&mut Vec::new()).is_ok());
}

fn device() -> DeviceRecord {
    DeviceRecord {
        nr_queues: 4,
        queue_depth: 128,
        dev_id: 3,
        max_io_buf_bytes: 1 << 20,
        boot_id: std::array::from_fn(|i| 0xc0 + i as u8),
        volume_uuid: VolumeUuid(VOLUME_B_UUID),
        dev_sectors: 2 * 1024 * 1024,
        flags: 0x1a5,
    }
}

fn dev_rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let d = DeviceRecord::decode(b)?;
    let mut o = Vec::new();
    d.encode(&mut o)?;
    Ok(o)
}

fn fix96(b: &mut [u8]) {
    let c = lunix_format::bytes::crc32c(&b[..92]).to_le_bytes();
    b[92..96].copy_from_slice(&c);
}

#[test]
fn device_golden_and_rejections() {
    let mut o = Vec::new();
    device().encode(&mut o).unwrap();
    assert_eq!(o.len(), 96);
    let g = golden("14-device.bin", &o);
    assert_eq!(DeviceRecord::decode(&g).unwrap(), device());
    negative_suite("device", &dev_rt, &g);
    let patch = |off: usize, v: &[u8]| {
        let mut b = g.clone();
        b[off..off + v.len()].copy_from_slice(v);
        fix96(&mut b);
        b
    };
    assert_rejects("magic", &dev_rt, &patch(0, b"LUNIXDEX"));
    assert_rejects("ver", &dev_rt, &patch(8, &[2]));
    assert_rejects("queues 0", &dev_rt, &patch(10, &[0, 0]));
    assert_rejects("queues 257", &dev_rt, &patch(10, &257u16.to_le_bytes()));
    assert_rejects("depth 0", &dev_rt, &patch(12, &[0, 0]));
    assert_rejects("depth 4097", &dev_rt, &patch(12, &4097u16.to_le_bytes()));
    assert_rejects("reserved", &dev_rt, &patch(14, &[1]));
    assert_rejects("io buf", &dev_rt, &patch(20, &262_143u32.to_le_bytes()));
    assert_rejects("reserved 20", &dev_rt, &patch(80, &[1]));
    assert!(dev_rt(&patch(20, &262_144u32.to_le_bytes())).is_ok());
}

fn pins_with(pin: Option<(CryptoMode, [u8; 16])>) -> LocalPins {
    LocalPins {
        volume_uuid: VolumeUuid(VOLUME_B_UUID),
        family_id: FamilyId(VOLUME_B_FAMILY),
        max_keyring_seq: 4,
        max_epoch: 0,
        pin: pin.map(|(mode, fingerprint)| Pin { mode, fingerprint }),
    }
}

fn file_with(pins: LocalPins, floor: Option<RollbackFloor>) -> LocalPinsFile {
    LocalPinsFile { pins, floor }
}

/// Decode and re-encode.
fn pins_rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let d = LocalPinsFile::decode(b)?;
    let mut o = Vec::new();
    d.encode(&mut o)?;
    Ok(o)
}

fn fixn(b: &mut [u8]) {
    let at = b.len() - 4;
    let c = lunix_format::bytes::crc32c(&b[..at]).to_le_bytes();
    b[at..].copy_from_slice(&c);
}

/// Section 23.9, transcribed from the document.
const DOC_PINS_V2: &str = "4c554e4958 50494e 0200 01 01 01 000000 \
    b0b1b2b3b4b546b788b9babbbcbdbebf 505152535455565758595a5b5c5d5e5f \
    0100000000000000 0000000000000000 92a4ec2689f62aab86bc8f278713183e \
    0100000000000000 \
    648d4fc9be6093837921d9a1739fe6ac80a74141 5664f5e2d2feed4defe754de \
    000000000000000000000000 c603742b";

fn doc_floor() -> RollbackFloor {
    RollbackFloor {
        max_commit_seq: lunix_format::CommitSeq(1),
        tip_hash: hex("648d4fc9be6093837921d9a1739fe6ac80a741415664f5e2d2feed4defe754de")
            .try_into()
            .unwrap(),
    }
}

fn doc_pins_b() -> LocalPins {
    let mut p = pins_with(Some((
        CryptoMode::Integrity,
        hex("92a4ec2689f62aab86bc8f278713183e").try_into().unwrap(),
    )));
    p.max_keyring_seq = 1;
    p
}

#[test]
fn pins_v2_matches_section_23_9() {
    let doc = hex(DOC_PINS_V2);
    assert_eq!(doc.len(), 136);
    let f = file_with(doc_pins_b(), Some(doc_floor()));
    let mut o = Vec::new();
    f.encode(&mut o).unwrap();
    assert_eq!(o, doc, "encoder differs from the doc");
    assert_eq!(LocalPinsFile::LEN, 136);
    // the doc's crc32c is over bytes 0..132
    assert_eq!(
        lunix_format::bytes::crc32c(&doc[..132]).to_le_bytes(),
        doc[132..]
    );
    let g = golden("14-pins-v2.bin", &o);
    assert_eq!(g, doc);
    assert_eq!(LocalPinsFile::decode(&g).unwrap(), f);
    negative_suite("14-pins-v2", &pins_rt, &g);
}

#[test]
fn pins_without_a_floor_goldens() {
    let fp: [u8; 16] = hex("92a4ec2689f62aab86bc8f278713183e").try_into().unwrap();
    for (name, p) in [
        ("14-pins.bin", pins_with(Some((CryptoMode::Integrity, fp)))),
        ("14-pins-none.bin", pins_with(None)),
    ] {
        let mut o = Vec::new();
        file_with(p.clone(), None).encode(&mut o).unwrap();
        let g = golden(name, &o);
        assert_eq!(g.len(), 136);
        let d = LocalPinsFile::decode(&g).unwrap();
        assert_eq!(d.pins, p, "{name}");
        assert_eq!(d.floor, None, "{name}");
        assert_eq!(LocalPins::decode(&g).unwrap(), p, "{name}");
        negative_suite(name, &pins_rt, &g);
    }
}

#[test]
fn pins_goldens_and_rejections() {
    let fp: [u8; 16] = hex("92a4ec2689f62aab86bc8f278713183e").try_into().unwrap();
    let mut o = Vec::new();
    file_with(pins_with(Some((CryptoMode::Plain, [0; 16]))), None)
        .encode(&mut o)
        .unwrap();
    assert!(LocalPinsFile::decode(&o).is_ok());
    assert!(file_with(pins_with(Some((CryptoMode::Plain, fp))), None)
        .encode(&mut Vec::new())
        .is_err());
    // LocalPins keeps its shape: it writes the file without a floor and
    // drops the floor it reads.
    let mut w = Vec::new();
    pins_with(None).encode(&mut w).unwrap();
    assert_eq!(w.len(), LocalPins::LEN);
    assert_eq!(LocalPinsFile::decode(&w).unwrap().floor, None);
    let doc = hex(DOC_PINS_V2);
    assert_eq!(LocalPins::decode(&doc).unwrap(), doc_pins_b());

    // Version 2 rejections.
    let g = doc;
    let patch = |off: usize, v: &[u8]| {
        let mut b = g.clone();
        b[off..off + v.len()].copy_from_slice(v);
        fixn(&mut b);
        b
    };
    assert_rejects("present 2", &pins_rt, &patch(10, &[2]));
    assert_rejects("mode 3", &pins_rt, &patch(11, &[3]));
    assert_rejects("floor_present 2", &pins_rt, &patch(12, &[2]));
    assert_rejects("reserved 13", &pins_rt, &patch(13, &[1]));
    assert_rejects("reserved 15", &pins_rt, &patch(15, &[1]));
    assert_rejects("reserved 120", &pins_rt, &patch(120, &[1]));
    assert_rejects("reserved 131", &pins_rt, &patch(131, &[1]));
    assert_rejects("version 3", &pins_rt, &patch(8, &[3]));
    assert_rejects("version 1 in a 136-byte file", &pins_rt, &patch(8, &[1]));
    assert_rejects("magic", &pins_rt, &patch(0, b"LUNIXPIX"));
    let mut bad_crc = g.clone();
    bad_crc[133] ^= 1;
    assert_rejects("crc", &pins_rt, &bad_crc);
    // lengths other than 136
    for n in [0usize, 95, 96, 97, 135, 137, 232] {
        let mut b = g.clone();
        b.resize(n, 0);
        assert_rejects("length", &pins_rt, &b);
    }
    // floor fields without a floor
    let mut no_floor = Vec::new();
    file_with(doc_pins_b(), None).encode(&mut no_floor).unwrap();
    for off in [80usize, 88, 119] {
        let mut b = no_floor.clone();
        b[off] = 1;
        fixn(&mut b);
        assert_rejects("floor fields without a floor", &pins_rt, &b);
    }
    // a floor at commit 0 (a genesis tip) is legal
    let genesis = RollbackFloor {
        max_commit_seq: lunix_format::CommitSeq(0),
        tip_hash: [7; 32],
    };
    let mut o = Vec::new();
    file_with(doc_pins_b(), Some(genesis))
        .encode(&mut o)
        .unwrap();
    assert_eq!(LocalPinsFile::decode(&o).unwrap().floor, Some(genesis));
    // plain pin with a fingerprint, no pin with a mode or fingerprint
    assert_rejects("plain pin with fingerprint", &pins_rt, &patch(11, &[0]));
    let mut b = no_floor.clone();
    b[10] = 0;
    fixn(&mut b);
    assert_rejects("pin_present 0 with a mode", &pins_rt, &b);
    let mut n = Vec::new();
    file_with(pins_with(None), None).encode(&mut n).unwrap();
    let mut b = n.clone();
    b[11] = 1;
    fixn(&mut b);
    assert_rejects("mode without pin", &pins_rt, &b);
    let mut b = n;
    b[64] = 1;
    fixn(&mut b);
    assert_rejects("fingerprint without pin", &pins_rt, &b);
}

// ---------------------------------------------------------------------------
// JSON

fn vj_rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    // Not a byte layout: any accepted input validates; report it unchanged.
    let s = std::str::from_utf8(b).map_err(|_| FormatError::invalid("volume.json", "utf-8"))?;
    VolumeJson::from_json(s)?;
    Ok(b.to_vec())
}

fn vs_rt(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let s = std::str::from_utf8(b).map_err(|_| FormatError::invalid("volumes.json", "utf-8"))?;
    VolumesJson::from_json(s)?;
    Ok(b.to_vec())
}

fn vj() -> VolumeJson {
    VolumeJson::new("vol-1.test", "/var/lib/lunix")
}

#[test]
fn volume_json_golden_and_rejections() {
    let text = vj().to_json_pretty().unwrap();
    golden_text("14-volume.json", &text);
    assert_eq!(VolumeJson::from_json(&text).unwrap(), vj());
    // Canonical output is stable.
    assert_eq!(
        VolumeJson::from_json(&text)
            .unwrap()
            .to_json_pretty()
            .unwrap(),
        text
    );
    mutations_are_safe("volume.json", &vj_rt, text.as_bytes());
    // Every strict prefix is truncated JSON.
    all_prefixes_fail(
        "volume.json",
        &|b| vj_rt(b),
        &text.as_bytes()[..text.len() - 2],
    );

    let bad = |f: &dyn Fn(&mut VolumeJson), what: &str| {
        let mut v = vj();
        f(&mut v);
        assert!(v.validate().is_err(), "{what}");
    };
    bad(&|v| v.version = 2, "version");
    bad(&|v| v.name = String::new(), "empty name");
    bad(&|v| v.name = "a b".into(), "name charset");
    bad(&|v| v.name = "x".repeat(65), "name length");
    bad(
        &|v| v.directory = "relative/dir".into(),
        "relative directory",
    );
    bad(
        &|v| v.space.free_floor = FreeFloor::Percent("101%".into()),
        "percent over 100",
    );
    bad(
        &|v| v.space.free_floor = FreeFloor::Percent("ten".into()),
        "percent text",
    );
    bad(&|v| v.space.full_timeout_ms = Some(0), "timeout 0");
    bad(&|v| v.commit.interval_ms = 999, "interval");
    bad(&|v| v.commit.dirty_bytes = (1 << 20) - 1, "dirty bytes");
    bad(&|v| v.retain_last = 0, "retain_last 0");
    bad(&|v| v.retain_last = 1001, "retain_last 1001");
    bad(&|v| v.read_timeout_ms = 999, "read timeout");
    bad(&|v| v.gc_grace_ms = 29_999, "grace floor (10 s + 2 x 10 s)");
    bad(&|v| v.gc_live_threshold = 0.0, "threshold 0");
    bad(&|v| v.gc_live_threshold = 1.0, "threshold 1");
    bad(&|v| v.gc_live_threshold = f64::NAN, "threshold NaN");
    bad(&|v| v.gc_step_budget_ms = 0, "step budget");
    bad(&|v| v.journal_max = 268_435_455, "journal_max");
    bad(&|v| v.map_dirty_leaves_max = 99, "dirty leaves");
    bad(&|v| v.max_io_buf_bytes = 262_143, "io buf");
    let mut ok = vj();
    ok.gc_grace_ms = 30_000;
    ok.space.free_floor = FreeFloor::Bytes(1 << 30);
    ok.space.full_timeout_ms = Some(5000);
    ok.space.on_full = OnFull::Error;
    assert!(VolumeJson::from_json(&ok.to_json_pretty().unwrap()).is_ok());

    // Unknown keys, top level and nested, are rejected; so is a wrong version.
    assert!(VolumeJson::from_json(&text.replace("\"retain_last\"", "\"retain_lastx\"")).is_err());
    assert!(VolumeJson::from_json(&text.replace("\"interval_ms\"", "\"interval_msx\"")).is_err());
    assert!(VolumeJson::from_json(&text.replace("\"version\": 1", "\"version\": 2")).is_err());
    assert!(
        VolumeJson::from_json(&text.replace("\"verify_reads\": false", "\"verify_reads\": 0"))
            .is_err()
    );
    assert!(VolumeJson::from_json("{}").is_err(), "no name");
    assert!(VolumeJson::from_json(&text[..text.len() / 2]).is_err());
    assert!(VolumeJson::from_json(&" ".repeat(MAX_JSON_LEN + 1)).is_err());
}

#[test]
fn volumes_json_golden_and_rejections() {
    let v = VolumesJson {
        version: 1,
        volumes: vec![
            RegistryEntry {
                name: "alpha".into(),
                uuid: "a0a1a2a3-a4a5-46a7-88a9-aaabacadaeaf".into(),
                directory: "/var/lib/lunix".into(),
            },
            RegistryEntry {
                name: "beta".into(),
                uuid: "b0b1b2b3-b4b5-46b7-88b9-babbbcbdbebf".into(),
                directory: "/data/lunix".into(),
            },
        ],
    };
    let text = v.to_json_pretty().unwrap();
    golden_text("14-volumes.json", &text);
    assert_eq!(VolumesJson::from_json(&text).unwrap(), v);
    mutations_are_safe("volumes.json", &vs_rt, text.as_bytes());

    let bad = |f: &dyn Fn(&mut VolumesJson), what: &str| {
        let mut x = v.clone();
        f(&mut x);
        assert!(x.validate().is_err(), "{what}");
    };
    bad(&|x| x.version = 0, "version");
    bad(&|x| x.volumes[1].name = "alpha".into(), "duplicate name");
    bad(
        &|x| x.volumes[0].uuid = "A0A1A2A3-A4A5-46A7-88A9-AAABACADAEAF".into(),
        "uppercase uuid",
    );
    bad(&|x| x.volumes[0].uuid = "nonsense".into(), "bad uuid");
    bad(
        &|x| x.volumes[0].directory = "rel".into(),
        "relative directory",
    );
    bad(&|x| x.volumes[0].name = "bad name".into(), "name charset");
    assert!(VolumesJson::from_json(&text.replace("\"uuid\"", "\"uuidx\"")).is_err());
    assert!(VolumesJson::from_json(&text[..text.len() - 3]).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn checkpoint_roundtrip(seq in 1u64.., lanes in 1usize..=256, stats in 0usize..=192, tail in any::<u32>(), c in any::<u64>(), id in any::<u64>()) {
        let mut s = slot(seq, lanes, stats);
        s.lane_tail_ordinals[0] = tail;
        s.last_commit_seq = c;
        s.id_high = id;
        let mut o = Vec::new();
        s.encode(&mut o).unwrap();
        prop_assert_eq!(o.len(), 4096);
        prop_assert_eq!(CheckpointSlot::decode(&o).unwrap(), s);
    }

    #[test]
    fn device_roundtrip(q in 1u16..=256, d in 1u16..=4096, id in any::<u32>(), buf in 262_144u32.., boot in any::<[u8;16]>(), u in any::<[u8;16]>(), sec in any::<u64>(), fl in any::<u64>()) {
        let r = DeviceRecord { nr_queues: q, queue_depth: d, dev_id: id, max_io_buf_bytes: buf, boot_id: boot, volume_uuid: VolumeUuid(u), dev_sectors: sec, flags: fl };
        let mut o = Vec::new();
        r.encode(&mut o).unwrap();
        prop_assert_eq!(DeviceRecord::decode(&o).unwrap(), r);
    }

    #[test]
    fn pins_roundtrip(u in any::<[u8;16]>(), f in any::<[u8;16]>(), k in any::<u64>(), e in any::<u64>(), fp in any::<[u8;16]>(), mode in 0u8..4, floor in proptest::option::of((any::<u64>(), any::<[u8;32]>()))) {
        let pin = match mode {
            0 => None,
            1 => Some(Pin { mode: CryptoMode::Plain, fingerprint: [0; 16] }),
            2 => Some(Pin { mode: CryptoMode::Integrity, fingerprint: fp }),
            _ => Some(Pin { mode: CryptoMode::Encrypted, fingerprint: fp }),
        };
        let p = LocalPins { volume_uuid: VolumeUuid(u), family_id: FamilyId(f), max_keyring_seq: k, max_epoch: e, pin };
        let file = LocalPinsFile {
            pins: p.clone(),
            floor: floor.map(|(s, h)| RollbackFloor { max_commit_seq: lunix_format::CommitSeq(s), tip_hash: h }),
        };
        let mut o = Vec::new();
        file.encode(&mut o).unwrap();
        prop_assert_eq!(o.len(), 136);
        prop_assert_eq!(LocalPinsFile::decode(&o).unwrap(), file);
        prop_assert_eq!(LocalPins::decode(&o).unwrap(), p.clone());
    }

    #[test]
    fn volume_json_roundtrip(retain in 1u32..=1000, rt in 1000u64..100_000, extra in 0u64..1000, th_milli in 1u32..1000, verify in any::<bool>(), pct in 0u32..=100) {
        let mut v = vj();
        v.retain_last = retain;
        v.read_timeout_ms = rt;
        v.gc_grace_ms = 10_000 + 2 * rt + extra;
        // serde_json without `float_roundtrip` is exact only for short decimals.
        v.gc_live_threshold = f64::from(th_milli) / 1000.0;
        v.verify_reads = verify;
        v.space.free_floor = FreeFloor::Percent(format!("{pct}%"));
        let t = v.to_json_pretty().unwrap();
        prop_assert_eq!(VolumeJson::from_json(&t).unwrap(), v);
    }
}

#[test]
fn version_is_required_in_both_json_files() {
    let ok = VolumeJson::from_json(&common::golden_text_read("14-volume.json")).expect("golden");
    let text = ok.to_json_pretty().expect("json");
    assert!(VolumeJson::from_json(&text.replace("  \"version\": 1,\n", "")).is_err());
    let reg = common::golden_text_read("14-volumes.json");
    assert!(VolumesJson::from_json(&reg.replace("  \"version\": 1,\n", "")).is_err());
}
