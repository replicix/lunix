//! `state/checkpoint`: the two-slot hint, and the guard on its journal tails.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, OpenOptionsExt};

use lunix_format::local::{CheckpointFile, CheckpointSlot, CKP_FILE_LEN, CKP_SLOT_LEN};
use lunix_format::paths::local as lpath;
use lunix_format::CommitSeq;
use lunix_iface::store::{CheckpointStore, CommitStore};
use lunix_iface::{Error, Result};

use crate::fsops::{io_err, publish, Publish, STATE_POINTS};
use crate::volume::LocalVolume;

/// A checkpoint slot plus the verdict of the tail guard.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CheckpointHint {
    /// The newest valid slot. When `tails_usable` is false its
    /// `lane_tail_ordinals` are all zero: "skip no journal file".
    pub slot: CheckpointSlot,
    /// True when the slot was written for a commit the chain still has
    /// (`last_commit_seq` is at most the tip's `commit_seq`).
    pub tails_usable: bool,
}

/// The tail guard of DESIGN §4.4 step 3 and on-disk-formats §18: lane tails
/// may be used only if the checkpoint was written for a commit at or below
/// the tip. After `--truncate-tip` a checkpoint can describe a commit that no
/// longer exists, and its tails may name files holding records the new tip
/// does not cover.
pub fn tails_usable(slot: &CheckpointSlot, tip: Option<CommitSeq>) -> bool {
    tip.is_some_and(|t| slot.last_commit_seq <= t.0)
}

impl LocalVolume {
    fn read_checkpoint_file(&self) -> Result<Option<Vec<u8>>> {
        match fs::read(self.inner.path(&lpath::state_checkpoint())) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(e)),
        }
    }

    /// Both slots as found and whether the file is all zero (what `zero`
    /// writes), or `None` for a missing or wrong-length file (with a warning
    /// for the latter).
    fn read_checkpoint(&self) -> Result<Option<(CheckpointFile, bool)>> {
        let Some(bytes) = self.read_checkpoint_file()? else {
            return Ok(None);
        };
        match CheckpointFile::decode(&bytes) {
            Ok(f) => Ok(Some((f, bytes.iter().all(|b| *b == 0)))),
            Err(e) => {
                self.inner
                    .warn(format!("checkpoint ignored: wrong length ({e})"));
                Ok(None)
            }
        }
    }

    /// The checkpoint with the tail guard applied against the commit tip. The
    /// plain [`CheckpointStore::load`] returns the slot as written.
    ///
    /// # Errors
    /// `Io`.
    pub fn load_checkpoint_hint(&self) -> Result<Option<CheckpointHint>> {
        let Some(mut slot) = self.load()? else {
            return Ok(None);
        };
        let tip = self.tip()?;
        let usable = tails_usable(&slot, tip);
        if !usable {
            slot.lane_tail_ordinals.iter_mut().for_each(|o| *o = 0);
        }
        Ok(Some(CheckpointHint {
            slot,
            tails_usable: usable,
        }))
    }
}

impl CheckpointStore for LocalVolume {
    fn load(&self) -> Result<Option<CheckpointSlot>> {
        let Some((file, zeroed)) = self.read_checkpoint()? else {
            return Ok(None);
        };
        let Some((_, slot)) = file.newest_valid() else {
            // A zeroed file (`zero`, or a fresh file whose first write died) is
            // expected and silent; anything else is a torn or damaged hint.
            if !zeroed {
                self.inner.warn("checkpoint ignored: no valid slot");
            }
            return Ok(None);
        };
        if slot.volume_uuid != self.inner.uuid {
            self.inner
                .warn("checkpoint ignored: written for another volume");
            return Ok(None);
        }
        Ok(Some(slot.clone()))
    }

    fn store(&self, slot: &CheckpointSlot) -> Result<()> {
        if slot.volume_uuid != self.inner.uuid {
            return Err(Error::invalid("checkpoint is for another volume"));
        }
        let current = self.read_checkpoint()?;
        let (next_slot, newest_seq) = match &current {
            Some((f, _)) => (
                f.next_slot_to_write(),
                f.newest_valid().map_or(0, |(_, s)| s.slot_seq),
            ),
            None => (0, 0),
        };
        let mut to_write = slot.clone();
        to_write.slot_seq = newest_seq.checked_add(1).ok_or(Error::Refused {
            why: "slot_seq exhausted",
        })?;
        let mut bytes = Vec::with_capacity(CKP_SLOT_LEN);
        to_write.encode(&mut bytes)?;
        debug_assert_eq!(bytes.len(), CKP_SLOT_LEN);

        let path = self.inner.path(&lpath::state_checkpoint());
        let existed = current.is_some();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            .open(&path)
            .map_err(io_err)?;
        let len = file.metadata().map_err(io_err)?.len();
        if len != CKP_FILE_LEN as u64 {
            // New (or unusable and then replaced) file: both slots start
            // invalid, so a torn first write leaves no valid slot.
            file.set_len(0).map_err(io_err)?;
            file.set_len(CKP_FILE_LEN as u64).map_err(io_err)?;
        }
        lunix_iface::crashpoint!("store.checkpoint.before_write");
        let at = (next_slot * CKP_SLOT_LEN) as u64;
        file.write_all_at(&bytes, at).map_err(io_err)?;
        lunix_iface::crashpoint!("store.checkpoint.before_fdatasync");
        self.inner.fs.fdatasync(&file).map_err(io_err)?;
        lunix_iface::crashpoint!("store.checkpoint.after_fdatasync");
        if !existed || len != CKP_FILE_LEN as u64 {
            self.inner
                .fs
                .fsync_dir(&self.inner.path("state"))
                .map_err(io_err)?;
            lunix_iface::crashpoint!("store.checkpoint.after_dirsync");
        }
        Ok(())
    }

    fn zero(&self) -> Result<()> {
        // One atomic replace, never two slot writes: a crash between two
        // writes would leave the older slot valid and describing a commit that
        // `--truncate-tip` just removed.
        let zeros = vec![0u8; CKP_FILE_LEN];
        publish(
            &self.inner,
            &self.inner.path("state"),
            "checkpoint",
            &zeros,
            Publish::Replace,
            &STATE_POINTS,
        )
    }
}
