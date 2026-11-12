//! The device oracle: a flat `Vec<u8>` that applies block-device requests the
//! obvious way, and an operation log that answers the crash question.
//!
//! * [`DeviceOracle`] is the reference every engine is compared with: reads
//!   return what the last write, discard or write-zeroes left (zeros where
//!   nothing was written), invalid requests are recognised with
//!   [`IoSpec::is_valid`].
//! * [`OracleLog`] records the mutating requests in the order the engine
//!   accepted them. After a crash the device must equal the oracle **as of
//!   some prefix** of that log that includes every request a completed FLUSH
//!   (or FUA write) covered: [`OracleLog::first_match`] finds that prefix.
//!
//! Everything is deterministic and allocation-light enough for 2 000-op
//! sequences on devices of tens of MiB.

use lunix_iface::block::IoOp;

use crate::strategies::{pattern, IoSpec};

const BLOCK: usize = 4096;

/// The outcome of [`DeviceOracle::apply`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Applied {
    /// The request is invalid; the engine must answer `EINVAL` and change nothing.
    Invalid,
    /// A read: the bytes it must return.
    Read(Vec<u8>),
    /// Anything else, applied.
    Done,
}

/// A device held as one flat byte vector.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeviceOracle {
    data: Vec<u8>,
}

impl DeviceOracle {
    /// A zeroed device.
    ///
    /// # Panics
    /// If `size_bytes` is not a multiple of 4096.
    pub fn new(size_bytes: u64) -> Self {
        assert!(size_bytes.is_multiple_of(BLOCK as u64), "unaligned size");
        Self {
            data: vec![0; usize::try_from(size_bytes).expect("size fits memory")],
        }
    }

    /// The size in bytes.
    pub fn size_bytes(&self) -> u64 {
        self.data.len() as u64
    }

    /// The whole device.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Grows with zeros.
    ///
    /// # Panics
    /// If `new_size` is smaller or unaligned.
    pub fn grow(&mut self, new_size: u64) {
        assert!(new_size >= self.size_bytes() && new_size.is_multiple_of(BLOCK as u64));
        self.data
            .resize(usize::try_from(new_size).expect("size fits memory"), 0);
    }

    fn span(&self, offset: u64, len: u64) -> std::ops::Range<usize> {
        let start = usize::try_from(offset).expect("offset");
        let end = start + usize::try_from(len).expect("len");
        assert!(end <= self.data.len(), "range past the end");
        start..end
    }

    /// Reads `len` bytes at `offset`.
    ///
    /// # Panics
    /// If the range is outside the device.
    pub fn read(&self, offset: u64, len: u64) -> Vec<u8> {
        self.data[self.span(offset, len)].to_vec()
    }

    /// Writes `bytes` at `offset`.
    ///
    /// # Panics
    /// If the range is outside the device.
    pub fn write(&mut self, offset: u64, bytes: &[u8]) {
        let r = self.span(offset, bytes.len() as u64);
        self.data[r].copy_from_slice(bytes);
    }

    /// Zeroes a range (the effect of discard and write-zeroes).
    ///
    /// # Panics
    /// If the range is outside the device.
    pub fn zero(&mut self, offset: u64, len: u64) {
        let r = self.span(offset, len);
        self.data[r].fill(0);
    }

    /// Applies one request. Flush and FUA need no state here.
    pub fn apply(&mut self, io: &IoSpec) -> Applied {
        if !io.is_valid(self.size_bytes()) {
            return Applied::Invalid;
        }
        let len = u64::from(io.len);
        match io.op {
            IoOp::Read => Applied::Read(self.read(io.offset_bytes, len)),
            IoOp::Write => {
                self.write(io.offset_bytes, &io.payload());
                Applied::Done
            }
            IoOp::Discard | IoOp::WriteZeroes => {
                self.zero(io.offset_bytes, len);
                Applied::Done
            }
            IoOp::Flush => Applied::Done,
        }
    }
}

