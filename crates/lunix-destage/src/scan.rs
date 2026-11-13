//! The zero scan (DESIGN section 5 step 2).

/// True when every byte of `data` is zero.
///
/// ORs 64-byte lanes into eight 64-bit accumulators, which the compiler turns
/// into vector ORs, and looks at the accumulators once per KiB so that data
/// with a non-zero byte early on leaves early.
pub fn is_all_zero(data: &[u8]) -> bool {
    let (lanes, tail) = data.as_chunks::<64>();
    let mut acc = [0u64; 8];
    for group in lanes.chunks(16) {
        for lane in group {
            let (words, _) = lane.as_chunks::<8>();
            for (a, w) in acc.iter_mut().zip(words) {
                *a |= u64::from_ne_bytes(*w);
            }
        }
        if acc.iter().any(|&a| a != 0) {
            return false;
        }
    }
    tail.iter().all(|&b| b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_set_bit_anywhere_is_found() {
        for len in [0usize, 1, 63, 64, 65, 1024, 4096, 16384 + 7] {
            let zeros = vec![0u8; len];
            assert!(is_all_zero(&zeros), "len {len}");
            for pos in [0, len / 2, len.saturating_sub(1)] {
                if pos < len {
                    let mut v = zeros.clone();
                    v[pos] = 0x80;
                    assert!(!is_all_zero(&v), "len {len} pos {pos}");
                }
            }
        }
    }
}
