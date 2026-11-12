//! `MemBlockEngine` driven through the `QueueHandler` trait: the 10 000-request
//! random sequence of the M1.B2 gate, and the handler contract around it.

use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lunix_iface::block::{BlockEngine, IoOp, IoRequest, QueueConfig, QueueId, ResizeHook, Tag};
use lunix_iface::ErrorKind;
use lunix_testkit::strategies::{io_ops, IoShape, IoSpec};
use lunix_testkit::{
    run_request, Applied, CompletionMode, DeviceOracle, MemBlockEngine, MemEngineConfig,
    TestCompleter,
};
use proptest::prelude::*;

const BLOCK: u64 = 4096;
const DEV: u64 = 8 << 20;
const CHUNK: u64 = 16 * BLOCK;
const MAX_IO: u32 = 1 << 20;

fn qcfg(queue: u16, nr: u16) -> QueueConfig {
    QueueConfig {
        queue: QueueId(queue),
        nr_queues: nr,
        depth: 8,
        max_io_buf_bytes: MAX_IO,
    }
}

fn req(io: &IoSpec, tag: u16) -> IoRequest {
    IoRequest {
        tag: Tag(tag),
        op: io.op,
        fua: io.fua,
        offset_bytes: io.offset_bytes,
        len: io.len,
    }
}

fn engine(mode: CompletionMode) -> MemBlockEngine {
    MemBlockEngine::new(MemEngineConfig {
        mode,
        ..MemEngineConfig::new(DEV)
    })
}

/// Runs `ops` one at a time and checks every answer against the oracle.
fn check_sequence(mode: CompletionMode, ops: &[IoSpec]) -> Result<(), TestCaseError> {
    let eng = engine(mode);
    let mut h = eng.queue_handler(&qcfg(0, 1)).expect("handler");
    let mut c = TestCompleter::new(MAX_IO);
    let mut oracle = DeviceOracle::new(DEV);
    for (i, io) in ops.iter().enumerate() {
        let want = oracle.apply(io);
        let (status, data) = run_request(&mut *h, &mut c, req(io, (i % 8) as u16), &io.payload());
        match want {
            Applied::Invalid => {
                prop_assert_eq!(status.errno(), Some(22), "op {} {:?}", i, io);
            }
            Applied::Read(bytes) => {
                prop_assert!(status.is_ok());
                prop_assert_eq!(status.raw(), io.len as i32);
                prop_assert!(data == bytes, "op {} read differs: {:?}", i, io);
            }
            Applied::Done => prop_assert!(status.is_ok(), "op {} {:?}", i, status),
        }
    }
    prop_assert!(eng.snapshot() == oracle.bytes(), "final device differs");
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1))]

    #[test]
    fn ten_thousand_requests_match_the_oracle_immediate(
        ops in io_ops(IoShape::new(DEV, CHUNK).with_invalid(), 10_000)
    ) {
        check_sequence(CompletionMode::Immediate, &ops)?;
    }

    #[test]
    fn two_thousand_requests_match_the_oracle_deferred(
        ops in io_ops(IoShape::new(DEV, CHUNK).with_invalid(), 2_000)
    ) {
        check_sequence(CompletionMode::Deferred, &ops)?;
    }

    #[test]
    fn two_thousand_requests_match_the_oracle_mixed(
        ops in io_ops(IoShape::new(DEV, CHUNK).with_invalid(), 2_000),
        seed in any::<u64>(),
    ) {
        check_sequence(CompletionMode::Mixed(seed), &ops)?;
    }
}

#[test]
fn deferred_requests_in_flight_together_complete_from_poll_in_submission_order() {
    let eng = engine(CompletionMode::Deferred);
    let mut h = eng.queue_handler(&qcfg(0, 1)).unwrap();
    let mut c = TestCompleter::new(MAX_IO);
    let mut oracle = DeviceOracle::new(DEV);
    let ops: Vec<IoSpec> = (0..8u64)
        .map(|i| IoSpec {
            op: if i % 2 == 0 { IoOp::Write } else { IoOp::Read },
            fua: false,
            offset_bytes: (i / 2) * BLOCK,
            len: BLOCK as u32,
            seed: i,
        })
        .collect();
    let mut bufs: Vec<Vec<u8>> = ops
        .iter()
        .map(|io| {
            if io.op == IoOp::Write {
                io.payload()
            } else {
                vec![0; BLOCK as usize]
            }
        })
        .collect();
    for (i, io) in ops.iter().enumerate() {
        h.on_request(req(io, i as u16), &mut bufs[i], &mut c);
    }
    assert!(
        c.take_completed().is_empty(),
        "deferred mode never completes inline"
    );
    assert!(h.wake_fd().as_raw_fd() >= 0);
    h.poll(&mut c);
    let done = c.take_completed();
    assert_eq!(done.len(), 8);
    assert_eq!(
        done.iter().map(|d| d.0 .0).collect::<Vec<_>>(),
        (0..8).collect::<Vec<_>>()
    );
    for (i, io) in ops.iter().enumerate() {
        if let Applied::Read(bytes) = oracle.apply(io) {
            assert_eq!(c.buf_of(Tag(i as u16), bytes.len()), bytes, "read {i}");
        }
    }
    let mut again = Vec::new();
    h.poll(&mut c);
    again.extend(c.take_completed());
    assert!(again.is_empty(), "poll is idempotent when nothing is ready");
}

