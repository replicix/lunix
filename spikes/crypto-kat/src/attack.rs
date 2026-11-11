//! Concrete demonstration of the hole in the *first draft* of ADR-11 (key from
//! `DEK || chunk_id`, constant nonce): recompression reuses `(key, nonce)`.

use crate::codec::Codec;
use crate::extent::{OsSalt, Writer};
use crate::kdf::Profile;

#[derive(Debug)]
pub struct XorLeak {
    pub overlap: usize,
    pub xor_equals_plaintext_xor: bool,
    pub lz4_header_len: usize,
    pub literal_len: usize,
    pub crib_len: usize,
    pub recovered_correct: usize,
}

/// LZ4 block: token (high nibble = literal length, 15 = extended) + extension bytes.
pub fn lz4_literal_header(block: &[u8]) -> (usize, usize) {
    let mut lit = (block[0] >> 4) as usize;
    let mut k = 1;
    if lit == 15 {
        loop {
            let b = block[k] as usize;
            k += 1;
            lit += b;
            if b != 255 {
                break;
            }
        }
    }
    (k, lit)
}

/// Compressible chunk: 1 KiB of high-entropy bytes, then that block repeated.
pub fn demo_chunk(len: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    let mut head = Vec::with_capacity(1024);
    for _ in 0..1024 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        head.push((s >> 24) as u8);
    }
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        v.extend_from_slice(&head);
    }
    v.truncate(len);
    v
}

/// Seal one chunk twice under the draft scheme, once `Raw` and once `Lz4`
/// (a recompress), and show what the two ciphertexts give an observer.
pub fn xor_leak_demo(chunk: &[u8]) -> XorLeak {
    let dek = [7u8; 32];
    let family = [1u8; 16];
    let id = *blake3::hash(chunk).as_bytes(); // stand-in for the keyed chunk_id
    let mut w = Writer::new(Profile::V0Draft, dek, family, 0, OsSalt);
    let e1 = w.seal(&id, chunk, Codec::Raw).unwrap();
    let e2 = w.seal(&id, chunk, Codec::Lz4).unwrap();
    let c1 = &e1.body[..e1.body.len() - 16];
    let c2 = &e2.body[..e2.body.len() - 16];
    let p1 = chunk.to_vec();
    let p2 = Codec::Lz4.compress(chunk);
    let n = c1.len().min(c2.len());
    let d: Vec<u8> = (0..n).map(|i| c1[i] ^ c2[i]).collect();
    let xor_equals_plaintext_xor = (0..n).all(|i| d[i] == p1[i] ^ p2[i]);

    // d[i] = P[i] ^ L[i] with L[i] = P[i-k] inside the first literal run.
    let (k, lit) = lz4_literal_header(&p2);
    // The attacker knows only the first k plaintext bytes (a file magic, say).
    let mut rec = chunk[..k].to_vec();
    for i in k..(k + lit).min(n) {
        let next = d[i] ^ rec[i - k];
        rec.push(next);
    }
    let recovered_correct = rec.iter().zip(chunk.iter()).filter(|(a, b)| a == b).count();
    XorLeak {
        overlap: n,
        xor_equals_plaintext_xor,
        lz4_header_len: k,
        literal_len: lit,
        crib_len: k,
        recovered_correct,
    }
}
