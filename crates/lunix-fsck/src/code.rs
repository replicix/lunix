//! The stable finding codes.
//!
//! Every finding carries one of these codes. They are part of the tool's
//! interface (scripts match on them) and never change meaning; a retired code
//! is never reused. The table in [`ALL`] is the documentation: code, severity
//! and what it says. A test walks the table and fails when a code the crate
//! can emit is missing from it.

use crate::report::Severity::{self, Error, Note, Warning};

/// One documented code.
#[derive(Clone, Copy, Debug)]
pub struct CodeInfo {
    /// The stable name.
    pub code: &'static str,
    /// The severity every finding of this code has.
    pub severity: Severity,
    /// What it means and, where one exists, what repairs it.
    pub doc: &'static str,
}

macro_rules! codes {
    ($($(#[$m:meta])* $name:ident = $code:literal, $sev:expr, $doc:literal;)*) => {
        $( $(#[$m])* pub const $name: &str = $code; )*
        /// Every code with its severity and meaning.
        pub const ALL: &[CodeInfo] = &[
            $( CodeInfo { code: $code, severity: $sev, doc: $doc }, )*
        ];
    };
}

codes! {
    // Volume, keys and local state.
    /// The `volume` file does not decode.
    VOLUME_PARSE = "volume_parse", Error, "the volume descriptor does not parse";
    /// The descriptor MAC (or checksum) does not verify.
    VOLUME_MAC = "volume_mac", Error, "the descriptor MAC or checksum does not verify";
    /// The keyring does not parse.
    KEYRING_PARSE = "keyring_parse", Error, "the keyring is missing or does not parse";
    /// No credential, or one that unwraps no slot.
    CREDENTIAL = "credential", Error, "no credential was given, or it unwraps no slot (a damaged keyring looks the same)";
    /// The local pin or the credential kind does not fit the descriptor.
    ATTACH = "attach", Error, "the attach check refused the volume (downgrade, mode mismatch, pinned fingerprint)";
    /// The keyring is older than the highest one seen.
    KEYRING_ROLLBACK = "keyring_rollback", Error, "the keyring is older than the highest keyring_seq this host has seen";
    /// Another process holds the volume lock.
    BUSY = "busy", Error, "the volume is locked by another process";
    /// The volume cannot be read at all.
    IO = "io", Error, "an i/o error stopped a check";
    /// `volume.json` is missing or invalid.
    VOLUME_JSON = "volume_json", Error, "volume.json is missing or invalid";
    /// `state/pins` was rejected.
    STATE_PINS = "state_pins", Error, "state/pins is rejected (a rejected file is never read as 'no floor')";
    /// `state/device` was rejected.
    STATE_DEVICE = "state_device", Warning, "state/device does not parse or names another volume";
    /// A checkpoint slot is damaged or disagrees with the chain.
    CHECKPOINT = "checkpoint", Warning, "a checkpoint slot is damaged or describes a commit above the tip (a hint only)";
    /// A pin object (snapshot, reader, hold) does not authenticate.
    PIN_OBJECT = "pin_object", Error, "a snapshots/, readers/ or holds/ object does not authenticate";
    /// `state/gc/condemned` does not decode.
    CONDEMNED_INVALID = "condemned_invalid", Warning, "state/gc/condemned does not decode (ignored; GC re-marks)";
    /// No rollback floor on this host.
    NO_FLOOR = "no_floor", Note, "no rollback floor is recorded: rollback of earlier history cannot be detected on this host";

    // Commit chain.
    /// The directory holds no commit.
    NO_COMMIT = "no_commit", Error, "state/commits holds no commit";
    /// The tip does not authenticate.
    TIP_AUTH = "tip_auth", Error, "the tip does not authenticate (repair: --repair --truncate-tip)";
    /// The tip is not a FULL commit.
    TIP_NOT_FULL = "tip_not_full", Error, "the tip is not a FULL commit";
    /// A commit between the floor and the tip is missing.
    COMMIT_MISSING = "commit_missing", Error, "a commit at or above chain_floor is missing";
    /// A retained commit does not authenticate.
    COMMIT_AUTH = "commit_auth", Error, "a retained commit does not authenticate or decode";
    /// A chain link is wrong.
    CHAIN_BROKEN = "chain_broken", Error, "commit_seq, parent_hash or epoch of a link is wrong, or genesis does not name the volume";
    /// A commit below the floor is damaged.
    COMMIT_BELOW_FLOOR = "commit_below_floor", Warning, "a commit file below chain_floor does not authenticate (legal only while condemned)";
    /// `chain_floor` disagrees with `retain_last`.
    CHAIN_FLOOR = "chain_floor", Warning, "chain_floor is not the older of the retain_last retained commits";
    /// `chain_floor` is above the tip.
    CHAIN_FLOOR_ABOVE_TIP = "chain_floor_above_tip", Error, "chain_floor is above the tip";
    /// `created_at` decreases along the chain.
    CREATED_AT = "created_at", Warning, "created_at decreases along the chain (a clock step; never a mount error, D17)";
    /// The volume size is not usable.
    SIZE = "size", Error, "the tip's size is not a multiple of the chunk size";
    /// Tip below the rollback floor.
    ROLLBACK = "rollback", Error, "the tip is below the rollback floor in state/pins";
    /// Another commit at the floor's height.
    FORK = "fork", Error, "the chain has another commit than the one the rollback floor recorded";
    /// The walk back to the floor cannot be completed.
    FORK_UNVERIFIABLE = "fork_unverifiable", Error, "a link between the tip and the rollback floor is missing or does not authenticate";

    // Map.
    /// A map node's pack is absent or unusable.
    MAP_NODE_MISSING = "map_node_missing", Error, "a map node's pack is missing or failed its own checks";
    /// A map node is not where its parent says.
    MAP_NODE_LOC = "map_node_loc", Error, "a map child's location is not an extent of its pack";
    /// A node extent's tag does not verify.
    MAP_NODE_AUTH = "map_node_auth", Error, "a map node extent does not authenticate";
    /// A node extent is structurally invalid.
    MAP_NODE_CORRUPT = "map_node_corrupt", Error, "a map node extent is structurally invalid";
    /// A node was stored with a codec other than 0 (R2).
    MAP_NODE_CODEC = "map_node_codec", Error, "a map node is not codec 0 (R2)";
    /// The extent header id is not the node hash the parent expects (R2).
    MAP_NODE_ID = "map_node_id", Error, "a map node extent carries another id than the hash its parent names (R2)";
    /// The recomputed node hash differs from the parent's `child_hash` (R2).
    MAP_NODE_HASH = "map_node_hash", Error, "a map node's recomputed hash differs from child_hash (R2)";
    /// A node has the wrong level, fanout or shape.
    MAP_NODE_SHAPE = "map_node_shape", Error, "a map node's level, fanout or children do not fit its position";
    /// The map root does not match the commit or the size.
    MAP_ROOT = "map_root", Error, "the map root does not fit the commit's size or its empty-root hash";
    /// A leaf entry lies beyond the end of the volume.
    MAP_ENTRY_RANGE = "map_entry_range", Error, "a map entry lies past the end of the volume";
    /// A retained root's entry resolves to no extent.
    MAP_ENTRY_UNRESOLVED = "map_entry_unresolved", Error, "a map entry resolves to no extent, by hint or through the index";
    /// An entry resolves on disk but not after the index is rebuilt.
    INDEX_INVARIANT = "index_invariant", Error, "a map entry with a stale hint does not resolve once the index is rebuilt from the footers";

    // Containers.
    /// A trailer does not decode or does not fit its file.
    CONTAINER_TRAILER = "container_trailer", Error, "a segment or pack trailer is damaged or names another object";
    /// A footer does not authenticate (R3): the container is absent.
    FOOTER_AUTH = "footer_auth", Error, "a footer does not authenticate (R3); the container counts as absent";
    /// A footer authenticates but is invalid.
    FOOTER_CORRUPT = "footer_corrupt", Error, "a footer does not decode or contradicts its trailer";
    /// The extent region does not hash to the footer's `seg_hash` (R3).
    SEG_HASH = "seg_hash", Error, "the extent region does not match the footer's seg_hash (R3)";
    /// The extents found by a scan differ from the footer entries.
    FOOTER_ENTRIES = "footer_entries", Error, "the extents found in the region differ from the footer entries (offset, length, codec or id)";
    /// `--deep`: an extent's tag does not verify.
    EXTENT_AUTH = "extent_auth", Error, "an extent's tag or AEAD does not verify, or its id is not the recomputed id (--deep)";
    /// `--deep`: an extent carries another id than its footer entry.
    EXTENT_ID = "extent_id", Error, "an extent's header id differs from its footer entry (--deep)";
    /// A sealed container nothing references.
    ORPHAN = "orphan", Warning, "a sealed segment or pack that no retained root references and nothing condemns (volume closed cleanly)";
    /// The same after an unclean stop.
    CRASH_LEFTOVER = "crash_leftover", Warning, "a sealed segment or pack that no retained root references (the volume was not closed cleanly)";
    /// A `*.tmp` or `*.open` file.
    LEFTOVER = "leftover", Warning, "a temporary or unsealed file (repair: --repair)";

    // Index.
    /// A named index run or the manifest is damaged.
    INDEX_RUN = "index_run", Error, "an index run or the manifest is damaged (repair: --rebuild-index)";
    /// A footer id has no index entry.
    INDEX_MISSING = "index_missing", Warning, "chunks in sealed segments have no index entry (mount catches up)";
    /// An index entry names a copy that is not there.
    INDEX_STALE = "index_stale", Warning, "index entries name an extent that is not at that location";
    /// An index entry for no stored chunk.
    INDEX_EXTRA = "index_extra", Warning, "index entries for chunks no segment holds";
    /// An index file that is not a run and not the manifest.
    INDEX_FILE = "index_file", Warning, "an unnamed or unreadable file in index/";

    // Journal.
    /// A ring file belongs to another volume.
    JOURNAL_UUID = "journal_uuid", Error, "a journal file belongs to another volume";
    /// Damage above the floor with valid records after it in its lane.
    JOURNAL_RESIDUE = "journal_residue", Note, "journal damage above the floor with valid records after it in its lane: it holds no durable record unless a number at or below the flushed watermark is missing, which journal_loss reports (ADR-4, E1 sign-off 2026-11-16); recovery wipes from the first gap";
    /// Damage at or below the floor.
    JOURNAL_DAMAGE_BELOW_FLOOR = "journal_damage_below_floor", Note, "journal damage at or below the floor: covered by the commit, costs nothing (a commit does not sync the journal, so a crash can tear such records; E1)";
    /// A torn tail.
    JOURNAL_TAIL = "journal_tail", Note, "a torn last record of a lane: crash residue, legal";
    /// The merge stops at or below the watermark.
    JOURNAL_LOSS = "journal_loss", Error, "the merged write_seq order (records and marks) misses a number at or below the flushed watermark, or holds one twice: durable records are lost, `on_journal_damage = refuse` refuses";
    /// Valid records beyond the first gap, above the watermark.
    IGNORED_AFTER_GAP = "ignored_after_gap", Note, "the merged write_seq order (records and marks) misses a number above the flushed watermark: crash residue, recovery ignores and wipes the valid records beyond it";
    /// A journal file name or header is unusable.
    JOURNAL_FILE = "journal_file", Warning, "a file in journal/ has an unusable name or header";
    /// Plain volume.
    PLAIN_NOTICE = "plain_notice", Note, "a plain volume detects corruption only: no check can find a writer who recomputed the checksums";

    // Repair and gc.
    /// A repair was refused.
    REPAIR_REFUSED = "repair_refused", Error, "a requested repair was refused; nothing was changed by it";
    /// A pass pending in `state/gc`.
    GC_PENDING = "gc_pending", Note, "GC work is pending in state/gc";
    /// fsck's per-segment liveness differs from `lunix-gc`'s mark.
    GC_LIVENESS = "gc_liveness", Error, "per-segment live bytes recounted by fsck differ from lunix-gc's mark";
    /// What a GC pass would do now.
    GC_DRY_RUN = "gc_dry_run", Note, "what a GC pass would condemn, delete and compact now (finish it with --repair --gc)";
}

/// The documented entry for `code`.
pub fn info(code: &str) -> Option<&'static CodeInfo> {
    ALL.iter().find(|c| c.code == code)
}
