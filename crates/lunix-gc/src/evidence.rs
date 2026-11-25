//! The pass's evidence lines, written at pass end as the body bytes of
//! `gc/<pass:016x>` (one JSON object per line, `docs/reference/gc.md`).

use std::io::Write as _;

use lunix_format::TimeMs;

use crate::condemned::ObjectRef;

/// What happened to an object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Added to the condemned list.
    Condemn,
    /// A recheck found a reference: removed from the list.
    Uncondemn,
    /// Deleted (or found already gone).
    Delete,
    /// Still condemned, delete postponed to a later pass.
    Defer,
    /// A segment compaction read its live extents from.
    Compact,
    /// A segment compaction wrote.
    Output,
    /// A compaction source with an extent that does not open: skipped.
    Skip,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Condemn => "condemn",
            Self::Uncondemn => "uncondemn",
            Self::Delete => "delete",
            Self::Defer => "defer",
            Self::Compact => "compact",
            Self::Output => "output",
            Self::Skip => "skip",
        }
    }
}

/// Why a condemned object stays condemned for now: what the deferral waits
/// for decides whether its pass writes evidence and is repeated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Deferral {
    /// A FULL that names a copy durably: the pass after the next commit
    /// decides again. No evidence, no retry (the tip changes).
    ForFull,
    /// The rollback floor or the store refused for now: the pass is retried
    /// one interval later, but writes no evidence (a degraded volume would
    /// otherwise put one object per pass and push out the real ones).
    Retry,
    /// The grace runs: written, and the pass repeats until it ends.
    Grace,
}

/// Lines beyond this many bytes are counted, not kept, so the object stays
/// well under the 64 MiB body cap of a `gc/` object.
const EVIDENCE_CAP: usize = 48 << 20;

/// The lines of one pass.
#[derive(Clone, Debug, Default)]
pub(crate) struct Evidence {
    bytes: Vec<u8>,
    lines: u64,
    dropped: u64,
    /// Lines other than `defer`, and grace deferrals.
    changes: u64,
    /// Deferrals that ask for the pass to be repeated without a new tip.
    retries: u64,
}

impl Evidence {
    /// A `defer` line; `kind` says whether it is a change and whether the
    /// pass is repeated.
    pub(crate) fn defer(
        &mut self,
        time: TimeMs,
        obj: ObjectRef,
        reason: &'static str,
        kind: Deferral,
    ) {
        match kind {
            Deferral::ForFull => {}
            Deferral::Retry => self.retries += 1,
            Deferral::Grace => self.changes += 1,
        }
        self.line(time, Action::Defer, obj, 0, reason);
    }

    /// Appends one line other than `defer`. `reason` is a fixed ASCII phrase
    /// without quotes.
    pub(crate) fn add(
        &mut self,
        time: TimeMs,
        action: Action,
        obj: ObjectRef,
        bytes: u64,
        reason: &'static str,
    ) {
        debug_assert!(action != Action::Defer, "a deferral goes through defer");
        self.changes += 1;
        self.line(time, action, obj, bytes, reason);
    }

    fn line(
        &mut self,
        time: TimeMs,
        action: Action,
        obj: ObjectRef,
        bytes: u64,
        reason: &'static str,
    ) {
        debug_assert!(!reason.contains(['"', '\\', '\n']));
        self.lines += 1;
        if self.bytes.len() >= EVIDENCE_CAP {
            self.dropped += 1;
            return;
        }
        // Writing to a Vec cannot fail.
        let _ = writeln!(
            self.bytes,
            "{{\"time\":{},\"action\":\"{}\",\"kind\":\"{}\",\"id\":\"{:016x}\",\"bytes\":{},\"reason\":\"{}\"}}",
            time.0,
            action.name(),
            obj.kind_name(),
            obj.raw(),
            bytes,
            reason
        );
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lines == 0
    }

    /// The pass changed nothing: no line but a `defer` that waits for a FULL,
    /// the floor or the store (not written; gc.md "the evidence object").
    pub(crate) fn only_defers(&self) -> bool {
        self.changes == 0
    }

    /// The pass is repeated without a new tip: it changed something, or a
    /// deferral waits for the floor or the store.
    pub(crate) fn wants_retry(&self) -> bool {
        self.changes > 0 || self.retries > 0
    }

    /// The body bytes; when lines were dropped, a last line says how many.
    pub(crate) fn finish(&self, time: TimeMs) -> Vec<u8> {
        let mut out = self.bytes.clone();
        if self.dropped > 0 {
            let _ = writeln!(
                out,
                "{{\"time\":{},\"action\":\"truncated\",\"kind\":\"pass\",\"id\":\"{:016x}\",\"bytes\":{},\"reason\":\"evidence cap; bytes is the count of lines dropped\"}}",
                time.0, 0, self.dropped
            );
        }
        out
    }

    /// The lines written so far (tests).
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.bytes.capacity()
    }
}
