//! The codecs of one attached volume, built from its descriptor and root.

use std::sync::Arc;

use lunix_format::volume::VolumeDescriptor;
use lunix_format::{CryptoMode, DekGen};
use lunix_iface::crypto::{ExtentCodec, Keys};
use lunix_iface::Result;

use crate::extent::{AesGcmExtents, PlainExtents};
use crate::keys::VolumeKeys;
use crate::meta::MetaSuite;
use crate::secret::Root;

/// Everything the engine, the store and fsck need to read and write one
/// volume's extents, metadata and commits.
pub struct VolumeCrypto {
    keys: Arc<VolumeKeys>,
    extents: Arc<dyn ExtentCodec>,
    meta: Arc<MetaSuite>,
}

impl VolumeCrypto {
    /// Derives the keys and builds the codecs. `root` is `None` for a `plain`
    /// volume. This does not check the descriptor MAC; `check_attach` does.
    ///
    /// # Errors
    /// `Invalid` when the root does not fit the descriptor's mode.
    pub fn new(desc: &VolumeDescriptor, root: Option<Root>, dek_gen: DekGen) -> Result<Self> {
        let keys = VolumeKeys::derive(desc, root, dek_gen)?;
        let extents: Arc<dyn ExtentCodec> = if keys.mode() == CryptoMode::Encrypted {
            Arc::new(AesGcmExtents::new(Arc::clone(&keys))?)
        } else {
            Arc::new(PlainExtents::new(Arc::clone(&keys))?)
        };
        let meta = Arc::new(MetaSuite::new(Arc::clone(&keys)));
        Ok(Self {
            keys,
            extents,
            meta,
        })
    }

    /// The opaque key handle.
    pub fn keys(&self) -> Keys {
        self.keys.handle()
    }

    /// The concrete keys.
    pub fn volume_keys(&self) -> &Arc<VolumeKeys> {
        &self.keys
    }

    /// Extent sealing and opening.
    pub fn extents(&self) -> &Arc<dyn ExtentCodec> {
        &self.extents
    }

    /// Metadata, commits and the lease.
    pub fn meta(&self) -> &Arc<MetaSuite> {
        &self.meta
    }
}
