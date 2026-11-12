//! The RAM filter of one run: a plain Bloom filter keyed by `xxh3`.
//!
//! Chunk ids are already uniform hashes, but `xxh3` is used anyway so a
//! volume with a weak or adversarial id function (plain mode with a custom
//! hash) cannot make every probe land on the same bits. The filter is a
//! hint: a false positive costs one block read, a false negative cannot
//! happen because every entry of the run is inserted.

use lunix_format::ChunkId;
use xxhash_rust::xxh3::xxh3_128;

/// Filter bits per indexed chunk: 2 bytes, about 0.05 % false positives.
pub(crate) const BITS_PER_ENTRY: u64 = 16;
const PROBES: u64 = 11;

#[derive(Debug)]
pub(crate) struct Bloom {
    words: Vec<u64>,
}

impl Bloom {
    /// A filter sized for `entries` ids. The caller has already bounded
    /// `entries` by the size of the file it came from.
    pub(crate) fn with_entries(entries: u64) -> Self {
        let bits = entries.saturating_mul(BITS_PER_ENTRY).max(64);
        let words = usize::try_from(bits.div_ceil(64)).unwrap_or(usize::MAX / 8);
        Self {
            words: vec![0; words],
        }
    }

    /// Heap bytes held.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.words.len() * 8
    }

    fn positions(&self, id: &ChunkId) -> impl Iterator<Item = (usize, u64)> {
        let h = xxh3_128(&id.0);
        let h1 = h as u64;
        let h2 = (h >> 64) as u64 | 1;
        let nbits = self.words.len() as u64 * 64;
        (0..PROBES).map(move |i| {
            let x = h1.wrapping_add(i.wrapping_mul(h2));
            // Multiply-shift range reduction: uniform without a division.
            let bit = ((u128::from(x) * u128::from(nbits)) >> 64) as u64;
            ((bit / 64) as usize, 1u64 << (bit % 64))
        })
    }

    pub(crate) fn insert(&mut self, id: &ChunkId) {
        let probes: Vec<(usize, u64)> = self.positions(id).collect();
        for (w, m) in probes {
            self.words[w] |= m;
        }
    }

    pub(crate) fn maybe_contains(&self, id: &ChunkId) -> bool {
        self.positions(id).all(|(w, m)| self.words[w] & m != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> ChunkId {
        ChunkId(*blake3::hash(&n.to_le_bytes()).as_bytes())
    }

    #[test]
    fn no_false_negatives_and_few_false_positives() {
        let n = 200_000u64;
        let mut b = Bloom::with_entries(n);
        for i in 0..n {
            b.insert(&id(i));
        }
        assert!((0..n).all(|i| b.maybe_contains(&id(i))));
        let fp = (n..2 * n).filter(|i| b.maybe_contains(&id(*i))).count();
        // 16 bits per entry and 11 probes give about 0.05 %; allow 0.2 %.
        assert!(fp < (n / 500) as usize, "{fp} false positives of {n}");
        assert_eq!(b.heap_bytes() as u64, n * 2);
    }
}
