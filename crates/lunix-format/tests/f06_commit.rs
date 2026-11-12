//! Freeze item 6: commits (section 10), envelope forms 0 and 1, genesis (D5).

mod common;

use common::*;
use lunix_format::commit::*;
use lunix_format::envelope::{self, EnvelopeBody};
use lunix_format::purpose::Purpose;
use lunix_format::{
    CommitSeq, Epoch, FamilyId, Form, FormatError, ObjId, ObjectKind, TimeMs, WriteSeq,
};
use proptest::prelude::*;

const E1_512: &str = "10c152fbfaaab0630db8b786eebf559cbd7b38cb964a20465d68e84412d61317";

/// Descriptor B (section 23.8), 120 bytes.
fn descriptor_b() -> Vec<u8> {
    hex(
        "4c554e4958564f4c 0100 01 00 00100000 00400000 0002 10 02 00 00 000000000000
         b0b1b2b3b4b546b788b9babbbcbdbebf 505152535455565758595a5b5c5d5e5f
         92a4ec2689f62aab86bc8f278713183e 00dc0dcda1010000
         4951c7ffd237693e73cd15e17c6916869a540c098bc6fdbe90340ceba80801b5",
    )
}

fn hmac_key() -> [u8; 32] {
    let msg = Purpose::IntegrityHmac
        .message(&FamilyId(VOLUME_B_FAMILY))
        .unwrap();
    purpose_key(&volume_b_ik(), &msg)
}

fn mac_for(seq: u64, unsigned: &[u8], kind: ObjectKind) -> [u8; 32] {
    let aad = envelope::meta_aad(&VOLUME_B_UUID, kind, &format!("commits/{seq:016x}")).unwrap();
    hmac_sha256(&hmac_key(), &envelope::mac_input(&aad, unsigned))
}

/// A form 0 object with a real MAC under volume B's integrity HMAC key.
fn object(c: &Commit) -> Vec<u8> {
    let unsigned = c.encode_object_form0_unsigned().unwrap();
    let mac = mac_for(c.common().commit_seq.0, &unsigned, c.object_kind());
    c.encode_object_form0(&mac).unwrap()
}

fn rt_object(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let d = decode_object(b)?;
    d.commit.encode_object_form0(&d.mac)
}

fn rt_sealed(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    let env = decode_envelope(b, Form::Sealed)?;
    match env.body {
        EnvelopeBody::Sealed {
            nonce,
            ciphertext,
            tag,
        } => envelope::encode_form1(env.kind, &nonce, ciphertext, &tag),
        EnvelopeBody::Mac { .. } => unreachable!(),
    }
}

fn common_hdr(kind: CommitKind, seq: u64) -> CommonHeader {
    CommonHeader {
        kind,
        commit_seq: CommitSeq(seq),
        epoch: Epoch(0),
        parent_hash: [0x5a; 32],
        created_at: TimeMs(CREATED_AT.wrapping_add(1000u64.wrapping_mul(seq))),
    }
}

fn id(raw: u64) -> ObjId {
    ObjId::from_raw(raw).unwrap()
}

// ---------------------------------------------------------------------------
// Worked examples (section 23.6)

