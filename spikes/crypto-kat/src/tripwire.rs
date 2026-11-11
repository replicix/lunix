//! The nonce-uniqueness tripwire: a ledger of every `(key, nonce)` pair the
//! extent writer model has sealed under, with a digest of what it sealed.
//! Two *distinct* messages under one pair is the AES-GCM catastrophe; the
//! ledger turns it into an `Err` the tests can assert on. Re-sealing the
//! *identical* message is harmless (identical ciphertext) and only counted.

use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq)]
pub struct Tripwire {
    pub key_fp: String,
    pub nonce: [u8; 12],
}

#[derive(Default)]
pub struct Ledger {
    seen: HashMap<([u8; 32], [u8; 12]), [u8; 32]>,
    pub benign_repeats: u64,
    pub violations: u64,
    pub seals: u64,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one seal. `msg` is `aad || plaintext` as sealed.
    pub fn record(
        &mut self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        aad: &[u8],
        pt: &[u8],
    ) -> Result<(), Tripwire> {
        self.seals += 1;
        let mut h = blake3::Hasher::new();
        h.update(&(aad.len() as u64).to_le_bytes());
        h.update(aad);
        h.update(pt);
        let digest = *h.finalize().as_bytes();
        match self.seen.insert((*key, *nonce), digest) {
            None => Ok(()),
            Some(prev) if prev == digest => {
                self.benign_repeats += 1;
                Ok(())
            }
            Some(_) => {
                self.violations += 1;
                Err(Tripwire {
                    key_fp: hex::encode(&blake3::hash(key).as_bytes()[..4]),
                    nonce: *nonce,
                })
            }
        }
    }
}
