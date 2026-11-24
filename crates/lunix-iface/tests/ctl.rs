//! Control protocol: frames round-trip, oversized and truncated frames are
//! rejected, messages are strict, secrets stay redacted.

use std::io::Cursor;

use lunix_format::local::{FreeFloor, OnFull, SpaceSettings};
use lunix_iface::ctl::*;
use lunix_iface::gc::{
    GcPassSummary, GcReclaimStats, GcState, GcStats, GcStatusSnapshot, RefsState, RefsStats,
};
use lunix_iface::stats::EngineStats;
use proptest::prelude::*;
use serde_json::{json, Value};

fn name() -> impl Strategy<Value = String> {
    "[A-Za-z0-9][A-Za-z0-9._-]{0,63}"
}

fn path() -> impl Strategy<Value = String> {
    "/[a-z0-9/._-]{0,100}"
}

fn secret() -> impl Strategy<Value = Secret> {
    proptest::collection::vec(any::<u8>(), 0..64).prop_map(Secret::new)
}

fn mode() -> impl Strategy<Value = VolumeMode> {
    prop_oneof![
        Just(VolumeMode::Encrypted),
        Just(VolumeMode::Integrity),
        Just(VolumeMode::Plain)
    ]
}

fn compress() -> impl Strategy<Value = Compress> {
    prop_oneof![
        Just(Compress::Auto),
        Just(Compress::Lz4),
        (-7i8..=22).prop_map(|level| Compress::Zstd { level }),
        Just(Compress::None),
    ]
}

fn credential() -> impl Strategy<Value = Credential> {
    (
        proptest::option::of(secret()),
        proptest::option::of(path()),
        proptest::option::of("[0-9a-f]{32}"),
        proptest::option::of(mode()),
    )
        .prop_map(|(passphrase, key_file, fingerprint, mode)| Credential {
            passphrase,
            key_file,
            fingerprint,
            mode,
        })
}

fn create() -> impl Strategy<Value = VolumeCreate> {
    (
        (
            name(),
            path(),
            any::<u64>(),
            any::<u32>(),
            mode(),
            compress(),
        ),
        (
            any::<u8>(),
            proptest::option::of(any::<u16>()),
            proptest::option::of(path()),
            proptest::option::of(secret()),
            proptest::option::of(any::<u32>()),
        ),
    )
        .prop_map(
            |(
                (name, dir, size_bytes, chunk_size, mode, compress),
                (id_len, map_fanout, key_file, passphrase, store_max_percent),
            )| {
                VolumeCreate {
                    name,
                    dir,
                    size_bytes,
                    chunk_size,
                    mode,
                    compress,
                    id_len,
                    map_fanout,
                    key_file,
                    passphrase,
                    store_max_percent,
                }
            },
        )
}

fn attach() -> impl Strategy<Value = Attach> {
    (
        name(),
        proptest::option::of(any::<u16>()),
        proptest::option::of(any::<u16>()),
        proptest::option::of("[0-9,-]{1,30}"),
        proptest::option::of(any::<u32>()),
        proptest::option::of(credential()),
        proptest::option::of(prop_oneof![
            Just(JournalDamagePolicy::Wipe),
            Just(JournalDamagePolicy::Refuse)
        ]),
        any::<bool>(),
    )
        .prop_map(
            |(
                name,
                queues,
                depth,
                cpus,
                max_io_buf_bytes,
                credential,
                on_journal_damage,
                allow_unverifiable_fork,
            )| Attach {
                name,
                queues,
                depth,
                cpus,
                max_io_buf_bytes,
                credential,
                on_journal_damage,
                allow_unverifiable_fork,
            },
        )
}

fn space_req() -> impl Strategy<Value = SpaceRequest> {
    (
        name(),
        proptest::option::of(prop_oneof![
            any::<u64>().prop_map(FreeFloor::Bytes),
            (0u8..=100).prop_map(|p| FreeFloor::Percent(format!("{p}%"))),
        ]),
        proptest::option::of(prop_oneof![Just(OnFull::Wait), Just(OnFull::Error)]),
        proptest::option::of(any::<u64>()),
        proptest::option::of(any::<u64>()),
    )
        .prop_map(
            |(name, free_floor, on_full, full_timeout_ms, cache_max)| SpaceRequest {
                name,
                free_floor,
                on_full,
                full_timeout_ms,
                cache_max,
            },
        )
}