fn genesis_hex() -> Vec<u8> {
    hex("4c584d31 01 01 00 00 a8000000
         01 01 0000 00000000
         0000000000000000 0000000000000000 0000000000000000
         6a6d952bbfcf690941f1c5659a312fd78d0faf46c82ecbfbdb1859ae9e51b8f5
         00dc0dcda1010000
         0000000000000000 0000004000000000 0000000000000000
         10c152fbfaaab0630db8b786eebf559cbd7b38cb964a20465d68e84412d61317
         0000000000000000 00000000 000000 00
         0000000000000000 00000000 00000000 00000000 00000000
         70776d1e8991acc35de81763bb936ddf0fc5d483fb50c57c906f86aa535c516e")
}

fn commit1_hex() -> Vec<u8> {
    hex("4c584d31 01 01 00 00 b8000000
         01 01 0100 00000000
         0100000000000000 0000000000000000 0000000000000000
         338e717684fbe16a5d22c7618a3d893a25d6808689df893eb6af53d942bb03c2
         30510ecda1010000
         0200000000000000 0000004000000000 0000000000000000
         69a6249dad0e6c194b6400c39e38e8f97a0c39cb37b005bda3a9b791f516e6ca
         0200000000000000 71000000 476000 00
         0000000000000000 01000000 01000000 00000000 00000000
         0200000000000000 0100000000000000
         4e3dc3b9db3ed5edee1f2bf7414bd1b5b70eb08bec33b9f5d53e00377e84ca00")
}

fn genesis_commit() -> Commit {
    let h = blake3_256(&descriptor_b());
    let e1: [u8; 32] = hex(E1_512).try_into().unwrap();
    Commit::Full(Full::genesis(h, 1 << 30, e1, TimeMs(CREATED_AT)))
}

fn commit1() -> Commit {
    let mut c = Full::genesis(
        blake3_256(&genesis_hex()),
        1 << 30,
        hex("69a6249dad0e6c194b6400c39e38e8f97a0c39cb37b005bda3a9b791f516e6ca")
            .try_into()
            .unwrap(),
        TimeMs(CREATED_AT + 30_000),
    );
    c.common.commit_seq = CommitSeq(1);
    c.cut_seq = WriteSeq(2);
    c.map_root = MapRoot {
        hash: c.map_root.hash,
        pack: Some(id(2)),
        off: 113,
        len: 24_647,
    };
    c.map_packs = vec![id(2)];
    c.new_segments = vec![id(1)];
    Commit::Full(c)
}

#[test]
fn a0_hmac_key_matches_the_doc() {
    assert_eq!(
        to_hex(&hmac_key()),
        "aa13505a19bd5e783cbc31582031cb918c1dfaa9aa6b94b27057acb06306f4df"
    );
}

#[test]
fn a0_genesis() {
    let c = genesis_commit();
    let mac: [u8; 32] = genesis_hex()[180..].try_into().unwrap();
    let bytes = c.encode_object_form0(&mac).unwrap();
    assert_eq!(bytes, genesis_hex(), "encoder differs from the doc");
    assert_eq!(bytes.len(), 212);
    // The doc's MAC is the HMAC the doc says it is.
    assert_eq!(mac, mac_for(0, &bytes[..180], ObjectKind::Commit));
    let g = golden("a0-commit-full-genesis.bin", &bytes);
    negative_suite("genesis", &rt_object, &g);
    let d = decode_object(&g).unwrap();
    assert_eq!(d.commit, c);
    let Commit::Full(f) = &d.commit else { panic!() };
    check_genesis(&f.common, &blake3_256(&descriptor_b())).unwrap();
    assert!(check_genesis(&f.common, &[0; 32]).is_err());
}

#[test]
fn a0_commit_1() {
    let c = commit1();
    let mac: [u8; 32] = commit1_hex()[196..].try_into().unwrap();
    let bytes = c.encode_object_form0(&mac).unwrap();
    assert_eq!(bytes, commit1_hex(), "encoder differs from the doc");
    assert_eq!(bytes.len(), 228);
    assert_eq!(mac, mac_for(1, &bytes[..196], ObjectKind::Commit));
    let g = golden("a0-commit-full-1.bin", &bytes);
    negative_suite("commit 1", &rt_object, &g);
    // chain rule against the genesis object
    let genesis_bytes = genesis_hex();
    let gen = decode_object(&genesis_bytes).unwrap();
    let c1 = decode_object(&g).unwrap();
    check_child(
        gen.commit.common(),
        &blake3_256(&genesis_hex()),
        c1.commit.common(),
    )
    .unwrap();
    assert!(check_child(gen.commit.common(), &[1; 32], c1.commit.common()).is_err());
    assert!(check_child(c1.commit.common(), &blake3_256(&g), gen.commit.common()).is_err());
}

// ---------------------------------------------------------------------------
// Other kinds and the sealed form

fn sample_full() -> Commit {
    let mut c = Full::genesis([7; 32], 1 << 30, [8; 32], TimeMs(CREATED_AT));
    c.common = common_hdr(CommitKind::Full, 5);
    c.common.epoch = Epoch(0);
    c.chain_floor = CommitSeq(4);
    c.cut_seq = WriteSeq(900);
    c.map_root = MapRoot {
        hash: [9; 32],
        pack: Some(id(0x0000_0100_0000_0005)),
        off: 4,
        len: 24_647,
    };
    c.heat = Some(CommitSeq(3));
    c.map_packs = vec![id(3), id(9)];
    c.new_segments = vec![id(4), id(5), id(0x0000_0100_0000_0001)];
    c.index_packs = vec![id(2)];
    Commit::Full(c)
}

fn sample_delta() -> Commit {
    let mut records = Vec::new();
    for i in 0..96u8 {
        records.push(i.wrapping_mul(7));
    }
    Commit::Delta(Delta {
        common: common_hdr(CommitKind::Delta, 6),
        prev_cut_seq: WriteSeq(900),
        cut_seq: WriteSeq(950),
        size: 1 << 30,
        records_codec: RecordsCodec::Raw,
        records_ulen: records.len() as u32,
        records,
        refs: vec![
            DeltaRef {
                kind: RefKind::Segment,
                id: 6,
                hash: [0x11; 32],
            },
            DeltaRef {
                kind: RefKind::Segment,
                id: 7,
                hash: [0x22; 32],
            },
        ],
    })
}

fn sample_epoch(authority: Option<&str>) -> Commit {
    let mut h = common_hdr(CommitKind::Epoch, 8);
    h.epoch = Epoch(2);
    Commit::Epoch(EpochCommit {
        common: h,
        authority: authority.map(str::to_owned),
    })
}

fn backend(id: &str, role: BackendRole, state: BackendState) -> Backend {
    Backend {
        id: id.into(),
        url: format!("s3://bucket-{id}/lunix"),
        role,
        state,
        since_seq: CommitSeq(3),
    }
}

fn sample_config() -> Commit {
    Commit::Config(ConfigCommit {
        common: common_hdr(CommitKind::Config, 9),
        config_version: 2,
        detached: false,
        authority: "s3-main".into(),
        policy_ver: 1,
        backends: vec![
            backend("s3-main", BackendRole::Authority, BackendState::InSync),
            backend("s3-mirror", BackendRole::Mirror, BackendState::CatchingUp),
        ],
        policy: b"{\"retain\":2}".to_vec(),
    })
}

fn sample_config_detached() -> Commit {
    Commit::Config(ConfigCommit {
        common: common_hdr(CommitKind::Config, 10),
        config_version: 3,
        detached: true,
        authority: String::new(),
        policy_ver: 0,
        backends: vec![],
        policy: vec![],
    })
}

fn sample_transfer() -> Commit {
    let mut h = common_hdr(CommitKind::Transfer, 11);
    h.epoch = Epoch(3);
    Commit::Transfer(TransferCommit {
        common: h,
        config_version: 4,
        reason: TransferReason::Steal,
        to: "s3-mirror".into(),
    })
}

fn check_golden(name: &str, c: &Commit) {
    let bytes = object(c);
    let g = golden(name, &bytes);
    negative_suite(name, &rt_object, &g);
    assert_eq!(&decode_object(&g).unwrap().commit, c);
}

#[test]
fn goldens_of_every_kind() {
    check_golden("06-commit-delta.bin", &sample_delta());
    check_golden("06-commit-epoch.bin", &sample_epoch(None));
    check_golden(
        "06-commit-epoch-authority.bin",
        &sample_epoch(Some("s3-main")),
    );
    check_golden("06-commit-config.bin", &sample_config());
    check_golden("06-commit-config-detached.bin", &sample_config_detached());
    check_golden("06-commit-transfer.bin", &sample_transfer());
}

#[test]
fn delta_uses_envelope_kind_3_and_others_kind_1() {
    assert_eq!(sample_delta().object_kind(), ObjectKind::Delta);
    assert_eq!(object(&sample_delta())[5], 3);
    for c in [
        sample_full(),
        sample_epoch(None),
        sample_config(),
        sample_transfer(),
    ] {
        assert_eq!(object(&c)[5], 1);
    }
}

#[test]
fn sealed_form() {
    let c = sample_full();
    let mut body = Vec::new();
    c.encode_body(&mut body).unwrap();
    // A stand-in for the AEAD: the format crate only frames the parts.
    let cipher: Vec<u8> = body.iter().map(|b| b ^ 0xa5).collect();
    let nonce: [u8; 24] = std::array::from_fn(|i| 0xc0 + i as u8);
    let tag: [u8; 16] = std::array::from_fn(|i| 0x30 + i as u8);
    let obj = c.encode_object_form1(&nonce, &cipher, &tag).unwrap();
    assert_eq!(obj.len(), body.len() + 52);
    let g = golden("06-commit-full-sealed.bin", &obj);
    negative_suite("sealed", &rt_sealed, &g);
    let env = decode_envelope(&g, Form::Sealed).unwrap();
    let EnvelopeBody::Sealed { ciphertext, .. } = env.body else {
        panic!()
    };
    let opened: Vec<u8> = ciphertext.iter().map(|b| b ^ 0xa5).collect();
    assert_eq!(Commit::decode_body(env.kind, &opened).unwrap(), c);
    // A form 0 reader refuses a sealed object and vice versa.
    assert!(decode_object(&g).is_err());
    assert!(decode_envelope(&object(&c), Form::Sealed).is_err());
}

#[test]
fn envelope_kind_must_match_commit_kind() {
    let mut body = Vec::new();
    sample_delta().encode_body(&mut body).unwrap();
    assert!(Commit::decode_body(ObjectKind::Commit, &body).is_err());
    assert!(Commit::decode_body(ObjectKind::Delta, &body).is_ok());
    let mut full = Vec::new();
    sample_full().encode_body(&mut full).unwrap();
    assert!(Commit::decode_body(ObjectKind::Delta, &full).is_err());
    assert!(Commit::decode_body(ObjectKind::Lease, &full).is_err());
    // A hand-built form 0 envelope with the wrong kind.
    let wrong = envelope::encode_form0(ObjectKind::Delta, &full, &[0; 32]).unwrap();
    assert!(decode_object(&wrong).is_err());
}

#[test]
fn delta_records_zstd() {
    let raw: Vec<u8> = (0..4000u32).map(|i| (i % 13) as u8).collect();
    let z = zstd::bulk::compress(&raw, 3).unwrap();
    let Commit::Delta(mut d) = sample_delta() else {
        panic!()
    };
    d.records_codec = RecordsCodec::Zstd;
    d.records_ulen = raw.len() as u32;
    d.records = z;
    let mut b = Vec::new();
    d.encode(&mut b).unwrap();
    let back = Delta::decode(&b).unwrap();
    assert_eq!(back, d);
    assert_eq!(back.records_decoded().unwrap(), raw);
    // Lying about the decoded length is caught when the region is opened.
    let mut lie = d.clone();
    lie.records_ulen = 10;
    assert!(lie.records_decoded().is_err());
    lie.records_ulen = 5000;
    assert!(lie.records_decoded().is_err());
}

// ---------------------------------------------------------------------------
// Rejections

fn body_of(c: &Commit) -> Vec<u8> {
    let mut b = Vec::new();
    c.encode_body(&mut b).unwrap();
    b
}

fn patched(mut b: Vec<u8>, edits: &[(usize, &[u8])]) -> Vec<u8> {
    for (off, bytes) in edits {
        b[*off..*off + bytes.len()].copy_from_slice(bytes);
    }
    b
}

fn dec(kind: ObjectKind, b: &[u8]) -> Result<Commit, FormatError> {
    Commit::decode_body(kind, b)
}

#[test]
fn common_header_rejections() {
    let full = body_of(&sample_full());
    let k = ObjectKind::Commit;
    assert!(dec(k, &full).is_ok());
    assert!(
        dec(k, &patched(full.clone(), &[(0, &[2])])).is_err(),
        "body_ver"
    );
    assert!(
        dec(k, &patched(full.clone(), &[(1, &[0])])).is_err(),
        "kind 0"
    );
    assert!(
        dec(k, &patched(full.clone(), &[(1, &[6])])).is_err(),
        "kind 6"
    );
    assert!(
        dec(k, &patched(full.clone(), &[(2, &[0x07])])).is_err(),
        "flag bit 2"
    );
    assert!(
        dec(k, &patched(full.clone(), &[(3, &[0x01])])).is_err(),
        "flag bit 8"
    );
    assert!(
        dec(k, &patched(full.clone(), &[(2, &[0x02])])).is_err(),
        "has_parent clear at seq 5"
    );
    assert!(
        dec(k, &patched(full.clone(), &[(4, &[1])])).is_err(),
        "reserved"
    );
    assert!(
        dec(k, &patched(full.clone(), &[(24, &[9])])).is_err(),
        "parent_seq"
    );
    // has_heat on a non-FULL kind
    let ep = body_of(&sample_epoch(None));
    assert!(dec(k, &patched(ep.clone(), &[(2, &[0x03])])).is_err());
    // genesis with has_parent set
    let g = body_of(&genesis_commit());
    assert!(dec(k, &patched(g.clone(), &[(2, &[0x01])])).is_err());
    assert!(
        dec(k, &patched(g, &[(24, &[1])])).is_err(),
        "parent_seq at genesis"
    );
    // created_at is never validated
    assert!(dec(k, &patched(full, &[(64, &[0; 8])])).is_ok());
}

#[test]
fn full_rejections() {
    let k = ObjectKind::Commit;
    let f = body_of(&sample_full());
    let ok = |b: Vec<u8>| dec(k, &b).is_ok();
    assert!(ok(f.clone()));
    assert!(
        !ok(patched(f.clone(), &[(80, &100u64.to_le_bytes())])),
        "size < 4096"
    );
    assert!(
        !ok(patched(f.clone(), &[(80, &4097u64.to_le_bytes())])),
        "size not a multiple"
    );
    assert!(
        !ok(patched(
            f.clone(),
            &[(80, &((1u64 << 56) + 4096).to_le_bytes())]
        )),
        "size too big"
    );
    assert!(
        ok(patched(f.clone(), &[(80, &(1u64 << 56).to_le_bytes())])),
        "size 2^56"
    );
    assert!(
        !ok(patched(f.clone(), &[(88, &6u64.to_le_bytes())])),
        "chain_floor > seq"
    );
    assert!(!ok(patched(f.clone(), &[(143, &[1])])), "lineage_idx");
    assert!(
        !ok(patched(f.clone(), &[(128, &0u64.to_le_bytes())])),
        "empty root with a location"
    );
    assert!(
        !ok(patched(f.clone(), &[(140, &[0, 0, 0])])),
        "len 0 with a pack"
    );
    assert!(
        !ok(patched(
            f.clone(),
            &[(128, &[0; 8]), (136, &[0; 4]), (140, &[1, 0, 0])]
        )),
        "len without a pack"
    );
    assert!(
        !ok(patched(f.clone(), &[(140, &[0x6d, 0x00, 0x01])])),
        "len 65645"
    );
    assert!(
        ok(patched(f.clone(), &[(140, &[0x6c, 0x00, 0x01])])),
        "len 65644"
    );
    assert!(
        !ok(patched(f.clone(), &[(152, &1025u32.to_le_bytes())])),
        "n_map_packs"
    );
    assert!(
        !ok(patched(f.clone(), &[(156, &262_145u32.to_le_bytes())])),
        "n_new_segments"
    );
    assert!(
        !ok(patched(f.clone(), &[(160, &4097u32.to_le_bytes())])),
        "n_index_packs"
    );
    assert!(
        !ok(patched(f.clone(), &[(152, &u32::MAX.to_le_bytes())])),
        "count over the input"
    );
    assert!(!ok(patched(f.clone(), &[(164, &[1])])), "reserved");
    // heat flag and heat_seq must agree
    assert!(
        !ok(patched(f.clone(), &[(2, &[0x01])])),
        "heat_seq without has_heat"
    );
    // lists: unsorted, duplicate, zero
    let at = 168;
    assert!(
        !ok(patched(
            f.clone(),
            &[(at, &9u64.to_le_bytes()), (at + 8, &3u64.to_le_bytes())]
        )),
        "unsorted"
    );
    assert!(
        !ok(patched(f.clone(), &[(at + 8, &3u64.to_le_bytes())])),
        "duplicate"
    );
    assert!(
        !ok(patched(f.clone(), &[(at, &0u64.to_le_bytes())])),
        "zero id"
    );
    // trailing byte
    let mut t = f;
    t.push(0);
    assert!(!ok(t));
}

#[test]
fn full_encode_validates() {
    let Commit::Full(good) = sample_full() else {
        panic!()
    };
    let enc = |f: &Full| f.encode(&mut Vec::new());
    assert!(enc(&good).is_ok());
    let mut f = good.clone();
    f.size = 5000;
    assert!(enc(&f).is_err());
    let mut f = good.clone();
    f.chain_floor = CommitSeq(6);
    assert!(enc(&f).is_err());
    let mut f = good.clone();
    f.map_packs = vec![id(9), id(3)];
    assert!(enc(&f).is_err());
    let mut f = good.clone();
    f.map_root.off = 1;
    f.map_root.pack = None;
    assert!(enc(&f).is_err());
    let mut f = good.clone();
    f.common.kind = CommitKind::Delta;
    assert!(enc(&f).is_err());
    let mut out = vec![1, 2, 3];
    let mut f = good;
    f.size = 1;
    let _ = f.encode(&mut out);
    assert_eq!(
        out,
        vec![1, 2, 3],
        "a failed encode leaves the buffer as it was"
    );
}

#[test]
fn delta_rejections() {
    let k = ObjectKind::Delta;
    let d = body_of(&sample_delta());
    let ok = |b: Vec<u8>| dec(k, &b).is_ok();
    assert!(ok(d.clone()));
    assert!(
        !ok(patched(d.clone(), &[(80, &900u64.to_le_bytes())])),
        "cut == prev"
    );
    assert!(
        !ok(patched(d.clone(), &[(88, &1u64.to_le_bytes())])),
        "size"
    );
    assert!(
        !ok(patched(d.clone(), &[(100, &95u32.to_le_bytes())])),
        "raw ulen != len"
    );
    assert!(!ok(patched(d.clone(), &[(106, &[2])])), "codec 2");
    assert!(!ok(patched(d.clone(), &[(107, &[1])])), "reserved");
    assert!(
        !ok(patched(d.clone(), &[(104, &4097u16.to_le_bytes())])),
        "n_refs"
    );
    assert!(
        !ok(patched(d.clone(), &[(104, &200u16.to_le_bytes())])),
        "refs beyond input"
    );
    assert!(!ok(patched(d.clone(), &[(112, &[0])])), "ref kind 0");
    assert!(!ok(patched(d.clone(), &[(112, &[3])])), "ref kind 3");
    assert!(!ok(patched(d.clone(), &[(113, &[1])])), "ref reserved");
    assert!(
        !ok(patched(d.clone(), &[(96, &(65u32 << 20).to_le_bytes())])),
        "records over 64 MiB"
    );
    assert!(
        !ok(patched(d.clone(), &[(96, &200u32.to_le_bytes())])),
        "records beyond input"
    );
    let mut t = d;
    t.push(0);
    assert!(!ok(t));
}

#[test]
fn delta_encode_validates() {
    let Commit::Delta(good) = sample_delta() else {
        panic!()
    };
    let enc = |d: &Delta| d.encode(&mut Vec::new());
    assert!(enc(&good).is_ok());
    let mut d = good.clone();
    d.cut_seq = d.prev_cut_seq;
    assert!(enc(&d).is_err());
    let mut d = good.clone();
    d.records_ulen += 1;
    assert!(enc(&d).is_err());
    let mut d = good.clone();
    d.records_codec = RecordsCodec::Zstd;
    d.records.clear();
    d.records_ulen = 0;
    assert!(enc(&d).is_err());
    let mut d = good;
    d.refs = vec![
        DeltaRef {
            kind: RefKind::Journal,
            id: 1,
            hash: [0; 32]
        };
        4097
    ];
    assert!(enc(&d).is_err());
}

#[test]
fn epoch_rejections() {
    let k = ObjectKind::Commit;
    let e = body_of(&sample_epoch(Some("s3-main")));
    let ok = |b: Vec<u8>| dec(k, &b).is_ok();
    assert!(ok(e.clone()));
    assert!(
        !ok(patched(e.clone(), &[(72, &65u16.to_le_bytes())])),
        "authority_len 65"
    );
    assert!(
        !ok(patched(e.clone(), &[(72, &6u16.to_le_bytes())])),
        "length mismatch leaves a byte"
    );
    assert!(!ok(patched(e.clone(), &[(74, &[1])])), "reserved");
    assert!(!ok(patched(e.clone(), &[(80, b"S")])), "uppercase");
    assert!(!ok(patched(e.clone(), &[(80, &[0xff])])), "not utf-8");
    let mut c = Vec::new();
    let Commit::Epoch(mut x) = sample_epoch(Some("x")) else {
        panic!()
    };
    x.authority = Some(String::new());
    assert!(x.encode(&mut c).is_err());
    x.authority = Some("A".into());
    assert!(x.encode(&mut c).is_err());
    x.authority = Some("a".repeat(65));
    assert!(x.encode(&mut c).is_err());
}

#[test]
fn config_rejections() {
    let k = ObjectKind::Commit;
    let c = body_of(&sample_config());
    let ok = |b: Vec<u8>| dec(k, &b).is_ok();
    assert!(ok(c.clone()));
    assert!(
        !ok(patched(c.clone(), &[(72, &0u64.to_le_bytes())])),
        "config_version 0"
    );
    assert!(
        !ok(patched(c.clone(), &[(80, &65u16.to_le_bytes())])),
        "n_backends 65"
    );
    assert!(!ok(patched(c.clone(), &[(82, &[2])])), "detached 2");
    assert!(!ok(patched(c.clone(), &[(83, &[1])])), "reserved 83");
    assert!(!ok(patched(c.clone(), &[(92, &[1])])), "reserved 92");
    assert!(
        !ok(patched(c.clone(), &[(86, &[0, 0])])),
        "policy_ver 0 with a policy"
    );
    assert!(
        !ok(patched(c.clone(), &[(88, &70_000u32.to_le_bytes())])),
        "policy_len"
    );
    // authority names the mirror
    assert!(
        !ok(patched(c.clone(), &[(96, b"s3-mirror")])),
        "authority/length mismatch"
    );
    // role byte of backend 0 -> mirror: no authority backend
    let b0 = 96 + 7;
    assert!(
        !ok(patched(c.clone(), &[(b0 + 4, &[2])])),
        "no authority backend"
    );
    assert!(!ok(patched(c.clone(), &[(b0 + 4, &[3])])), "role 3");
    assert!(!ok(patched(c.clone(), &[(b0 + 5, &[0])])), "state 0");
    assert!(!ok(patched(c.clone(), &[(b0 + 5, &[6])])), "state 6");
    assert!(
        !ok(patched(c.clone(), &[(b0 + 6, &[1])])),
        "backend reserved"
    );
    assert!(
        !ok(patched(c.clone(), &[(b0, &0u16.to_le_bytes())])),
        "id_len 0"
    );
    assert!(
        !ok(patched(c.clone(), &[(b0 + 2, &0u16.to_le_bytes())])),
        "url_len 0"
    );
    assert!(
        !ok(patched(c.clone(), &[(b0 + 2, &2049u16.to_le_bytes())])),
        "url_len 2049"
    );
    let mut t = c;
    t.push(0);
    assert!(!ok(t));
}

#[test]
fn config_encode_validates() {
    let Commit::Config(good) = sample_config() else {
        panic!()
    };
    let enc = |c: &ConfigCommit| c.encode(&mut Vec::new());
    assert!(enc(&good).is_ok());
    let mut c = good.clone();
    c.backends.reverse();
    assert!(enc(&c).is_err(), "unsorted");
    let mut c = good.clone();
    c.backends[1].id = "s3-main".into();
    assert!(enc(&c).is_err(), "duplicate id");
    let mut c = good.clone();
    c.backends[1].role = BackendRole::Authority;
    assert!(enc(&c).is_err(), "two authorities");
    let mut c = good.clone();
    c.authority = "s3-mirror".into();
    assert!(enc(&c).is_err(), "authority names a mirror");
    let mut c = good.clone();
    c.authority.clear();
    assert!(enc(&c).is_err(), "empty authority on an attached volume");
    let mut c = good.clone();
    c.policy_ver = 0;
    assert!(enc(&c).is_err());
    let mut c = good.clone();
    c.policy = vec![0; 65_537];
    assert!(enc(&c).is_err());
    let mut c = good.clone();
    c.backends[0].url = String::new();
    assert!(enc(&c).is_err());
    let mut c = good.clone();
    c.backends[0].url = "x".repeat(2049);
    assert!(enc(&c).is_err());
    let mut c = good.clone();
    c.backends[0].url = "a\0b".into();
    assert!(enc(&c).is_err());
    let mut c = good.clone();
    c.config_version = 0;
    assert!(enc(&c).is_err());
    // detached with a backend list and an authority is fine; detached with an
    // empty authority and backends is not.
    let mut c = good.clone();
    c.detached = true;
    assert!(enc(&c).is_ok());
    c.authority.clear();
    assert!(enc(&c).is_err());
    let mut c = good;
    c.detached = true;
    for b in &mut c.backends {
        b.role = BackendRole::Mirror;
    }
    assert!(
        enc(&c).is_err(),
        "authority must still name an authority backend"
    );
    let mut c = ConfigCommit {
        common: common_hdr(CommitKind::Config, 3),
        config_version: 1,
        detached: false,
        authority: String::new(),
        policy_ver: 0,
        backends: vec![],
        policy: vec![],
    };
    assert!(enc(&c).is_err(), "attached volume with no backends");
    c.detached = true;
    assert!(enc(&c).is_ok());
}

#[test]
fn transfer_rejections() {
    let k = ObjectKind::Commit;
    let t = body_of(&sample_transfer());
    let ok = |b: Vec<u8>| dec(k, &b).is_ok();
    assert!(ok(t.clone()));
    assert!(
        !ok(patched(t.clone(), &[(72, &0u64.to_le_bytes())])),
        "config_version 0"
    );
    assert!(!ok(patched(t.clone(), &[(80, &[0])])), "reason 0");
    assert!(!ok(patched(t.clone(), &[(80, &[3])])), "reason 3");
    assert!(!ok(patched(t.clone(), &[(81, &[1])])), "reserved 81");
    assert!(!ok(patched(t.clone(), &[(84, &[1])])), "reserved 84");
    assert!(
        !ok(patched(t.clone(), &[(82, &0u16.to_le_bytes())])),
        "to_len 0"
    );
    assert!(
        !ok(patched(t.clone(), &[(82, &65u16.to_le_bytes())])),
        "to_len 65"
    );
    assert!(!ok(patched(t.clone(), &[(88, b"S")])), "uppercase");
}

#[test]
fn wrong_body_type_is_refused() {
    let full = body_of(&sample_full());
    assert!(Delta::decode(&full).is_err());
    assert!(EpochCommit::decode(&full).is_err());
    assert!(ConfigCommit::decode(&full).is_err());
    assert!(TransferCommit::decode(&full).is_err());
    assert!(Commit::decode_body(ObjectKind::Commit, &[]).is_err());
    assert!(Commit::decode_body(ObjectKind::Commit, &[1]).is_err());
}

#[test]
fn chain_rules() {
    let a = common_hdr(CommitKind::Full, 4);
    let mut b = common_hdr(CommitKind::Epoch, 5);
    let h = [3u8; 32];
    b.parent_hash = h;
    assert!(check_child(&a, &h, &b).is_ok());
    assert!(check_child(&a, &[4; 32], &b).is_err(), "parent hash");
    b.commit_seq = CommitSeq(6);
    assert!(check_child(&a, &h, &b).is_err(), "gap");
    b.commit_seq = CommitSeq(5);
    let mut a2 = a;
    a2.epoch = Epoch(2);
    assert!(check_child(&a2, &h, &b).is_err(), "epoch decreased");
    b.epoch = Epoch(2);
    assert!(check_child(&a2, &h, &b).is_ok());
    // created_at going backwards is not a reader error
    b.created_at = TimeMs(0);
    assert!(check_child(&a2, &h, &b).is_ok());
    let mut top = common_hdr(CommitKind::Full, u64::MAX);
    top.commit_seq = CommitSeq(u64::MAX);
    assert!(
        check_child(&top, &h, &b).is_err(),
        "overflow is an error, not a panic"
    );
}

#[test]
fn genesis_is_a_valid_full() {
    let Commit::Full(g) = genesis_commit() else {
        panic!()
    };
    assert_eq!(g.common.parent_seq(), None);
    assert_eq!(g.cut_seq, WriteSeq(0));
    assert_eq!(g.chain_floor, CommitSeq(0));
    let b = body_of(&Commit::Full(g.clone()));
    assert_eq!(b.len(), 168);
    assert_eq!(Full::decode(&b).unwrap(), g);
}

// ---------------------------------------------------------------------------
// Property tests

fn arb_common(kind: CommitKind) -> impl Strategy<Value = CommonHeader> {
    (0..u64::MAX, any::<u64>(), any::<[u8; 32]>(), any::<u64>()).prop_map(
        move |(seq, ep, ph, ca)| CommonHeader {
            kind,
            commit_seq: CommitSeq(seq),
            epoch: Epoch(ep),
            parent_hash: ph,
            created_at: TimeMs(ca),
        },
    )
}

fn arb_objid() -> impl Strategy<Value = ObjId> {
    (0..(1u64 << 24), 1..=ObjId::MAX_COUNTER).prop_map(|(e, c)| ObjId::new(e, c).unwrap())
}

fn arb_ids(max: usize) -> impl Strategy<Value = Vec<ObjId>> {
    prop::collection::btree_set(arb_objid(), 0..max).prop_map(|s| s.into_iter().collect())
}

fn arb_full() -> impl Strategy<Value = Full> {
    let root = (
        any::<[u8; 32]>(),
        prop::option::of((arb_objid(), any::<u32>(), 1..=MAX_NODE_EXTENT)),
    )
        .prop_map(|(hash, loc)| match loc {
            Some((p, off, len)) => MapRoot {
                hash,
                pack: Some(p),
                off,
                len,
            },
            None => MapRoot::empty(hash),
        });
    (
        arb_common(CommitKind::Full),
        any::<u64>(),
        1..=(1u64 << 44),
        any::<u64>(),
        root,
        prop::option::of(any::<u64>()),
        (arb_ids(6), arb_ids(6), arb_ids(6)),
    )
        .prop_map(
            |(common, cut, blocks, floor_x, map_root, heat, (mp, ns, ip))| Full {
                chain_floor: CommitSeq(floor_x % (common.commit_seq.0 + 1)),
                common,
                cut_seq: WriteSeq(cut),
                size: blocks * 4096,
                map_root,
                heat: heat.map(CommitSeq),
                map_packs: mp,
                new_segments: ns,
                index_packs: ip,
            },
        )
}

fn arb_delta() -> impl Strategy<Value = Delta> {
    let r = (
        prop::sample::select(vec![RefKind::Segment, RefKind::Journal]),
        any::<u64>(),
        any::<[u8; 32]>(),
    )
        .prop_map(|(kind, id, hash)| DeltaRef { kind, id, hash });
    (
        arb_common(CommitKind::Delta),
        0..u64::MAX / 2,
        1..1000u64,
        1..=(1u64 << 44),
        prop::collection::vec(any::<u8>(), 0..200),
        prop::collection::vec(r, 0..6),
    )
        .prop_map(|(common, prev, d, blocks, records, refs)| Delta {
            common,
            prev_cut_seq: WriteSeq(prev),
            cut_seq: WriteSeq(prev + d),
            size: blocks * 4096,
            records_codec: RecordsCodec::Raw,
            records_ulen: records.len() as u32,
            records,
            refs,
        })
}

fn arb_bid() -> impl Strategy<Value = String> {
    "[a-z0-9._-]{1,16}"
}

fn arb_epoch() -> impl Strategy<Value = EpochCommit> {
    (arb_common(CommitKind::Epoch), prop::option::of(arb_bid()))
        .prop_map(|(common, authority)| EpochCommit { common, authority })
}

fn arb_config() -> impl Strategy<Value = ConfigCommit> {
    let be = prop::collection::btree_set(arb_bid(), 0..5);
    (
        arb_common(CommitKind::Config),
        1..u64::MAX,
        any::<bool>(),
        be,
        any::<prop::sample::Index>(),
        prop::collection::vec(
            (
                "[a-z]{1,4}://[a-z0-9./]{1,30}",
                any::<bool>(),
                1..=5u8,
                any::<u64>(),
            ),
            5,
        ),
        prop::collection::vec(any::<u8>(), 0..32),
        1..u16::MAX,
    )
        .prop_map(|(common, ver, detached, ids, pick, extra, policy, pv)| {
            let ids: Vec<String> = ids.into_iter().collect();
            let auth_idx = if ids.is_empty() {
                None
            } else {
                Some(pick.index(ids.len()))
            };
            let backends: Vec<Backend> = ids
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let (url, _, st, since) = extra[i].clone();
                    Backend {
                        id: id.clone(),
                        url,
                        role: if Some(i) == auth_idx {
                            BackendRole::Authority
                        } else {
                            BackendRole::Mirror
                        },
                        state: [
                            BackendState::Joining,
                            BackendState::CatchingUp,
                            BackendState::InSync,
                            BackendState::Draining,
                            BackendState::Removed,
                        ][usize::from(st) - 1],
                        since_seq: CommitSeq(since),
                    }
                })
                .collect();
            let (authority, detached) = match auth_idx {
                Some(i) => (ids[i].clone(), detached),
                None => (String::new(), true),
            };
            ConfigCommit {
                common,
                config_version: ver,
                detached,
                authority,
                policy_ver: if policy.is_empty() { 0 } else { pv },
                backends,
                policy,
            }
        })
}

