//! The GC-specific checks of `lunix-fsck` (M17.B fills this file; M14 only
//! creates the stub, declares `mod gc;` and calls it).
//!
//! The contract M17.B implements (plan M17.B "fsck integration"):
//!
//! * the condemned list is parsed; files it names that still exist are
//!   `ok (gc pending)`, files it names that are gone are `ok`;
//! * a missing file that no retained root resolves to is legal, one a
//!   retained root needs (after hint and index fallback) is an error;
//! * `--repair --gc` can finish a pending pass offline; the dry-run output is
//!   the same code path (`ctx.repair` says which);
//! * per-segment liveness recounted from the maps equals `lunix-gc`'s.
//!
//! `GcContext` gives read access to everything the other checks learned
//! (`facts`: the verified chain, every container, the walk's entries and the
//! segments the retained roots reference) and to the locked volume. Add fields
//! here freely; the signature of [`check_gc`] does not change.

use std::collections::HashSet;

use lunix_format::gcfile::Condemned;
use lunix_format::ObjId;

use crate::facts::Facts;
use crate::report::Finding;
use crate::session::Session;

/// Input of [`check_gc`].
pub struct GcContext<'a> {
    pub(crate) session: &'a Session,
    pub(crate) facts: &'a Facts,
    /// `--repair --gc` was given: finish a pending pass instead of only
    /// reporting it.
    pub(crate) repair: bool,
}

/// The GC-specific checks. Returns findings (notes such as `gc_pending`
/// included); M14's stub finds nothing.
pub fn check_gc(ctx: &GcContext<'_>) -> Vec<Finding> {
    let _ = (ctx.session, ctx.facts, ctx.repair);
    Vec::new()
}

/// The ids the condemned list names, so that the orphan check does not report
/// them. The body format is M17.A's (`docs/reference/gc.md`); until M17.B
/// parses it the stub names nothing.
pub fn condemned_ids(_list: &Condemned) -> HashSet<ObjId> {
    HashSet::new()
}