#[test]
fn the_handler_contract_around_queues_flush_and_errors() {
    let eng = MemBlockEngine::new(MemEngineConfig {
        nr_queues: 2,
        ..MemEngineConfig::new(DEV)
    });
    assert_eq!(eng.size_bytes(), DEV);
    assert_eq!(
        eng.queue_handler(&qcfg(2, 2)).err().map(|e| e.kind()),
        Some(ErrorKind::Invalid)
    );
    assert_eq!(
        eng.queue_handler(&qcfg(0, 3)).err().map(|e| e.kind()),
        Some(ErrorKind::Invalid),
        "queue count must match"
    );
    let mut h0 = eng.queue_handler(&qcfg(0, 2)).unwrap();
    assert_eq!(
        eng.queue_handler(&qcfg(0, 2)).err().map(|e| e.kind()),
        Some(ErrorKind::Invalid),
        "one handler per queue"
    );
    let mut h1 = eng.queue_handler(&qcfg(1, 2)).unwrap();
    let mut c = TestCompleter::new(MAX_IO);

    // A write on one queue is visible to a read on the other.
    let w = IoSpec {
        op: IoOp::Write,
        fua: true,
        offset_bytes: 8 * BLOCK,
        len: 2 * BLOCK as u32,
        seed: 5,
    };
    let (s, _) = run_request(&mut *h0, &mut c, req(&w, 0), &w.payload());
    assert_eq!(s.raw(), 2 * BLOCK as i32);
    let r = IoSpec {
        op: IoOp::Read,
        fua: false,
        ..w
    };
    let (s, data) = run_request(&mut *h1, &mut c, req(&r, 1), &[]);
    assert!(s.is_ok());
    assert_eq!(data, w.payload());

    // FLUSH has length 0; a bad length or offset is EINVAL and changes nothing.
    let f = IoSpec {
        op: IoOp::Flush,
        fua: false,
        offset_bytes: 0,
        len: 0,
        seed: 0,
    };
    assert!(run_request(&mut *h0, &mut c, req(&f, 2), &[]).0.is_ok());
    for (off, len) in [
        (1u64, 4096u32),
        (0, 100),
        (DEV, 4096),
        (DEV - 4096, 8192),
        (0, 0),
    ] {
        let bad = IoSpec {
            op: IoOp::Discard,
            fua: false,
            offset_bytes: off,
            len,
            seed: 0,
        };
        assert_eq!(
            run_request(&mut *h0, &mut c, req(&bad, 3), &[]).0.errno(),
            Some(22)
        );
    }
    let too_big = IoSpec {
        op: IoOp::Read,
        fua: false,
        offset_bytes: 0,
        len: MAX_IO + 4096,
        seed: 0,
    };
    assert_eq!(
        run_request(&mut *h0, &mut c, req(&too_big, 3), &[])
            .0
            .errno(),
        Some(22)
    );
    let (_, still) = run_request(&mut *h0, &mut c, req(&r, 4), &[]);
    assert_eq!(still, w.payload());

    // Discard and write-zeroes read back as zeros.
    for op in [IoOp::Discard, IoOp::WriteZeroes] {
        run_request(&mut *h0, &mut c, req(&w, 5), &w.payload());
        let z = IoSpec {
            op,
            fua: false,
            len: BLOCK as u32,
            ..w
        };
        assert!(run_request(&mut *h0, &mut c, req(&z, 6), &[]).0.is_ok());
        let (_, data) = run_request(&mut *h0, &mut c, req(&r, 7), &[]);
        assert_eq!(&data[..BLOCK as usize], &[0u8; BLOCK as usize][..]);
        assert_eq!(&data[BLOCK as usize..], &w.payload()[BLOCK as usize..]);
    }

    let stats = eng.stats();
    assert_eq!(stats.ops_flush, 1);
    assert_eq!((stats.ops_discard, stats.ops_write_zeroes), (1, 1));
    assert!(stats.bytes_written >= 6 * BLOCK);
    assert_eq!(eng.counters().einval, 6);
    assert_eq!(
        eng.counters().fua_writes,
        3,
        "the first write and two in the loop"
    );
}

