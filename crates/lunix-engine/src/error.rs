//! Typed mount errors of [`Engine::open`](crate::Engine::open).

use lunix_format::CommitSeq;
use lunix_iface::Error;

/// Why a volume did not open. Every variant leaves the volume untouched
/// except where noted (the orphan sweep is idempotent and safe to repeat).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OpenError {
    /// The volume has no commit at all (not even genesis).
    #[error("the volume has no commit")]
    NoCommit,
    /// The tip does not authenticate or decode. There is no automatic
    /// fallback (DESIGN §4.4): the repair is `lunix fsck --repair --truncate-tip`.
    #[error("tip commit {tip} does not authenticate: {source}")]
    TipUnauthenticated {
        /// The tip's `commit_seq`.
        tip: CommitSeq,
        /// What failed.
        #[source]
        source: Error,
    },
    /// A link of the retained chain is missing or broken.
    #[error("commit chain below tip {tip} is broken: {source}")]
    BrokenChain {
        /// The tip's `commit_seq`.
        tip: CommitSeq,
        /// What failed.
        #[source]
        source: Error,
    },
    /// The tip is not a FULL commit (phase 1 writes only FULLs locally).
    #[error("tip commit {tip} is not a FULL commit")]
    TipNotFull {
        /// The tip's `commit_seq`.
        tip: CommitSeq,
    },
    /// R4: the tip is below the host's rollback floor.
    #[error("rollback: tip {tip} is below the recorded floor {floor}")]
    Rollback {
        /// The floor's `max_commit_seq`.
        floor: CommitSeq,
        /// The verified tip.
        tip: CommitSeq,
    },
    /// R4: the tip is at the floor's height with another hash.
    #[error("fork: tip {tip} differs from the commit the floor recorded at that height")]
    Fork {
        /// The tip (and floor) height.
        tip: CommitSeq,
    },
    /// Journal recovery found damage or records beyond a gap and the volume
    /// was opened with `on_journal_damage = refuse`. Nothing was wiped.
    #[error("journal damage found and on_journal_damage = refuse")]
    JournalDamage,
    /// A configuration value does not fit the volume.
    #[error("invalid engine configuration: {0}")]
    Config(&'static str),
    /// A store, journal, index or map failure.
    #[error(transparent)]
    Store(#[from] Error),
}
