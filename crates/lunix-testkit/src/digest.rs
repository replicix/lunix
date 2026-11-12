//! A small keyed 256-bit digest for the fakes.
//!
//! `lunix-testkit` may not depend on `blake3` (its dependency list is fixed by
//! M1.A), so the fakes use this instead of BLAKE3. It is **not cryptographic**:
//! it only has to be deterministic, to depend on every input byte and on the
//! key, and to change when any single byte changes. Every absorb step is a
//! bijection of the lane state for a fixed word, and of the word for a fixed
//! state, so two inputs of equal length that differ in one word never collide.

const IV: [u64; 4] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
];
const MUL: [u64; 4] = [
    0x9e37_79b9_7f4a_7c15,
    0xbf58_476d_1ce4_e5b9,
    0x94d0_49bb_1331_11eb,
    0xd6e8_feb8_6659_fd93,
];

/// Streaming digest state.
pub(crate) struct Digest {
    lanes: [u64; 4],
    len: u64,
    tail: [u8; 8],
    ntail: usize,
}

impl Digest {
    /// A digest in `domain` (separates uses of the same bytes) under `key`
    /// (all zero for the unkeyed uses).
    pub(crate) fn new(domain: u8, key: &[u8; 16]) -> Self {
        let k0 = u64::from_le_bytes(key[..8].try_into().expect("8 bytes"));
        let k1 = u64::from_le_bytes(key[8..].try_into().expect("8 bytes"));
        let mut lanes = IV;
        for (i, lane) in lanes.iter_mut().enumerate() {
            *lane ^= if i % 2 == 0 { k0 } else { k1 };
            *lane ^= u64::from(domain) << (8 * i);
        }
        let mut d = Self {
            lanes,
            len: 0,
            tail: [0; 8],
            ntail: 0,
        };
        // Mix the key and domain in before any data.
        d.absorb(k0 ^ u64::from(domain));
        d.absorb(k1);
        d
    }

    fn absorb(&mut self, w: u64) {
        for (i, h) in self.lanes.iter_mut().enumerate() {
            *h = (*h ^ w.rotate_left(13 * i as u32)).wrapping_mul(MUL[i]);
            *h ^= *h >> 29;
        }
    }

    /// Feeds `data`.
    pub(crate) fn update(&mut self, mut data: &[u8]) -> &mut Self {
        self.len += data.len() as u64;
        if self.ntail > 0 {
            let take = (8 - self.ntail).min(data.len());
            self.tail[self.ntail..self.ntail + take].copy_from_slice(&data[..take]);
            self.ntail += take;
            data = &data[take..];
            if self.ntail < 8 {
                return self;
            }
            self.absorb(u64::from_le_bytes(self.tail));
            self.ntail = 0;
        }
        let (words, rest) = data.as_chunks::<8>();
        for w in words {
            self.absorb(u64::from_le_bytes(*w));
        }
        self.tail[..rest.len()].copy_from_slice(rest);
        self.ntail = rest.len();
        self
    }

    /// The 32-byte result.
    pub(crate) fn finish(mut self) -> [u8; 32] {
        if self.ntail > 0 {
            let mut w = [0u8; 8];
            w[..self.ntail].copy_from_slice(&self.tail[..self.ntail]);
            self.absorb(u64::from_le_bytes(w));
        }
        self.absorb(self.len);
        for _ in 0..2 {
            for (i, m) in MUL.iter().enumerate() {
                self.lanes[i] ^= self.lanes[(i + 1) % 4].rotate_left(17);
                self.lanes[i] = self.lanes[i].wrapping_mul(*m);
                self.lanes[i] ^= self.lanes[i] >> 32;
            }
        }
        let mut out = [0u8; 32];
        for (chunk, lane) in out.chunks_mut(8).zip(self.lanes) {
            chunk.copy_from_slice(&lane.to_le_bytes());
        }
        out
    }
}

/// One-shot digest of the concatenation of `parts`.
pub(crate) fn digest(domain: u8, key: &[u8; 16], parts: &[&[u8]]) -> [u8; 32] {
    let mut d = Digest::new(domain, key);
    for p in parts {
        d.update(p);
    }
    d.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_KEY: [u8; 16] = [0; 16];

    #[test]
    fn splitting_the_input_does_not_change_the_digest() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let whole = digest(1, &NO_KEY, &[&data]);
        for cut in [0, 1, 7, 8, 9, 500, 999, 1000] {
            let (a, b) = data.split_at(cut);
            assert_eq!(digest(1, &NO_KEY, &[a, b]), whole, "cut {cut}");
        }
    }

    #[test]
    fn domain_key_length_and_every_byte_matter() {
        let base = digest(1, &NO_KEY, &[b"hello world"]);
        assert_ne!(base, digest(2, &NO_KEY, &[b"hello world"]));
        assert_ne!(base, digest(1, &[1; 16], &[b"hello world"]));
        assert_ne!(base, digest(1, &NO_KEY, &[b"hello world\0"]));
        for i in 0..11 {
            let mut m = *b"hello world";
            m[i] ^= 1;
            assert_ne!(base, digest(1, &NO_KEY, &[&m]), "byte {i}");
        }
    }
}