fn arb_transfer() -> impl Strategy<Value = TransferCommit> {
    (
        arb_common(CommitKind::Transfer),
        1..u64::MAX,
        any::<bool>(),
        arb_bid(),
    )
        .prop_map(|(common, config_version, steal, to)| TransferCommit {
            common,
            config_version,
            reason: if steal {
                TransferReason::Steal
            } else {
                TransferReason::Planned
            },
            to,
        })
}

fn cfg() -> ProptestConfig {
    ProptestConfig::with_cases(CASES)
}

fn roundtrip(c: Commit) {
    let mut b = Vec::new();
    c.encode_body(&mut b).unwrap();
    let back = Commit::decode_body(c.object_kind(), &b).unwrap();
    assert_eq!(back, c);
    let mut again = Vec::new();
    back.encode_body(&mut again).unwrap();
    assert_eq!(again, b);
    let mac = [0x42; 32];
    let obj = c.encode_object_form0(&mac).unwrap();
    let d = decode_object(&obj).unwrap();
    assert_eq!(d.commit, c);
    assert_eq!(d.mac, mac);
}

proptest! {
    #![proptest_config(cfg())]
    #[test]
    fn full_roundtrip(f in arb_full()) { roundtrip(Commit::Full(f)); }
    #[test]
    fn delta_roundtrip(d in arb_delta()) { roundtrip(Commit::Delta(d)); }
    #[test]
    fn epoch_roundtrip(e in arb_epoch()) { roundtrip(Commit::Epoch(e)); }
    #[test]
    fn config_roundtrip(c in arb_config()) { roundtrip(Commit::Config(c)); }
    #[test]
    fn transfer_roundtrip(t in arb_transfer()) { roundtrip(Commit::Transfer(t)); }
    #[test]
    fn arbitrary_bytes_never_panic(b in prop::collection::vec(any::<u8>(), 0..300)) {
        for k in [ObjectKind::Commit, ObjectKind::Delta] {
            let _ = Commit::decode_body(k, &b);
        }
        let _ = decode_object(&b);
    }
}