fn request() -> impl Strategy<Value = Request> {
    prop_oneof![
        Just(Request::Ping),
        create().prop_map(Request::VolumeCreate),
        Just(Request::VolumeList),
        name().prop_map(|name| Request::VolumeStatus(NameParam { name })),
        attach().prop_map(Request::Attach),
        name().prop_map(|name| Request::Detach(NameParam { name })),
        (name(), any::<u64>())
            .prop_map(|(name, size_bytes)| Request::Resize(Resize { name, size_bytes })),
        space_req().prop_map(Request::Space),
        Just(Request::QuiesceAll),
        (name(), any::<bool>())
            .prop_map(|(name, dry_run)| Request::Gc(GcRequest { name, dry_run })),
        name().prop_map(|name| Request::GcStatus(NameParam { name })),
    ]
}

fn ratio() -> impl Strategy<Value = Option<f64>> {
    proptest::option::of((0u32..100_000).prop_map(|n| f64::from(n) / 8.0))
}

fn stats() -> impl Strategy<Value = EngineStats> {
    proptest::collection::vec(any::<u64>(), EngineStats::SLOTS)
        .prop_map(|v| EngineStats::from_slots(&v).expect("exact slot count"))
}

fn figures() -> impl Strategy<Value = SpaceFigures> {
    proptest::collection::vec(any::<u64>(), 11).prop_map(|v| SpaceFigures {
        used: v[0],
        limit: v[1],
        fs_free: v[2],
        journal: v[3],
        segments: v[4],
        cache: v[5],
        map: v[6],
        index: v[7],
        commits: v[8],
        headroom_held: v[9],
        waiters: v[10],
    })
}

fn throttle() -> impl Strategy<Value = ThrottleStatus> {
    use lunix_iface::space::ThrottleState as S;
    (
        proptest::collection::vec(any::<u64>(), 20),
        prop_oneof![
            Just(S::Open),
            Just(S::Slowdown),
            Just(S::Stop),
            Just(S::Full)
        ],
        prop_oneof![
            Just(GcPressure::Normal),
            Just(GcPressure::Raised),
            Just(GcPressure::Urgent)
        ],
        proptest::option::of(any::<u64>()),
        ratio(),
    )
        .prop_map(
            |(v, state, gc_pressure, rate, amplification)| ThrottleStatus {
                state,
                state_since_ms: v[0],
                state_for_ms: v[1],
                headroom: v[2],
                soft: v[3],
                resume: v[4],
                limit: v[5],
                rate,
                paced_waiters: v[6],
                space_waiters: v[7],
                delayed_writes: v[8],
                delayed_ms: v[9],
                delay_max_ms: v[10],
                delay_capped: v[11],
                bypassed_fua: v[12],
                holes_admitted: v[13],
                enospc: v[14],
                stops: v[15],
                fulls: v[16],
                throttled_ms: v[17],
                ingest_rate: v[18],
                reclaim_rate: v[19],
                gc_pressure,
                amplification,
            },
        )
}

fn summary() -> impl Strategy<Value = GcPassSummary> {
    (proptest::collection::vec(any::<u64>(), 9), any::<bool>()).prop_map(|(v, dry_run)| {
        GcPassSummary {
            pass: v[0],
            dry_run,
            segments_condemned: v[1],
            segments_deleted: v[2],
            packs_deleted: v[3],
            runs_deleted: v[4],
            commits_deleted: v[5],
            segments_compacted: v[6],
            bytes_freed: v[7],
            bytes_moved: v[8],
        }
    })
}

