//! Tests that assert on log lines. They live in their own test binary, which
//! installs ONE process-global capture subscriber: a scoped subscriber plus
//! `rebuild_interest_cache` is racy when tests run in parallel (a callsite
//! first hit on a thread without a subscriber caches "never"). Events are
//! attributed to the thread that emitted them; every test runs its work on its
//! own thread and reads only its own lines.

mod common;

use std::sync::{Mutex, OnceLock};
use std::thread::ThreadId;

use common::scratch;
use common::volume::{damage_record, journal_bytes, open, params, torn_tail_volume};
use lunix_engine::OnJournalDamage;
use lunix_format::CryptoMode;
use lunixd::stack::open_volume;

type Lines = Mutex<Vec<(ThreadId, String)>>;

struct CaptureSub;

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={v:?}", f.name()));
    }
}

fn lines() -> &'static Lines {
    static LINES: OnceLock<Lines> = OnceLock::new();
    LINES.get_or_init(|| {
        tracing::subscriber::set_global_default(CaptureSub)
            .expect("no other global subscriber in this binary");
        Mutex::new(Vec::new())
    })
}

impl tracing::Subscriber for CaptureSub {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, e: &tracing::Event<'_>) {
        let mut m = Fields(String::new());
        e.record(&mut m);
        lines()
            .lock()
            .unwrap()
            .push((std::thread::current().id(), m.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Installs the global subscriber (once) before any event can be emitted.
fn install() {
    let _ = lines();
}

/// The lines the calling thread has emitted so far.
fn mine() -> Vec<String> {
    let me = std::thread::current().id();
    lines()
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| *t == me)
        .map(|(_, l)| l.clone())
        .collect()
}

/// Plan M12 (journal damage at startup): a damaged range is logged, with its
/// sequence bounds, before the first lane open wipes it.
#[test]
fn journal_damage_is_logged_before_the_wipe() {
    install();
    let Some(t) = scratch("jdlog") else { return };
    let mode = CryptoMode::Plain;
    let (v, marks) = torn_tail_volume(t.path());
    damage_record(&v.voldir, &marks[1]); // the middle record; the third is valid
    let before = journal_bytes(&v.voldir);

    let s = open(&v, mode);
    let rep = s.engine.recovery_report();
    assert_eq!(rep.damaged.len(), 1, "{rep:?}");
    let dmg = &rep.damaged[0];
    let all = mine();
    let line = all
        .iter()
        .find(|l| l.contains("journal recovery stopped at a gap or skipped damage"))
        .unwrap_or_else(|| panic!("no damage log line: {all:?}"));
    for want in [
        format!("start: {}", dmg.start),
        format!("end: {}", dmg.end),
        "prev_seq: WriteSeq(1)".to_string(),
        "next_seq: Some(WriteSeq(3))".to_string(),
    ] {
        assert!(line.contains(&want), "{want} not in {line}");
    }
    // Logged before the wipe: the files are as they were until a lane opens.
    assert!(
        journal_bytes(&v.voldir) == before,
        "wiped before the log line"
    );
}

/// A torn last record under `on_journal_damage = refuse` opens as crash
/// residue and says so in the log.
#[test]
fn a_torn_tail_is_logged_as_crash_residue_under_refuse() {
    install();
    let Some(t) = scratch("tornlog") else { return };
    let mode = CryptoMode::Plain;
    let (v, marks) = torn_tail_volume(t.path());
    damage_record(&v.voldir, &marks[2]);
    let mut p = params(&v, mode);
    p.on_journal_damage = OnJournalDamage::Refuse;
    let _s = open_volume(p).expect("a torn tail is crash residue: refuse opens");
    let all = mine();
    assert!(
        all.iter().any(|l| l.contains("treated as crash residue")),
        "the torn tail is logged: {all:?}"
    );
}
