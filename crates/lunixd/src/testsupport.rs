//! The in-process volume builder for the stack tests (M12b), fsck (M14) and
//! GC on the real stack (M17.B): a volume made by [`crate::stack`] on a
//! scratch directory, opened as a [`VolumeStack`] without a block device.
//!
//! M12a fixes the signatures; M12b owns this file and fills the bodies (the
//! queue driver that calls the engine's `QueueHandler` directly, crash
//! simulation, journal damage past the last flush point).

use std::path::PathBuf;

use lunix_format::local::VolumeJson;
use lunix_format::CryptoMode;

use crate::stack::{EngineTweak, StackError, VolumeStack};

/// A change to the `volume.json` defaults.
pub type JsonTweak = Box<dyn Fn(&mut VolumeJson) + Send + Sync>;

/// Builds a new volume under a scratch directory.
pub struct VolumeBuilder {
    /// The directory that holds the volume directory.
    pub parent: PathBuf,
    /// Chunk size (16 KiB or 64 KiB in the stack tests).
    pub chunk_size: u32,
    /// Size in bytes (a multiple of the chunk size).
    pub size_bytes: u64,
    /// Crypto mode.
    pub mode: CryptoMode,
    /// A passphrase slot (encrypted volumes; integrity volumes get a key file
    /// otherwise).
    pub passphrase: Option<String>,
    /// Journal lanes (queues).
    pub queues: u16,
    /// `volume.json` changes (small journal, short commit interval).
    pub json: Option<JsonTweak>,
    /// Engine settings (background coordinator off, manual ticks).
    pub engine: Option<EngineTweak>,
}

impl VolumeBuilder {
    /// Defaults: 64 KiB chunks, 1 GiB, `integrity`, one queue.
    pub fn new(parent: impl Into<PathBuf>) -> Self {
        Self {
            parent: parent.into(),
            chunk_size: 64 << 10,
            size_bytes: 1 << 30,
            mode: CryptoMode::Integrity,
            passphrase: None,
            queues: 1,
            json: None,
            engine: None,
        }
    }

    /// Creates the volume (descriptor, keys, genesis) without opening it.
    ///
    /// # Errors
    /// What `stack::create_volume` reports.
    pub fn create(self) -> Result<TestVolume, StackError> {
        unimplemented!("M12b: lunixd::testsupport::VolumeBuilder::create")
    }
}

/// A volume made by [`VolumeBuilder`].
pub struct TestVolume {
    /// `<parent>/<uuid>`.
    pub voldir: PathBuf,
    /// The builder's settings, for reopening.
    pub builder: VolumeBuilder,
}

impl TestVolume {
    /// Opens the volume (recovery included) as the daemon would, without a
    /// device: the caller builds queue handlers from `stack.engine`.
    ///
    /// # Errors
    /// What `stack::open_volume` reports (R4, attach check, recovery).
    pub fn open(&self) -> Result<VolumeStack, StackError> {
        unimplemented!("M12b: lunixd::testsupport::TestVolume::open")
    }

    /// A crash: drops `stack` without a final commit, then damages every
    /// journal lane past its last flush point and deletes files that were
    /// never synced (the store's test hook).
    pub fn crash(&self, stack: VolumeStack, seed: u64) {
        let _ = (stack, seed);
        unimplemented!("M12b: lunixd::testsupport::TestVolume::crash")
    }
}