fn gc_state() -> impl Strategy<Value = Option<GcState>> {
    proptest::option::of(prop_oneof![
        Just(GcState::Idle),
        Just(GcState::Marking),
        Just(GcState::Condemning),
        Just(GcState::Grace),
        Just(GcState::Rechecking),
        Just(GcState::Deleting),
        Just(GcState::Compacting),
    ])
}

fn gc_status() -> impl Strategy<Value = GcStatusSnapshot> {
    (
        gc_state(),
        any::<u64>(),
        any::<u64>(),
        proptest::option::of(summary()),
        proptest::collection::vec(any::<u64>(), 8),
        proptest::option::of(any::<u64>()),
    )
        .prop_map(
            |(state, condemned_pending, moved_pending, last_pass, s, last_finished_ms)| {
                GcStatusSnapshot {
                    state,
                    condemned_pending,
                    moved_pending,
                    last_pass,
                    stats: GcStats {
                        passes: s[0],
                        live_bytes: s[1],
                        dead_bytes: s[2],
                        bytes_freed: s[3],
                        segments_compacted: s[4],
                        compaction_read: s[5],
                        compaction_written: s[6],
                    },
                    last_finished_ms,
                    requests: s[7],
                    reclaim: Box::new(GcReclaimStats {
                        fast_passes: s[0],
                        regular_passes: s[1],
                        fast_segments_deleted: s[2],
                        fast_bytes_freed: s[3],
                        recheck_lag_ms_sum: s[4],
                        recheck_lag_ms_max: s[5],
                        recheck_lag_count: s[6],
                        fast_lag_ms_sum: s[7],
                        fast_lag_ms_max: s[0],
                        fast_lag_count: s[1],
                        proposals_offered: s[2],
                        proposal_condemned: s[3],
                        proposal_condemned_bytes: s[4],
                        proposal_rejected_pins: s[5],
                        proposal_rejected_early: s[6],
                        proposal_rejected_kept: s[7],
                        proposal_rejected_room: s[0],
                        propose_errors: s[1],
                        proposals_deferred: s[2],
                        proposal_uncondemned: s[3],
                        proposal_deleted: s[4],
                        proposal_deleted_bytes: s[5],
                        mark_condemned_segments: s[6],
                    }),
                    refs: Some(Box::new(RefsStats {
                        state: RefsState::Drifted,
                        segments: s[0],
                        zero_waiting: s[1],
                        ready: s[2],
                        drift_segments: s[3],
                        rebases: s[4],
                        report_mismatches: s[5],
                        shadow_would_propose: s[6],
                        shadow_offered_idle: s[7],
                        shadow_offered_busy: s[0],
                        shadow_confirmed: s[1],
                        shadow_confirmed_lead: s[2],
                        shadow_lead_ms_sum: s[3],
                        shadow_lead_ms_max: s[4],
                        shadow_confirmed_no_lead: s[5],
                        shadow_unconfirmed_live: s[6],
                        shadow_unconfirmed_kept: s[7],
                        shadow_unconfirmed_named: s[2],
                        shadow_unconfirmed_listed: s[0],
                        shadow_revived: s[1],
                        shadow_early_by_mark: s[2],
                        shadow_missed: s[3],
                        shadow_confirmed_bytes: s[5],
                        shadow_early_bytes: s[6],
                        shadow_missed_bytes: s[7],
                        shadow_lead_byte_ms: s[0],
                        first_mark_condemn_ms: Some(s[4]),
                        condemn_lag_ms_sum: s[5],
                        condemn_lag_ms_max: s[6],
                        condemn_lag_count: s[7],
                        idle_wait_ms_sum: s[0],
                        idle_wait_ms_max: s[1],
                        idle_wait_count: s[2],
                        idle_waiting: s[3],
                        first_ready_ms: Some(s[4]),
                        first_proposal_ms: Some(s[5]),
                        first_proposal_delete_ms: Some(s[6]),
                        proposals_taken: s[7],
                        pin_blocked: s[0],
                    })),
                }
            },
        )
}

