//! Shared helpers for the testkit's integration tests.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use lunix_format::CommitSeq;
use lunix_iface::store::{CommitInfo, CommitStore};
use lunix_iface::{Error, Result};

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A scratch commit store for the crypto contract's chain tests. (The full
/// `MemStore` arrives with M1.B2b.)
#[derive(Default)]
pub struct TestCommits {
    commits: Mutex<BTreeMap<u64, Vec<u8>>>,
}

impl CommitStore for TestCommits {
    fn put(&self, seq: CommitSeq, object: &[u8]) -> Result<()> {
        let mut m = lock(&self.commits);
        if m.contains_key(&seq.0) {
            return Err(Error::AlreadyExists { what: "commit" });
        }
        m.insert(seq.0, object.to_vec());
        Ok(())
    }

    fn get(&self, seq: CommitSeq) -> Result<Vec<u8>> {
        lock(&self.commits)
            .get(&seq.0)
            .cloned()
            .ok_or(Error::not_found("commit"))
    }

    fn list(&self) -> Result<Vec<CommitInfo>> {
        Ok(lock(&self.commits)
            .iter()
            .map(|(&k, v)| CommitInfo {
                seq: CommitSeq(k),
                size_bytes: v.len() as u64,
            })
            .collect())
    }

    fn tip(&self) -> Result<Option<CommitSeq>> {
        Ok(lock(&self.commits)
            .keys()
            .next_back()
            .map(|&k| CommitSeq(k)))
    }

    fn delete(&self, seq: CommitSeq, chain_floor: CommitSeq) -> Result<u64> {
        let mut m = lock(&self.commits);
        if seq >= chain_floor || m.keys().next_back() == Some(&seq.0) {
            return Err(Error::Refused {
                why: "commit is the tip or at or above chain_floor",
            });
        }
        Ok(m.remove(&seq.0).map_or(0, |v| v.len() as u64))
    }

    fn set_aside(&self, seq: CommitSeq) -> Result<()> {
        let mut m = lock(&self.commits);
        match m.keys().next_back().copied() {
            None => Err(Error::not_found("commit")),
            Some(t) if t != seq.0 => Err(Error::Refused {
                why: "only the tip can be set aside",
            }),
            Some(t) => {
                m.remove(&t);
                Ok(())
            }
        }
    }
}
