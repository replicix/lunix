//! Self-consistency of the device oracle: 1 000 sequences of 100 requests
//! against a sparse block model with a different representation, and the
//! operation log against replay.

use std::collections::BTreeMap;

use lunix_iface::block::IoOp;
use lunix_testkit::strategies::{io_ops, IoShape, IoSpec};
use lunix_testkit::{Applied, DeviceOracle, LogOp, OracleLog};
use proptest::prelude::*;

const BLOCK: u64 = 4096;
const DEV: u64 = 64 * BLOCK * 4; // 1 MiB
const CHUNK: u64 = 16 * BLOCK;

/// One map entry per written block; absent means zeros.
#[derive(Default)]
struct Sparse(BTreeMap<u64, Vec<u8>>);

impl Sparse {
    fn write(&mut self, off: u64, data: &[u8]) {
        for (i, c) in data.chunks(BLOCK as usize).enumerate() {
            self.0.insert(off / BLOCK + i as u64, c.to_vec());
        }
    }
    fn zero(&mut self, off: u64, len: u64) {
        for b in off / BLOCK..(off + len) / BLOCK {
            self.0.remove(&b);
        }
    }
    fn read(&self, off: u64, len: u64) -> Vec<u8> {
        let mut out = Vec::new();
        for b in off / BLOCK..(off + len) / BLOCK {
            out.extend_from_slice(self.0.get(&b).map_or(&[0u8; BLOCK as usize][..], |v| v));
        }
        out
    }
}

fn shape() -> IoShape {
    IoShape::new(DEV, CHUNK).with_invalid()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn the_oracle_matches_a_sparse_block_model(ops in io_ops(shape(), 100)) {
        let mut oracle = DeviceOracle::new(DEV);
        let mut model = Sparse::default();
        for io in &ops {
            let applied = oracle.apply(io);
            if !io.is_valid(DEV) {
                prop_assert_eq!(applied, Applied::Invalid);
                continue;
            }
            let len = u64::from(io.len);
            match io.op {
                IoOp::Read => {
                    prop_assert_eq!(applied, Applied::Read(model.read(io.offset_bytes, len)));
                }
                IoOp::Write => {
                    model.write(io.offset_bytes, &io.payload());
                    prop_assert_eq!(applied, Applied::Done);
                }
                IoOp::Discard | IoOp::WriteZeroes => {
                    model.zero(io.offset_bytes, len);
                    prop_assert_eq!(applied, Applied::Done);
                }
                IoOp::Flush => prop_assert_eq!(applied, Applied::Done),
            }
        }
        prop_assert_eq!(oracle.bytes(), &model.read(0, DEV)[..]);
    }

    #[test]
    fn the_log_replays_and_finds_prefixes(ops in io_ops(shape(), 100), pick in any::<prop::sample::Index>()) {
        let mut log = OracleLog::new(DEV);
        let mut live = DeviceOracle::new(DEV);
        let mut states_at = vec![live.bytes().to_vec()];
        for io in &ops {
            live.apply(io);
            if log.record(io, DEV) {
                states_at.push(live.bytes().to_vec());
            }
        }
        prop_assert_eq!(log.len(), states_at.len() - 1);
        let full = log.state_after(log.len());
        prop_assert_eq!(full.bytes(), live.bytes());
        let n = pick.index(log.len() + 1);
        let state = log.state_after(n);
        prop_assert_eq!(state.bytes(), &states_at[n][..]);
        // The first match is the earliest equal state, never later than n.
        let first = log.first_match(state.bytes(), 0).expect("a prefix matches");
        prop_assert!(first <= n);
        prop_assert_eq!(&states_at[first], &states_at[n]);
        // With a floor above n the same bytes can only match if they recur.
        let later = log.first_match(state.bytes(), n);
        prop_assert_eq!(later, Some(n));
        // Corrupting one block of a prefix state matches no prefix, unless the
        // corrupted bytes happen to be another prefix.
        let mut bad = state.bytes().to_vec();
        bad[(DEV as usize) / 2] ^= 0x80;
        if let Some(m) = log.first_match(&bad, 0) {
            prop_assert_eq!(&states_at[m], &bad);
        }
    }
}

#[test]
fn first_match_respects_the_floor_and_sizes() {
    let mut log = OracleLog::new(4 * BLOCK);
    let w = |seed: u64, block: u64| IoSpec {
        op: IoOp::Write,
        fua: false,
        offset_bytes: block * BLOCK,
        len: BLOCK as u32,
        seed,
    };
    assert!(log.record(&w(1, 0), 4 * BLOCK));
    assert!(log.record(&w(2, 1), 4 * BLOCK));
    log.record_grow(8 * BLOCK);
    assert!(log.record(&w(3, 7), 8 * BLOCK));
    assert!(
        !log.record(&w(4, 9), 8 * BLOCK),
        "invalid requests are not logged"
    );
    let flush = IoSpec {
        op: IoOp::Flush,
        fua: false,
        offset_bytes: 0,
        len: 0,
        seed: 0,
    };
    assert!(!log.record(&flush, 8 * BLOCK), "flushes are not logged");
    assert_eq!(log.len(), 4);
    assert!(matches!(
        (LogOp::Grow(1), LogOp::Grow(1)),
        (LogOp::Grow(_), LogOp::Grow(_))
    ));
    for n in 0..=4 {
        let s = log.state_after(n);
        assert_eq!(log.first_match(s.bytes(), 0), Some(n), "n = {n}");
        assert_eq!(log.first_match(s.bytes(), n), Some(n));
    }
    // The device before the grow is smaller than any later state.
    let before = log.state_after(2);
    assert_eq!(
        log.first_match(before.bytes(), 3),
        None,
        "floor above the match"
    );
    assert_eq!(before.size_bytes(), 4 * BLOCK);
}