#[test]
fn injected_errors_a_read_only_volume_and_quiesce() {
    let eng = engine(CompletionMode::Immediate);
    let mut h = eng.queue_handler(&qcfg(0, 1)).unwrap();
    let mut c = TestCompleter::new(MAX_IO);
    let w = IoSpec {
        op: IoOp::Write,
        fua: false,
        offset_bytes: 0,
        len: BLOCK as u32,
        seed: 1,
    };
    let r = IoSpec {
        op: IoOp::Read,
        ..w
    };
    eng.fail_next(IoOp::Write, 28);
    assert_eq!(
        run_request(&mut *h, &mut c, req(&w, 0), &w.payload())
            .0
            .errno(),
        Some(28)
    );
    assert!(
        run_request(&mut *h, &mut c, req(&w, 0), &w.payload())
            .0
            .is_ok(),
        "one shot"
    );
    eng.set_read_only(true);
    let other = IoSpec { seed: 2, ..w };
    assert_eq!(
        run_request(&mut *h, &mut c, req(&other, 0), &other.payload())
            .0
            .errno(),
        Some(5)
    );
    let (s, data) = run_request(&mut *h, &mut c, req(&r, 1), &[]);
    assert!(s.is_ok(), "reads continue");
    assert_eq!(data, w.payload(), "the refused write changed nothing");
    assert!(!eng.is_quiesced());
    eng.quiesce().unwrap();
    eng.quiesce().unwrap();
    assert!(eng.is_quiesced());
    assert_eq!(eng.counters().quiesces, 2);
}

struct Hook(AtomicU64);

impl ResizeHook for Hook {
    fn resized(&self, new_size_bytes: u64) -> lunix_iface::Result<()> {
        self.0.store(new_size_bytes, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn grow_extends_the_device_calls_the_hook_and_refuses_shrinking() {
    let eng = engine(CompletionMode::Immediate);
    let hook = Arc::new(Hook(AtomicU64::new(0)));
    eng.set_on_resize(hook.clone());
    let mut h = eng.queue_handler(&qcfg(0, 1)).unwrap();
    let mut c = TestCompleter::new(MAX_IO);
    let past = IoSpec {
        op: IoOp::Read,
        fua: false,
        offset_bytes: DEV,
        len: BLOCK as u32,
        seed: 0,
    };
    assert_eq!(
        run_request(&mut *h, &mut c, req(&past, 0), &[]).0.errno(),
        Some(22)
    );
    assert_eq!(
        eng.grow(DEV - BLOCK).unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(eng.grow(DEV + 100).unwrap_err().kind(), ErrorKind::Invalid);
    eng.grow(DEV).unwrap();
    assert_eq!(hook.0.load(Ordering::SeqCst), 0, "no change, no hook");
    eng.grow(2 * DEV).unwrap();
    assert_eq!(hook.0.load(Ordering::SeqCst), 2 * DEV);
    assert_eq!(eng.size_bytes(), 2 * DEV);
    let (s, data) = run_request(&mut *h, &mut c, req(&past, 1), &[]);
    assert!(s.is_ok());
    assert!(data.iter().all(|&b| b == 0));
    assert_eq!(eng.snapshot().len() as u64, 2 * DEV);
}

#[test]
fn a_dropped_handler_frees_its_queue_and_mixed_mode_reorders_completions() {
    let eng = engine(CompletionMode::Mixed(5));
    {
        let _h = eng.queue_handler(&qcfg(0, 1)).unwrap();
        assert!(eng.queue_handler(&qcfg(0, 1)).is_err());
    }
    let mut h = eng
        .queue_handler(&qcfg(0, 1))
        .expect("rebuilt after the drop");
    let mut c = TestCompleter::new(MAX_IO);
    let mut bufs: Vec<Vec<u8>> = (0..8).map(|_| Vec::new()).collect();
    let f = IoSpec {
        op: IoOp::Flush,
        fua: false,
        offset_bytes: 0,
        len: 0,
        seed: 0,
    };
    let (mut reordered, mut survived) = (false, false);
    for round in 0..40u16 {
        let base = (round % 2) * 8;
        for i in 0..8u16 {
            h.on_request(req(&f, base + i), &mut bufs[usize::from(i)], &mut c);
        }
        let mut order = Vec::new();
        order.extend(c.take_completed().into_iter().map(|d| d.0 .0));
        let inline = order.len();
        let mut polls = 0;
        while order.len() < 8 {
            h.poll(&mut c);
            polls += 1;
            order.extend(c.take_completed().into_iter().map(|d| d.0 .0));
            assert!(polls <= 2, "a held request waits one more poll at most");
        }
        survived |= polls == 2;
        reordered |= order.windows(2).any(|w| w[0] > w[1]) || inline > 0;
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (base..base + 8).collect::<Vec<_>>());
    }
    assert!(reordered && survived);
}
