//! A bounds-checked cursor over input bytes and little-endian writers.
//!
//! Every read checks the remaining input first, so no decoder indexes a slice
//! with an attacker-chosen offset. Counts are checked with
//! [`Reader::check_count`] before anything is allocated.

use crate::error::{FormatError, Result};

/// A forward-only reader. `what` names the structure for error messages.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Reader<'a> {
    /// Starts reading `buf`, naming the structure `what`.
    pub fn new(buf: &'a [u8], what: &'static str) -> Self {
        Self { buf, pos: 0, what }
    }

    /// The structure name given at construction.
    pub fn what(&self) -> &'static str {
        self.what
    }

    /// Bytes consumed so far.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Bytes not yet consumed.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// The unread tail, without consuming it.
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    /// Everything consumed so far.
    pub fn consumed(&self) -> &'a [u8] {
        &self.buf[..self.pos]
    }

    /// The whole input.
    pub fn whole(&self) -> &'a [u8] {
        self.buf
    }

    /// Consumes `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let have = self.remaining();
        if n > have {
            return Err(FormatError::Truncated {
                what: self.what,
                need: n,
                have,
            });
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// Consumes a fixed-size array.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    /// Reads one byte.
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    /// Reads a little-endian `u16`.
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian 24-bit integer.
    pub fn u24(&mut self) -> Result<u32> {
        let b = self.array::<3>()?;
        Ok(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
    }

    /// Reads a little-endian `u32`.
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u64`.
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// Reads an `i8`.
    pub fn i8(&mut self) -> Result<i8> {
        Ok(i8::from_le_bytes(self.array()?))
    }

    /// Consumes `n` reserved bytes and rejects any that is not zero.
    pub fn reserved(&mut self, n: usize) -> Result<()> {
        let what = self.what;
        if self.take(n)?.iter().any(|&b| b != 0) {
            return Err(FormatError::Reserved { what });
        }
        Ok(())
    }

    /// Consumes `magic.len()` bytes and requires them to equal `magic`.
    pub fn magic(&mut self, magic: &[u8]) -> Result<()> {
        let what = self.what;
        if self.take(magic.len())? != magic {
            return Err(FormatError::BadMagic { what });
        }
        Ok(())
    }

    /// Reads a version byte and requires `expected`.
    pub fn version_u8(&mut self, expected: u8) -> Result<()> {
        let got = self.u8()?;
        if got != expected {
            return Err(FormatError::BadVersion {
                what: self.what,
                got: u64::from(got),
            });
        }
        Ok(())
    }

    /// Reads a `u16` version and requires `expected`.
    pub fn version_u16(&mut self, expected: u16) -> Result<()> {
        let got = self.u16()?;
        if got != expected {
            return Err(FormatError::BadVersion {
                what: self.what,
                got: u64::from(got),
            });
        }
        Ok(())
    }

    /// Checks `count <= max` and that `count * elem` bytes remain, then
    /// returns the count as `usize`. Call before allocating for `count` items.
    pub fn check_count(
        &self,
        field: &'static str,
        count: u64,
        max: u64,
        elem: usize,
    ) -> Result<usize> {
        if count > max {
            return Err(FormatError::count(self.what, field, count, max));
        }
        let need = usize::try_from(count)
            .ok()
            .and_then(|c| c.checked_mul(elem))
            .ok_or(FormatError::count(self.what, field, count, max))?;
        if need > self.remaining() {
            return Err(FormatError::Truncated {
                what: self.what,
                need,
                have: self.remaining(),
            });
        }
        Ok(count as usize)
    }

    /// Requires that the whole input was consumed.
    pub fn finish(self) -> Result<()> {
        if self.remaining() != 0 {
            return Err(FormatError::Trailing {
                what: self.what,
                extra: self.remaining(),
            });
        }
        Ok(())
    }

    /// Reads `len` bytes as a UTF-8 string with no NUL byte.
    pub fn string(&mut self, field: &'static str, len: usize) -> Result<&'a str> {
        let what = self.what;
        let s = core::str::from_utf8(self.take(len)?)
            .map_err(|_| FormatError::BadString { what, field })?;
        if s.contains('\0') {
            return Err(FormatError::BadString { what, field });
        }
        Ok(s)
    }
}

/// Little-endian writers on a byte vector.
pub trait Put {
    /// Appends one byte.
    fn put_u8(&mut self, v: u8);
    /// Appends a little-endian `u16`.
    fn put_u16(&mut self, v: u16);
    /// Appends the low 24 bits of `v`; the caller checked `v < 2^24`.
    fn put_u24(&mut self, v: u32);
    /// Appends a little-endian `u32`.
    fn put_u32(&mut self, v: u32);
    /// Appends a little-endian `u64`.
    fn put_u64(&mut self, v: u64);
    /// Appends `n` zero bytes.
    fn put_zeros(&mut self, n: usize);
}

impl Put for Vec<u8> {
    fn put_u8(&mut self, v: u8) {
        self.push(v);
    }
    fn put_u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_le_bytes());
    }
    fn put_u24(&mut self, v: u32) {
        self.extend_from_slice(&v.to_le_bytes()[..3]);
    }
    fn put_u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_le_bytes());
    }
    fn put_u64(&mut self, v: u64) {
        self.extend_from_slice(&v.to_le_bytes());
    }
    fn put_zeros(&mut self, n: usize) {
        self.resize(self.len() + n, 0);
    }
}

/// CRC-32C (Castagnoli) of `data`.
pub fn crc32c(data: &[u8]) -> u32 {
    crc32c::crc32c(data)
}

/// Continues a CRC-32C over more bytes.
pub fn crc32c_append(crc: u32, data: &[u8]) -> u32 {
    crc32c::crc32c_append(crc, data)
}

/// Rounds `x` up to a multiple of 8, or `None` on overflow.
pub fn align8(x: usize) -> Option<usize> {
    x.checked_add(7).map(|v| v & !7)
}

/// Rounds `x` up to a multiple of 4096, or `None` on overflow.
pub fn round4k(x: usize) -> Option<usize> {
    x.checked_add(4095).map(|v| v & !4095)
}

/// Narrows a `u64` field to `usize`.
pub fn to_usize(what: &'static str, field: &'static str, v: u64) -> Result<usize> {
    usize::try_from(v).map_err(|_| FormatError::range(what, field, v))
}
