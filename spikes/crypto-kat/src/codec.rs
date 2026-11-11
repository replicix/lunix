//! Codecs used by the extent model. `Zstd1`/`Zstd3` stand in for "a newer zstd
//! version or another level" (ADR-12 recompress-on-seal), `Lz4` for the probe.

use std::io::Read;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Raw = 0,
    Lz4 = 1,
    Zstd1 = 2,
    Zstd3 = 3,
}

impl Codec {
    pub fn from_u8(v: u8) -> Option<Codec> {
        Some(match v {
            0 => Codec::Raw,
            1 => Codec::Lz4,
            2 => Codec::Zstd1,
            3 => Codec::Zstd3,
            _ => return None,
        })
    }

    pub fn level(self) -> i8 {
        match self {
            Codec::Raw | Codec::Lz4 => 0,
            Codec::Zstd1 => 1,
            Codec::Zstd3 => 3,
        }
    }

    pub fn compress(self, data: &[u8]) -> Vec<u8> {
        match self {
            Codec::Raw => data.to_vec(),
            Codec::Lz4 => lz4_flex::block::compress(data),
            Codec::Zstd1 => zstd::bulk::compress(data, 1).expect("zstd"),
            Codec::Zstd3 => zstd::bulk::compress(data, 3).expect("zstd"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecompressError {
    /// Output would exceed the cap (decompression bomb or lying `ulen`).
    TooBig,
    /// Output length differs from the declared `ulen`.
    LengthMismatch,
    Corrupt,
}

/// Bounded decompression: never produces more than `cap + 1` bytes
/// (Constellation audit finding 2: `codec::decompress_bounded`).
pub fn decompress_bounded(
    codec: Codec,
    payload: &[u8],
    ulen: usize,
    cap: usize,
) -> Result<Vec<u8>, DecompressError> {
    if ulen > cap {
        return Err(DecompressError::TooBig);
    }
    let out = match codec {
        Codec::Raw => {
            if payload.len() > cap {
                return Err(DecompressError::TooBig);
            }
            payload.to_vec()
        }
        Codec::Lz4 => {
            // lz4_flex needs the exact size up front, which is already capped.
            lz4_flex::block::decompress(payload, ulen).map_err(|_| DecompressError::Corrupt)?
        }
        Codec::Zstd1 | Codec::Zstd3 => {
            let dec =
                zstd::stream::read::Decoder::new(payload).map_err(|_| DecompressError::Corrupt)?;
            let mut out = Vec::new();
            dec.take(cap as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|_| DecompressError::Corrupt)?;
            if out.len() > cap {
                return Err(DecompressError::TooBig);
            }
            out
        }
    };
    if out.len() != ulen {
        return Err(DecompressError::LengthMismatch);
    }
    Ok(out)
}