/// One entry of an [`OracleLog`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogOp {
    /// A write, discard or write-zeroes.
    Mutate(IoSpec),
    /// The device grew to this size.
    Grow(u64),
}

/// The mutating requests in acceptance order, replayable to any prefix.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OracleLog {
    initial_size: u64,
    ops: Vec<LogOp>,
}

impl OracleLog {
    /// An empty log of a device of `size_bytes`.
    pub fn new(size_bytes: u64) -> Self {
        Self {
            initial_size: size_bytes,
            ops: Vec::new(),
        }
    }

    /// Entries so far (the "prefix length" of the current state).
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// True when nothing was logged.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Logs a request that the engine accepted. Reads, flushes and invalid
    /// requests are ignored; returns whether the request was logged.
    pub fn record(&mut self, io: &IoSpec, dev_bytes: u64) -> bool {
        let mutates = matches!(io.op, IoOp::Write | IoOp::Discard | IoOp::WriteZeroes);
        if mutates && io.is_valid(dev_bytes) {
            self.ops.push(LogOp::Mutate(*io));
            true
        } else {
            false
        }
    }

    /// Logs a completed grow.
    pub fn record_grow(&mut self, new_size: u64) {
        self.ops.push(LogOp::Grow(new_size));
    }

    fn apply_op(state: &mut DeviceOracle, op: &LogOp) {
        match op {
            LogOp::Mutate(io) => {
                state.apply(io);
            }
            LogOp::Grow(size) => state.grow(*size),
        }
    }

    /// The device after the first `n` entries.
    ///
    /// # Panics
    /// If `n` is past the end of the log.
    pub fn state_after(&self, n: usize) -> DeviceOracle {
        let mut state = DeviceOracle::new(self.initial_size);
        for op in &self.ops[..n] {
            Self::apply_op(&mut state, op);
        }
        state
    }

    /// The smallest `n >= min_n` such that `actual` equals the device after the
    /// first `n` entries (size included), if any. Cost: one replay to `min_n`
    /// plus the size of each later request.
    ///
    /// # Panics
    /// If `min_n` is past the end of the log.
    pub fn first_match(&self, actual: &[u8], min_n: usize) -> Option<usize> {
        let mut cur = self.state_after(min_n);
        let mut bad = count_bad(cur.bytes(), actual);
        if bad == 0 {
            return Some(min_n);
        }
        for (i, op) in self.ops.iter().enumerate().skip(min_n) {
            match op {
                LogOp::Mutate(io) if io.is_valid(cur.size_bytes()) => {
                    let first = (io.offset_bytes / BLOCK as u64) as usize;
                    let last = ((io.offset_bytes + u64::from(io.len)) / BLOCK as u64) as usize;
                    let before = (first..last)
                        .filter(|&b| !block_eq(cur.bytes(), actual, b))
                        .count();
                    cur.apply(io);
                    let after = (first..last)
                        .filter(|&b| !block_eq(cur.bytes(), actual, b))
                        .count();
                    bad = bad + after - before;
                }
                LogOp::Mutate(_) => {}
                LogOp::Grow(size) => {
                    cur.grow(*size);
                    bad = count_bad(cur.bytes(), actual);
                }
            }
            if bad == 0 {
                return Some(i + 1);
            }
        }
        None
    }

    /// The pattern a write of `seed` leaves, for assertions in tests.
    pub fn pattern_of(seed: u64, len: usize) -> Vec<u8> {
        pattern(seed, len)
    }
}

fn block_eq(a: &[u8], b: &[u8], block: usize) -> bool {
    let r = block * BLOCK..(block + 1) * BLOCK;
    match (a.get(r.clone()), b.get(r)) {
        (Some(x), Some(y)) => x == y,
        (None, None) => true,
        _ => false,
    }
}

fn count_bad(a: &[u8], b: &[u8]) -> usize {
    let blocks = a.len().max(b.len()).div_ceil(BLOCK);
    (0..blocks).filter(|&i| !block_eq(a, b, i)).count()
}
