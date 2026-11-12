//! `state/commits/<commit_seq:016x>`: create-only commit objects.

use std::fs;
use std::io;

use lunix_format::paths::local as lpath;
use lunix_format::CommitSeq;
use lunix_iface::store::{CommitInfo, CommitStore};
use lunix_iface::{Error, Result};

use crate::fsops::{
    io_err, io_or_missing, list_hex_ids, publish, unlink_durable, Publish, COMMIT_POINTS,
};
use crate::volume::LocalVolume;

/// The largest commit object the format allows (a DELTA body of 68 MiB plus
/// its envelope); anything bigger on disk is not a commit.
const MAX_COMMIT_FILE: u64 = (68 << 20) + 128;

impl LocalVolume {
    fn commit_dir(&self) -> std::path::PathBuf {
        self.inner.path("state/commits")
    }

    fn commit_seqs(&self) -> Result<Vec<CommitInfo>> {
        Ok(list_hex_ids(&self.commit_dir(), "")
            .map_err(io_err)?
            .into_iter()
            .map(|(seq, size_bytes)| CommitInfo {
                seq: CommitSeq(seq),
                size_bytes,
            })
            .collect())
    }
}

impl CommitStore for LocalVolume {
    fn put(&self, seq: CommitSeq, object: &[u8]) -> Result<()> {
        if object.is_empty() || object.len() as u64 > MAX_COMMIT_FILE {
            return Err(Error::invalid("commit object length"));
        }
        let name = format!("{seq}");
        publish(
            &self.inner,
            &self.commit_dir(),
            &name,
            object,
            Publish::CreateOnly("commit"),
            &COMMIT_POINTS,
        )
    }

    fn get(&self, seq: CommitSeq) -> Result<Vec<u8>> {
        let path = self.inner.path(&lpath::commit(seq));
        let size = fs::metadata(&path)
            .map_err(|e| io_or_missing(e, "commit"))?
            .len();
        if size > MAX_COMMIT_FILE {
            return Err(Error::Corrupt {
                what: "commit",
                detail: "longer than any commit object",
            });
        }
        fs::read(&path).map_err(|e| io_or_missing(e, "commit"))
    }

    fn list(&self) -> Result<Vec<CommitInfo>> {
        self.commit_seqs()
    }

    fn tip(&self) -> Result<Option<CommitSeq>> {
        Ok(self.commit_seqs()?.last().map(|c| c.seq))
    }

    fn delete(&self, seq: CommitSeq, chain_floor: CommitSeq) -> Result<u64> {
        if seq >= chain_floor {
            return Err(Error::Refused {
                why: "commit is at or above chain_floor",
            });
        }
        if self.tip()? == Some(seq) {
            return Err(Error::Refused {
                why: "commit is the tip",
            });
        }
        let path = self.inner.path(&lpath::commit(seq));
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let removed = unlink_durable(&self.inner, &path)?;
        Ok(if removed { size } else { 0 })
    }

    fn set_aside(&self, seq: CommitSeq) -> Result<()> {
        let tip = self.tip()?.ok_or_else(|| Error::not_found("commit"))?;
        if seq > tip {
            return Err(Error::not_found("commit"));
        }
        if seq != tip {
            return Err(Error::Refused {
                why: "only the tip can be set aside",
            });
        }
        let from = self.inner.path(&lpath::commit(seq));
        // A damaged file is never deleted: pick a name that is free.
        let mut to = self.inner.path(&format!("{}.damaged", lpath::commit(seq)));
        let mut n = 0u32;
        loop {
            lunix_iface::crashpoint!("store.set_aside.before_rename");
            match self.inner.fs.rename(&from, &to, true) {
                Ok(()) => break,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    n += 1;
                    to = self
                        .inner
                        .path(&format!("{}.damaged.{n}", lpath::commit(seq)));
                }
                Err(e) => return Err(io_or_missing(e, "commit")),
            }
        }
        lunix_iface::crashpoint!("store.set_aside.after_rename");
        self.inner
            .fs
            .fsync_dir(&self.commit_dir())
            .map_err(io_err)?;
        lunix_iface::crashpoint!("store.set_aside.after_dirsync");
        Ok(())
    }
}