fn status() -> impl Strategy<Value = VolumeStatusBody> {
    (
        (
            name(),
            "[0-9a-f-]{36}",
            prop_oneof![
                Just(VolumeState::Detached),
                Just(VolumeState::Attached),
                Just(VolumeState::Recovering),
                Just(VolumeState::ReadOnly)
            ],
            any::<u64>(),
            any::<u32>(),
            mode(),
        ),
        (
            proptest::option::of(path()),
            stats(),
            figures(),
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            proptest::option::of(any::<u64>()),
            ratio(),
            ratio(),
            throttle(),
        ),
    )
        .prop_map(
            |(
                (name, uuid, state, size_bytes, chunk_size, mode),
                (
                    device,
                    stats,
                    space,
                    journal_used,
                    journal_capacity,
                    last_commit_seq,
                    last_commit_ms,
                    dedup_ratio,
                    compression_ratio,
                    throttle,
                ),
            )| {
                VolumeStatusBody {
                    name,
                    uuid,
                    state,
                    size_bytes,
                    chunk_size,
                    mode,
                    device,
                    stats,
                    space,
                    throttle,
                    journal_used,
                    journal_capacity,
                    journal_marks_written: journal_used % 7,
                    journal_marks_skipped: journal_capacity % 3,
                    write_path: WritePathStatus {
                        ring_waiters: journal_used % 5,
                        ring_parked_writes: journal_used % 19,
                        ring_wait_max_since_attach_ms: journal_capacity % 11,
                        ring_wait_max_60s_ms: journal_capacity % 7,
                        freed_files_by_lane: vec![journal_used % 3, journal_capacity % 4],
                        write_latency_log2_us: (0..WRITE_LATENCY_BUCKETS as u64)
                            .map(|b| (journal_used + b) % 23)
                            .collect(),
                        ring_pace_rate: journal_capacity % 29,
                        ring_paced_writes: journal_used % 31,
                        ring_paced_ms: journal_used % 37,
                        ring_pace_max_ms: journal_capacity % 41,
                        ring_pace_capped: journal_used % 43,
                        ring_pace_bypassed_fua: journal_capacity % 47,
                        store_factor_milli: journal_used % 67,
                        store_factor_biased_milli: journal_capacity % 89,
                        store_journal_uncut: journal_capacity % 71,
                        store_h_proj: journal_used % 73,
                        store_paced_above_soft: journal_used % 2 == 1,
                        store_above_soft_ms: journal_used % 97,
                        store_delayed_writes: journal_capacity % 101,
                        store_delayed_ms: journal_used % 103,
                        store_pred_milli: journal_capacity % 79,
                        store_real_milli: journal_used % 83,
                        window_resident_bytes: journal_used,
                        window_hits: journal_capacity % 13,
                        window_misses: journal_used % 17,
                        last_cut_bytes: journal_capacity,
                        last_cut_ms: last_commit_seq,
                        cuts: journal_used % 53,
                        cut_bytes_sum: journal_used % 59,
                        cut_us_sum: journal_used % 67,
                        seals: journal_used % 19,
                        seal_in_flight_max: journal_capacity % 23,
                        seal_busy_ms: journal_used % 53,
                        seal_wait_ms: journal_capacity % 59,
                        seal_stall_ms: journal_used % 61,
                    },
                    locked_bytes: journal_used.wrapping_mul(3),
                    last_commit_seq,
                    last_commit_ms,
                    dedup_ratio,
                    compression_ratio,
                    rollback_floor: (last_commit_seq % 2 == 1).then(|| RollbackFloorView {
                        max_commit_seq: last_commit_seq,
                        tip_hash: "ab".repeat(32),
                    }),
                }
            },
        )
}

fn code() -> impl Strategy<Value = ErrorCode> {
    prop_oneof![
        Just(ErrorCode::Busy),
        Just(ErrorCode::NotFound),
        Just(ErrorCode::Downgrade),
        Just(ErrorCode::WrongKmk),
        Just(ErrorCode::NoSpace),
        Just(ErrorCode::Invalid),
        Just(ErrorCode::Io),
        Just(ErrorCode::Internal),
        Just(ErrorCode::Rollback),
        Just(ErrorCode::Fork),
        Just(ErrorCode::ForkUnverifiable),
        Just(ErrorCode::JournalDamage),
    ]
}

