//! [`MemPacks`]: map packs in memory. Just enough of [`PackStore`] for
//! [`FlatMap`](crate::FlatMap) and its tests; the full store (segments,
//! commits, checkpoint, GC state) is `MemStore` in M1.B2b.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lunix_format::node::NodeLoc;
use lunix_format::segment::FooterBody;
use lunix_format::{ObjId, PackId};
use lunix_iface::store::{PackInfo, PackRead, PackStore, PackWrite, SealedPack};
use lunix_iface::{Error, Result};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[derive(Debug, Default)]
struct Shared {
    sealed: Mutex<BTreeMap<u64, Vec<u8>>>,
    next: AtomicU64,
}

/// In-memory map packs. Cloning shares the same packs.
#[derive(Clone, Debug, Default)]
pub struct MemPacks(Arc<Shared>);

impl MemPacks {
    /// An empty store; the first pack is id 1.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total bytes in sealed packs.
    pub fn sealed_bytes(&self) -> u64 {
        lock(&self.0.sealed).values().map(|b| b.len() as u64).sum()
    }
}

struct MemPackWriter {
    shared: Arc<Shared>,
    id: PackId,
    buf: Vec<u8>,
    nodes: u32,
}

impl PackWrite for MemPackWriter {
    fn id(&self) -> PackId {
        self.id
    }

    fn append_node(&mut self, _hash: &[u8; 32], extent: &[u8]) -> Result<NodeLoc> {
        if extent.is_empty() {
            return Err(Error::invalid("empty node extent"));
        }
        let off = u32::try_from(self.buf.len()).map_err(|_| Error::NoSpace)?;
        let len = u32::try_from(extent.len()).map_err(|_| Error::invalid("node too long"))?;
        self.buf.extend_from_slice(extent);
        self.nodes += 1;
        Ok(NodeLoc {
            pack: self.id,
            off,
            len,
        })
    }

    fn len(&self) -> u64 {
        self.buf.len() as u64
    }

    fn seal(self: Box<Self>) -> Result<SealedPack> {
        let size_bytes = self.buf.len() as u64;
        lock(&self.shared.sealed).insert(self.id.raw(), self.buf);
        Ok(SealedPack {
            pack: self.id,
            size_bytes,
            nodes: self.nodes,
        })
    }
}

impl PackRead for MemPacks {
    fn read_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        let sealed = lock(&self.0.sealed);
        let pack = sealed
            .get(&loc.pack.raw())
            .ok_or(Error::not_found("pack"))?;
        let start = loc.off as usize;
        let bytes = start
            .checked_add(loc.len as usize)
            .and_then(|end| pack.get(start..end))
            .ok_or(Error::Corrupt {
                what: "pack",
                detail: "range outside pack",
            })?;
        out.extend_from_slice(bytes);
        Ok(())
    }

    fn pack_footer(&self, _pack: PackId) -> Result<FooterBody> {
        Err(Error::Unsupported {
            what: "MemPacks has no pack footers (MemStore in M1.B2b does)",
        })
    }

    fn list_packs(&self) -> Result<Vec<PackInfo>> {
        lock(&self.0.sealed)
            .iter()
            .map(|(&raw, b)| {
                Ok(PackInfo {
                    pack: ObjId::from_raw(raw)?,
                    size_bytes: b.len() as u64,
                })
            })
            .collect()
    }
}

impl PackStore for MemPacks {
    fn open_pack(&self) -> Result<Box<dyn PackWrite>> {
        let counter = self.0.next.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(Box::new(MemPackWriter {
            shared: Arc::clone(&self.0),
            id: ObjId::new(0, counter)?,
            buf: Vec::new(),
            nodes: 0,
        }))
    }

    fn delete_pack(&self, pack: PackId) -> Result<u64> {
        Ok(lock(&self.0.sealed)
            .remove(&pack.raw())
            .map_or(0, |b| b.len() as u64))
    }
}
