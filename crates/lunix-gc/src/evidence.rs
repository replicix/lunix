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

/// Lines beyond this many bytes are counted, not kept, so the object stays
/// well under the 64 MiB body cap of a `gc/` object.
const EVIDENCE_CAP: usize = 48 << 20;

/// The lines of one pass.
#[derive(Clone, Debug, Default)]
pub(crate) struct Evidence {
    bytes: Vec<u8>,
    lines: u64,
    dropped: u64,
    /// Lines other than `defer`.
    changes: u64,
}

impl Evidence {
    /// Appends one line. `reason` is a fixed ASCII phrase without quotes.
    pub(crate) fn add(
        &mut self,
        time: TimeMs,
        action: Action,
        obj: ObjectRef,
        bytes: u64,
        reason: &'static str,
    ) {
        debug_assert!(!reason.contains(['"', '\\', '\n']));
        self.lines += 1;
        if action != Action::Defer {
            self.changes += 1;
        }
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

    /// The pass changed nothing: no line but `defer` (a repeated deferral is
    /// not written; gc.md "the evidence object").
    pub(crate) fn only_defers(&self) -> bool {
        self.changes == 0
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

    pub(crate) fn heap_bytes(&self) -> usize {
        self.bytes.capacity()
    }
}