fn response() -> impl Strategy<Value = Response> {
    prop_oneof![
        (any::<u32>(), name()).prop_map(|(version, daemon)| Response::Pong { version, daemon }),
        (
            name(),
            "[0-9a-f-]{36}",
            path(),
            mode(),
            proptest::option::of(path())
        )
            .prop_map(|(name, uuid, dir, mode, key_file)| Response::VolumeCreated(
                VolumeCreated {
                    name,
                    uuid,
                    dir,
                    mode,
                    key_file
                }
            )),
        proptest::collection::vec(
            (
                name(),
                "[0-9a-f-]{36}",
                path(),
                any::<u64>(),
                mode(),
                proptest::option::of(path())
            )
                .prop_map(|(name, uuid, dir, size_bytes, mode, device)| {
                    VolumeSummary {
                        name,
                        uuid,
                        dir,
                        size_bytes,
                        mode,
                        device,
                    }
                }),
            0..4
        )
        .prop_map(|volumes| Response::VolumeList { volumes }),
        status().prop_map(|s| Response::VolumeStatus(Box::new(s))),
        (
            path(),
            any::<u32>(),
            any::<u16>(),
            any::<u16>(),
            any::<bool>(),
            any::<bool>()
        )
            .prop_map(|(device, dev_id, queues, depth, recovered, first_attach)| {
                Response::Attached(Attached {
                    device,
                    dev_id,
                    queues,
                    depth,
                    recovered,
                    first_attach,
                })
            }),
        Just(Response::Detached),
        any::<u64>().prop_map(|size_bytes| Response::Resized { size_bytes }),
        (
            figures(),
            any::<u64>(),
            proptest::option::of(any::<u64>()),
            proptest::option::of(any::<u32>()),
        )
            .prop_map(|(figures, headroom, cache_max, cache_max_percent)| {
                Response::Space(SpaceBody {
                    settings: SpaceSettings {
                        headroom,
                        cache_max,
                        cache_max_percent,
                        ..SpaceSettings::default()
                    },
                    figures,
                })
            }),
        any::<u32>().prop_map(|devices| Response::Quiesced { devices }),
        summary().prop_map(Response::Gc),
        gc_status().prop_map(Response::GcStatus),
    ]
}

fn roundtrip_frame<T: serde::Serialize + serde::de::DeserializeOwned>(v: &T) -> T {
    let payload = to_payload(v).expect("serialise");
    let framed = encode_frame(&payload, MAX_FRAME).expect("frame");
    let mut cur = Cursor::new(framed);
    let got = read_frame(&mut cur, MAX_FRAME).expect("read frame");
    assert_eq!(&*got, &*payload);
    from_payload(&got).expect("parse")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn request_frames_round_trip(id in any::<u64>(), call in request()) {
        let f = RequestFrame::new(id, call);
        prop_assert_eq!(roundtrip_frame(&f), f);
    }

    #[test]
    fn ok_response_frames_round_trip(id in any::<u64>(), body in response()) {
        let f = ResponseFrame::ok(id, body);
        prop_assert_eq!(roundtrip_frame(&f), f);
    }

    #[test]
    fn error_response_frames_round_trip(id in any::<u64>(), c in code(), msg in ".{0,600}") {
        let f = ResponseFrame::err(id, CtlError::new(c, msg));
        prop_assert!(matches!(&f.result, Outcome::Err(e) if e.message.chars().count() <= CtlError::MAX_MESSAGE));
        prop_assert_eq!(roundtrip_frame(&f), f);
    }

    #[test]
    fn any_prefix_of_a_frame_is_truncated(call in request(), cut in 0usize..10_000) {
        let payload = to_payload(&RequestFrame::new(1, call)).expect("serialise");
        let framed = encode_frame(&payload, MAX_FRAME).expect("frame");
        let cut = cut % framed.len();
        let mut cur = Cursor::new(&framed[..cut]);
        match read_frame(&mut cur, MAX_FRAME) {
            Err(FrameError::Eof) => prop_assert_eq!(cut, 0),
            Err(FrameError::Truncated { need, have }) => prop_assert!(have < need),
            other => prop_assert!(false, "unexpected {:?}", other.map(|b| b.len())),
        }
    }

    #[test]
    fn declared_length_above_the_limit_is_rejected_before_reading(len in (MAX_FRAME + 1)..=u32::MAX) {
        let mut cur = Cursor::new(len.to_be_bytes().to_vec());
        assert!(matches!(read_frame(&mut cur, MAX_FRAME), Err(FrameError::TooLarge { .. })));
        assert!(matches!(frame_len(len.to_be_bytes(), MAX_FRAME), Err(FrameError::TooLarge { .. })));
    }

    #[test]
    fn garbage_payloads_never_panic(bytes in proptest::collection::vec(any::<u8>(), 1..300)) {
        let _ = from_payload::<RequestFrame>(&bytes);
        let _ = from_payload::<ResponseFrame>(&bytes);
    }
}

