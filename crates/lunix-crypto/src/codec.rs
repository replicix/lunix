//! Bounded decompression and the end of the open path, shared by the
//! encrypted and the unencrypted extent codecs.
//!
//! Decompression is capped by the **volume's chunk size** (64 KiB for map
//! nodes), never by a length read from the object: the destination buffer is
//! sized from the already-checked `ulen`, and the decoders write into it and
//! nowhere else (DESIGN section 15).

use std::cell::RefCell;

use lunix_format::extent::MAX_NODE_ULEN;
use lunix_format::{Codec, ExtentKind};
use lunix_iface::crypto::{OpenExpect, OpenOutput};
use lunix_iface::{Error, Result};

const WHAT: &str = "extent";

fn corrupt(detail: &'static str) -> Error {
    Error::Corrupt { what: WHAT, detail }
}

thread_local! {
    static ZSTD: RefCell<Option<zstd::bulk::Decompressor<'static>>> = const { RefCell::new(None) };
}

/// Decompresses `stored` into `dst`, which must be exactly as long as the
/// chunk (the checked `ulen`). Succeeds only if the output fills `dst`.
pub(crate) fn decompress_into(codec: Codec, stored: &[u8], dst: &mut [u8]) -> Result<()> {
    let n = match codec {
        Codec::Raw => {
            if stored.len() != dst.len() {
                return Err(corrupt("raw length differs from ulen"));
            }
            dst.copy_from_slice(stored);
            dst.len()
        }
        Codec::Lz4 => lz4_flex::block::decompress_into(stored, dst)
            .map_err(|_| corrupt("lz4 block does not decode"))?,
        Codec::Zstd => ZSTD.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = Some(
                    zstd::bulk::Decompressor::new()
                        .map_err(|_| corrupt("zstd decoder unavailable"))?,
                );
            }
            let dec = slot
                .as_mut()
                .ok_or_else(|| corrupt("zstd decoder unavailable"))?;
            dec.decompress_to_buffer(stored, dst)
                .map_err(|_| corrupt("zstd frame does not decode"))
        })?,
        Codec::ZstdDict => {
            return Err(Error::Unsupported {
                what: "zstd dictionary codec (phase 4)",
            })
        }
    };
    if n == dst.len() {
        Ok(())
    } else {
        Err(corrupt("decompressed length differs from ulen"))
    }
}

/// The largest plaintext an extent of `kind` may declare.
pub(crate) fn plaintext_cap(kind: ExtentKind, chunk_size: u32) -> u32 {
    match kind {
        ExtentKind::Data => chunk_size,
        // The node cap is 65 536 and fits.
        ExtentKind::Node => u32::try_from(MAX_NODE_ULEN).unwrap_or(65_536),
    }
}

/// What `finish` needs to know about the authenticated extent.
pub(crate) struct Opened {
    pub(crate) codec: Codec,
    /// The header `ulen`, already checked against the cap.
    pub(crate) ulen: usize,
    pub(crate) kind: ExtentKind,
}

/// Completes `open` once the extent is authenticated and its stored payload
/// (the codec output) sits at `out[start..]`.
///
/// Decompresses when the caller wants plaintext or asked for the id check,
/// recomputes the chunk id with `id_matches` when `verify_id` is set (data
/// extents only: a node's hash input is not its stored encoding, so the map
/// verifies node hashes), and leaves `out` holding the form `expect.output`
/// asks for.
pub(crate) fn finish(
    o: &Opened,
    start: usize,
    out: &mut Vec<u8>,
    expect: &OpenExpect<'_>,
    id_matches: impl FnOnce(&[u8]) -> bool,
) -> Result<()> {
    let clen = out.len() - start;
    let check_id = expect.verify_id && o.kind == ExtentKind::Data;
    let want_plain = expect.output == OpenOutput::Plaintext;

    if o.codec == Codec::Raw {
        if check_id && !id_matches(&out[start..]) {
            return Err(Error::Auth { what: WHAT });
        }
        return Ok(());
    }
    if !want_plain && !check_id {
        return Ok(());
    }
    // `ulen` is at most the chunk size (data) or 64 KiB (node): bounded.
    out.resize(start + clen + o.ulen, 0);
    let (stored, plain) = out[start..].split_at_mut(clen);
    decompress_into(o.codec, stored, plain)?;
    if check_id && !id_matches(plain) {
        return Err(Error::Auth { what: WHAT });
    }
    if want_plain {
        out.drain(start..start + clen);
    } else {
        out.truncate(start + clen);
    }
    Ok(())
}

/// Restores `out` to `start` after a failed open and wipes what was written,
/// which may be decrypted bytes of an object that did not authenticate.
///
/// Bytes left in the old allocation of a `Vec` that `finish` grew (the data
/// is local plaintext, outside ADR-11's key protection, F11) are not wiped.
pub(crate) fn rollback(out: &mut Vec<u8>, start: usize) {
    if out.len() > start {
        out[start..].fill(0);
        out.truncate(start);
    }
}
