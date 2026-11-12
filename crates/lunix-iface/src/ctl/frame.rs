//! Length-prefixed JSON frames.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;
use zeroize::Zeroizing;

/// The default maximum payload of a frame: 1 MiB.
pub const MAX_FRAME: u32 = 1 << 20;

/// A framing failure. Never echoes payload bytes.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The declared length exceeds the limit; nothing was allocated or read.
    #[error("frame of {len} bytes exceeds the limit of {max}")]
    TooLarge {
        /// The declared (or attempted) length.
        len: u64,
        /// The limit.
        max: u32,
    },
    /// A zero-length frame.
    #[error("empty frame")]
    Empty,
    /// The stream ended inside a frame.
    #[error("truncated frame: need {need} bytes, have {have}")]
    Truncated {
        /// Bytes the frame needs.
        need: usize,
        /// Bytes that arrived.
        have: usize,
    },
    /// The stream ended cleanly between frames.
    #[error("end of stream")]
    Eof,
    /// The payload is not a valid message.
    #[error("malformed message")]
    Malformed,
    /// An I/O error other than end of stream.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

/// Validates a 4-byte header and returns the payload length. For async readers:
/// read 4 bytes, call this, then read exactly that many.
///
/// # Errors
/// `Empty` for 0; `TooLarge` above `max`.
pub fn frame_len(header: [u8; 4], max: u32) -> Result<usize, FrameError> {
    let len = u32::from_be_bytes(header);
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len > max {
        return Err(FrameError::TooLarge {
            len: u64::from(len),
            max,
        });
    }
    Ok(len as usize)
}

/// Builds `header ‖ payload`.
///
/// # Errors
/// `Empty`, or `TooLarge` when `payload` exceeds `max`.
pub fn encode_frame(payload: &[u8], max: u32) -> Result<Vec<u8>, FrameError> {
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    let len = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge {
        len: payload.len() as u64,
        max,
    })?;
    if len > max {
        return Err(FrameError::TooLarge {
            len: u64::from(len),
            max,
        });
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Writes one frame.
///
/// # Errors
/// As [`encode_frame`], plus I/O.
pub fn write_frame<W: Write>(w: &mut W, payload: &[u8], max: u32) -> Result<(), FrameError> {
    let frame = encode_frame(payload, max)?;
    w.write_all(&frame)?;
    w.flush()?;
    Ok(())
}

/// Reads one frame. The payload is returned zeroizing because it may carry a
/// [`Secret`](super::Secret).
///
/// # Errors
/// `Eof` for a clean end before the first byte; `Truncated` for an end inside
/// a frame; `Empty` and `TooLarge` from the header (before any payload
/// allocation); `Io`.
pub fn read_frame<R: Read>(r: &mut R, max: u32) -> Result<Zeroizing<Vec<u8>>, FrameError> {
    let mut header = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut header[got..]) {
            Ok(0) if got == 0 => return Err(FrameError::Eof),
            Ok(0) => return Err(FrameError::Truncated { need: 4, have: got }),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let len = frame_len(header, max)?;
    let mut buf = Zeroizing::new(vec![0u8; len]);
    let mut have = 0;
    while have < len {
        match r.read(&mut buf[have..]) {
            Ok(0) => return Err(FrameError::Truncated { need: len, have }),
            Ok(n) => have += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(buf)
}

/// Serialises a message to a zeroizing JSON payload.
///
/// # Errors
/// `Malformed` if the value cannot be represented.
pub fn to_payload<T: Serialize>(msg: &T) -> Result<Zeroizing<Vec<u8>>, FrameError> {
    serde_json::to_vec(msg)
        .map(Zeroizing::new)
        .map_err(|_| FrameError::Malformed)
}

/// Parses a message from a frame payload.
///
/// # Errors
/// `Malformed` for anything that is not a valid message of type `T`.
pub fn from_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    serde_json::from_slice(payload).map_err(|_| FrameError::Malformed)
}