#[test]
fn boundary_lengths() {
    assert!(matches!(
        frame_len([0; 4], MAX_FRAME),
        Err(FrameError::Empty)
    ));
    assert_eq!(
        frame_len(MAX_FRAME.to_be_bytes(), MAX_FRAME).expect("max"),
        MAX_FRAME as usize
    );
    assert!(encode_frame(&vec![b'x'; MAX_FRAME as usize], MAX_FRAME).is_ok());
    assert!(matches!(
        encode_frame(&vec![b'x'; MAX_FRAME as usize + 1], MAX_FRAME),
        Err(FrameError::TooLarge { .. })
    ));
    assert!(matches!(
        encode_frame(b"", MAX_FRAME),
        Err(FrameError::Empty)
    ));
}

#[test]
fn frames_stream_back_to_back() {
    let mut buf = Vec::new();
    for id in 1..=3 {
        let p = to_payload(&RequestFrame::new(id, Request::Ping)).expect("p");
        write_frame(&mut buf, &p, MAX_FRAME).expect("w");
    }
    let mut cur = Cursor::new(buf);
    for id in 1..=3 {
        let f: RequestFrame =
            from_payload(&read_frame(&mut cur, MAX_FRAME).expect("r")).expect("parse");
        assert_eq!(f.id, id);
    }
    assert!(matches!(
        read_frame(&mut cur, MAX_FRAME),
        Err(FrameError::Eof)
    ));
}

#[test]
fn wire_shape_is_the_documented_one() {
    let v: Value =
        serde_json::from_slice(&to_payload(&RequestFrame::new(7, Request::Ping)).expect("p"))
            .expect("json");
    assert_eq!(v, json!({"v": 1, "id": 7, "method": "ping"}));
    let req = Request::Detach(NameParam { name: "a".into() });
    let v: Value =
        serde_json::from_slice(&to_payload(&RequestFrame::new(8, req)).expect("p")).expect("json");
    assert_eq!(
        v,
        json!({"v": 1, "id": 8, "method": "detach", "params": {"name": "a"}})
    );
    let err = ResponseFrame::err(3, CtlError::new(ErrorCode::Busy, "in use"));
    let v: Value = serde_json::from_slice(&to_payload(&err).expect("p")).expect("json");
    assert_eq!(
        v,
        json!({"v": 1, "id": 3, "err": {"code": "busy", "message": "in use"}})
    );
    let ok = ResponseFrame::ok(4, Response::Detached);
    let v: Value = serde_json::from_slice(&to_payload(&ok).expect("p")).expect("json");
    assert_eq!(v, json!({"v": 1, "id": 4, "ok": {"type": "detached"}}));
}

