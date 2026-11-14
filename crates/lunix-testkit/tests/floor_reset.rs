//! `MemStore::reset_rollback_floor` follows the real store's rules.

use lunix_format::local::RollbackFloor;
use lunix_format::CommitSeq;
use lunix_iface::store::{CommitStore, FloorReset, VolumeDir};
use lunix_iface::ErrorKind;
use lunix_testkit::{MemStore, MemStoreConfig};

fn at(seq: u64, h: [u8; 32]) -> RollbackFloor {
    RollbackFloor {
        max_commit_seq: CommitSeq(seq),
        tip_hash: h,
    }
}

#[test]
fn lowers_verified_and_audited() {
    let s = MemStore::with_config(MemStoreConfig {
        allow_floor_reset: true,
        ..MemStoreConfig::default()
    });
    let mut h = Vec::new();
    for n in 0..3u64 {
        let b = format!("c{n}").into_bytes();
        s.put(CommitSeq(n), &b).expect("put");
        h.push(*blake3::hash(&b).as_bytes());
    }
    s.raise_rollback_floor(&at(2, h[2])).expect("raise");
    assert_eq!(
        s.reset_rollback_floor(&at(1, [0; 32]), FloorReset::TruncateTip)
            .expect_err("hash")
            .kind(),
        ErrorKind::Refused
    );
    assert_eq!(
        s.reset_rollback_floor(&at(2, h[2]), FloorReset::TruncateTip)
            .expect_err("not lower")
            .kind(),
        ErrorKind::Refused
    );
    let old = s
        .reset_rollback_floor(&at(1, h[1]), FloorReset::TruncateTip)
        .expect("lower");
    assert_eq!(old, Some(at(2, h[2])));
    assert_eq!(s.rollback_floor().expect("f"), Some(at(1, h[1])));
    assert_eq!(s.floor_audit().len(), 1);
}

#[test]
fn refused_without_the_option() {
    let s = MemStore::with_config(MemStoreConfig::default());
    assert_eq!(
        s.reset_rollback_floor(&at(0, [0; 32]), FloorReset::OperatorReset)
            .expect_err("refused")
            .kind(),
        ErrorKind::Refused
    );
}
