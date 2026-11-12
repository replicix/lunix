//! The two reads a cut makes of stored extents, both under rule R1: the base
//! chunk of a partial overwrite and the confirmation of a dedup hint. Neither
//! is a scan, so each passes the id it expects.

use lunix_format::node::DataEntry;
use lunix_format::{ChunkId, ExtentKind, Hint};
use lunix_iface::crypto::{OpenExpect, OpenOutput};
use lunix_iface::index::IndexedChunk;
use lunix_iface::map::entry_hint;
use lunix_iface::{Error, ErrorKind, Result};

use crate::deps::DestageDeps;
use crate::stats::DestageStats;

/// Failures that mean "this stored copy cannot be used" as opposed to "the host
/// is failing".
fn is_data_error(e: &Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::NotFound | ErrorKind::Corrupt | ErrorKind::Auth | ErrorKind::Unsupported
    )
}

/// Reads and opens the extent at `hint` into `plain` (cleared first), checking
/// the expected id and the chunk size. `raw` is scratch.
fn read_open(
    deps: &DestageDeps<'_>,
    hint: &Hint,
    expect: &OpenExpect<'_>,
    raw: &mut Vec<u8>,
    out: &mut Vec<u8>,
) -> Result<()> {
    // Rule 2: a payload is never longer than its chunk, whatever a hint says.
    if hint.clen == 0 || hint.clen > deps.params.chunk_size {
        return Err(Error::Corrupt {
            what: "extent",
            detail: "hint clen outside 1 ..= chunk size",
        });
    }
    raw.clear();
    deps.segments.read_extent(hint, raw)?;
    out.clear();
    deps.codec.open(ExtentKind::Data, raw, expect, out)?;
    Ok(())
}

/// Confirms a dedup hit taken from a persisted run (D17): the extent at `hint`
/// must carry exactly `id` and its tag or AEAD must verify. Anything but a
/// host failure means "not confirmed", which the caller treats as a miss.
pub(crate) fn confirm_hint(
    deps: &DestageDeps<'_>,
    id: &ChunkId,
    hint: &Hint,
    raw: &mut Vec<u8>,
    scratch: &mut Vec<u8>,
    stats: &mut DestageStats,
) -> Result<bool> {
    let expect = OpenExpect {
        chunk_size: deps.params.chunk_size,
        chunk_id: Some(id),
        id_prefix: None,
        verify_id: false,
        output: OpenOutput::Stored,
    };
    match read_open(deps, hint, &expect, raw, scratch) {
        Ok(()) => Ok(true),
        Err(e) if is_data_error(&e) => {
            if e.kind() == ErrorKind::Auth {
                stats.extent_auth_failures += 1;
            }
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Reads the base chunk behind `entry` into `out` (`chunk_size` bytes).
///
/// The leaf's id prefix goes in `OpenExpect`; an extent that is not the one
/// the map names fails instead of being patched into the new chunk. A stale
/// hint (the segment is gone, or the extent there is another chunk's) falls
/// back to the index by prefix, and every candidate is confirmed against its
/// full id and tag before use (DESIGN section 7); with no candidate the
/// original error stands.
pub(crate) fn read_base(
    deps: &DestageDeps<'_>,
    entry: &DataEntry,
    raw: &mut Vec<u8>,
    out: &mut Vec<u8>,
    stats: &mut DestageStats,
) -> Result<()> {
    let hint = entry_hint(entry);
    let expect = OpenExpect {
        chunk_size: deps.params.chunk_size,
        chunk_id: None,
        id_prefix: Some(&entry.prefix),
        verify_id: false,
        output: OpenOutput::Plaintext,
    };
    let first = match read_open(deps, &hint, &expect, raw, out) {
        Ok(()) => return check_len(deps, out),
        Err(e) if is_data_error(&e) => e,
        Err(e) => return Err(e),
    };
    if first.kind() == ErrorKind::Auth {
        stats.extent_auth_failures += 1;
    }
    // A lean map (`id_len = 0`) has no prefix to search by.
    let candidates: Vec<IndexedChunk> = match deps.index.get_by_prefix(&entry.prefix) {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::Unsupported => return Err(first),
        Err(e) => return Err(e),
    };
    for cand in candidates {
        if cand.hint == hint || !entry.prefix.matches(&cand.id) {
            continue;
        }
        let expect = OpenExpect {
            chunk_size: deps.params.chunk_size,
            chunk_id: Some(&cand.id),
            id_prefix: None,
            verify_id: false,
            output: OpenOutput::Plaintext,
        };
        match read_open(deps, &cand.hint, &expect, raw, out) {
            Ok(()) => {
                check_len(deps, out)?;
                stats.stale_hint_fallbacks += 1;
                return Ok(());
            }
            Err(e) if is_data_error(&e) => {
                if e.kind() == ErrorKind::Auth {
                    stats.extent_auth_failures += 1;
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(first)
}

fn check_len(deps: &DestageDeps<'_>, out: &[u8]) -> Result<()> {
    if out.len() == deps.params.chunk_size as usize {
        Ok(())
    } else {
        Err(Error::Corrupt {
            what: "extent",
            detail: "base chunk length differs from the chunk size",
        })
    }
}