#[test]
fn requests_are_strict() {
    let bad = [
        r#"{"v":1,"id":1,"method":"nope"}"#,
        r#"{"v":1,"id":1,"method":"detach","params":{"name":"a","extra":1}}"#,
        r#"{"v":1,"id":1,"method":"detach","params":{}}"#,
        r#"{"v":1,"method":"ping"}"#,
        r#"{"v":1,"id":1,"method":"attach","params":{"name":"a","credential":{"passphrase":"zz"}}}"#,
        r#"[]"#,
    ];
    for b in bad {
        assert!(from_payload::<RequestFrame>(b.as_bytes()).is_err(), "{b}");
    }
    let defaults: RequestFrame = from_payload(
        br#"{"v":1,"id":1,"method":"volume_create","params":{"name":"v","dir":"/d","size_bytes":4096}}"#,
    )
    .expect("defaults");
    let Request::VolumeCreate(c) = defaults.call else {
        panic!("wrong call")
    };
    assert_eq!(
        (c.chunk_size, c.mode, c.id_len),
        (65536, VolumeMode::Integrity, 16)
    );
}

#[test]
fn validation_bounds() {
    let ok = |r: Request| r.validate().is_ok();
    let attach = |f: &dyn Fn(&mut Attach)| {
        let mut a = Attach {
            name: "v".into(),
            queues: None,
            depth: None,
            cpus: None,
            max_io_buf_bytes: None,
            credential: None,
            on_journal_damage: None,
            allow_unverifiable_fork: false,
        };
        f(&mut a);
        Request::Attach(a)
    };
    assert!(ok(attach(&|_| {})));
    assert!(!ok(attach(&|a| a.name = String::new())));
    assert!(!ok(attach(&|a| a.name = "x".repeat(65))));
    assert!(!ok(attach(&|a| a.name = "a b".into())));
    assert!(!ok(attach(&|a| a.queues = Some(0))));
    assert!(!ok(attach(&|a| a.queues = Some(17))));
    assert!(!ok(attach(&|a| a.max_io_buf_bytes = Some(1024))));
    assert!(!ok(attach(&|a| a.cpus = Some("0;1".into()))));
    assert!(!ok(attach(&|a| a.credential = Some(Credential {
        fingerprint: Some("XYZ".into()),
        ..Credential::default()
    }))));
    assert!(!ok(attach(&|a| a.name = ".".into())));
    assert!(!ok(attach(&|a| a.name = "..".into())));
    assert!(ok(attach(&|a| a.name = "..a".into())));
    assert!(!ok(attach(&|a| a.credential = Some(Credential {
        passphrase: Some(Secret::from_text("x")),
        mode: Some(VolumeMode::Plain),
        ..Credential::default()
    }))));
    assert!(ok(attach(&|a| a.credential = Some(Credential {
        mode: Some(VolumeMode::Plain),
        ..Credential::default()
    }))));
    let create = |f: &dyn Fn(&mut VolumeCreate)| {
        let mut c = VolumeCreate {
            name: "v".into(),
            dir: "/d".into(),
            size_bytes: 1 << 30,
            chunk_size: 65536,
            mode: VolumeMode::Integrity,
            compress: Compress::Auto,
            id_len: 16,
            map_fanout: None,
            key_file: None,
            passphrase: None,
            store_max_percent: None,
        };
        f(&mut c);
        Request::VolumeCreate(c)
    };
    assert!(ok(create(&|_| {})));
    assert!(!ok(create(&|c| c.dir = "relative".into())));
    assert!(!ok(create(&|c| c.size_bytes = 100)));
    assert!(!ok(create(&|c| c.chunk_size = 20_000)));
    assert!(!ok(create(&|c| c.chunk_size = 2 << 20)));
    assert!(!ok(create(&|c| c.id_len = 8)));
    assert!(!ok(create(&|c| c.map_fanout = Some(100))));
    assert!(ok(create(&|c| c.store_max_percent = Some(0))));
    assert!(ok(create(&|c| c.store_max_percent = Some(150))));
    assert!(!ok(create(&|c| c.store_max_percent = Some(99))));
    assert!(!ok(create(&|c| c.store_max_percent = Some(10_001))));
    assert!(!ok(create(&|c| c.compress = Compress::Zstd { level: 40 })));
    assert!(!ok(create(&|c| {
        c.mode = VolumeMode::Plain;
        c.passphrase = Some(Secret::from_text("x"));
    })));
    assert!(!ok(Request::Resize(Resize {
        name: "v".into(),
        size_bytes: 0
    })));
}

