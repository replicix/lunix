//! What the checks learn and hand to later checks.

use std::collections::{BTreeMap, HashSet};

use lunix_format::commit::{Commit, Full};
use lunix_format::gcfile::Condemned;
use lunix_format::local::{RollbackFloor, VolumeJson};
use lunix_format::{CommitSeq, ObjId};
use lunix_iface::store::Leftover;

use crate::containers::Container;
use crate::mapwalk::WalkOut;
use crate::report::{Ratios, Stats};

/// The authenticated tip.
#[derive(Clone, Debug)]
pub(crate) struct Tip {
    pub(crate) seq: CommitSeq,
    /// BLAKE3-256 of the stored bytes.
    pub(crate) hash: [u8; 32],
    pub(crate) full: Full,
}

/// A commit of the verified chain from the floor to the tip.
#[derive(Clone, Debug)]
pub(crate) struct Retained {
    pub(crate) seq: CommitSeq,
    pub(crate) commit: Commit,
    pub(crate) hash: [u8; 32],
}

#[derive(Default)]
pub(crate) struct Facts {
    pub(crate) json: Option<VolumeJson>,
    pub(crate) floor: Option<RollbackFloor>,
    pub(crate) tip: Option<Tip>,
    /// The verified chain, ascending; may be partial when a link failed.
    pub(crate) retained: Vec<Retained>,
    /// Whether the chain from the tip down to `chain_floor` verified.
    pub(crate) chain_ok: bool,
    pub(crate) condemned: Option<Condemned>,
    pub(crate) condemned_ids: HashSet<ObjId>,
    pub(crate) leftovers: Vec<Leftover>,
    pub(crate) closed_cleanly: bool,
    pub(crate) segments: BTreeMap<ObjId, Container>,
    pub(crate) packs: BTreeMap<ObjId, Container>,
    pub(crate) walk: Option<WalkOut>,
    pub(crate) ratios: Option<Ratios>,
    pub(crate) stats: Stats,
    /// Segments and packs some retained commit names.
    pub(crate) named_segments: HashSet<ObjId>,
    pub(crate) named_packs: HashSet<ObjId>,
    /// Segments the retained maps reference (after hint or index resolution).
    pub(crate) referenced_segments: HashSet<ObjId>,
    /// Lanes seen in `journal/` (highest lane + 1).
    pub(crate) journal_lanes: usize,
    /// Sum of `journal/` file sizes.
    pub(crate) journal_bytes: u64,
    pub(crate) uncovered: Vec<String>,
}