#[test]
fn secrets_never_print() {
    let s = Secret::from_text("hunter2-canary");
    assert!(!format!("{s:?}").contains("hunter2"));
    let req = Request::Attach(Attach {
        name: "v".into(),
        queues: None,
        depth: None,
        cpus: None,
        max_io_buf_bytes: None,
        credential: Some(Credential {
            passphrase: Some(s.clone()),
            ..Credential::default()
        }),
        on_journal_damage: None,
        allow_unverifiable_fork: false,
    });
    assert!(!format!("{req:?}").contains("hunter2"));
    assert_eq!(s, Secret::from_text("hunter2-canary"));
    assert_ne!(s, Secret::from_text("hunter2-canarX"));
    let oversize = format!("\"{}\"", "ab".repeat(4097));
    assert!(serde_json::from_str::<Secret>(&oversize).is_err());
    assert!(serde_json::from_str::<Secret>("\"abc\"").is_err());
    assert!(serde_json::from_str::<Secret>("\"AB\"").is_err());
}

#[test]
fn error_codes_map_to_exit_codes() {
    let want = [
        (ErrorCode::Busy, 3),
        (ErrorCode::NotFound, 4),
        (ErrorCode::NoSpace, 5),
        (ErrorCode::Downgrade, 6),
        (ErrorCode::WrongKmk, 6),
        (ErrorCode::Rollback, 6),
        (ErrorCode::Fork, 6),
        (ErrorCode::ForkUnverifiable, 6),
        (ErrorCode::JournalDamage, 7),
        (ErrorCode::Invalid, 2),
        (ErrorCode::Io, 1),
        (ErrorCode::Internal, 1),
    ];
    for (c, n) in want {
        assert_eq!(c.exit_code(), n);
    }
}

#[test]
fn newer_daemon_codes_and_mode_conversions() {
    let f: ResponseFrame =
        from_payload(br#"{"v":1,"id":2,"err":{"code":"from_the_future","message":"x"}}"#)
            .expect("parse");
    assert!(matches!(
        f.result,
        Outcome::Err(CtlError {
            code: ErrorCode::Unknown,
            ..
        })
    ));
    assert_eq!(ErrorCode::Unknown.exit_code(), 1);
    for m in [
        VolumeMode::Plain,
        VolumeMode::Integrity,
        VolumeMode::Encrypted,
    ] {
        assert_eq!(VolumeMode::from(lunix_format::CryptoMode::from(m)), m);
    }
    let big = Request::Attach(Attach {
        name: "v".into(),
        queues: None,
        depth: None,
        cpus: None,
        max_io_buf_bytes: Some(MAX_IO_BUF + 1),
        credential: None,
        on_journal_damage: None,
        allow_unverifiable_fork: false,
    });
    assert!(big.validate().is_err());
}

#[test]
fn optional_request_fields_default_when_absent() {
    let a: Attach = serde_json::from_value(json!({"name": "v"})).unwrap();
    assert_eq!(
        (a.on_journal_damage, a.allow_unverifiable_fork),
        (None, false)
    );
    let c: Credential = serde_json::from_value(json!({"passphrase": "00"})).unwrap();
    assert_eq!(c.mode, None);
    let p: Attach =
        serde_json::from_value(json!({"name": "v", "on_journal_damage": "refuse"})).unwrap();
    assert_eq!(p.on_journal_damage, Some(JournalDamagePolicy::Refuse));
    assert_eq!(
        serde_json::to_value(ErrorCode::ForkUnverifiable).unwrap(),
        json!("fork_unverifiable")
    );
    assert_eq!(
        serde_json::to_value(ErrorCode::JournalDamage).unwrap(),
        json!("journal_damage")
    );
}
