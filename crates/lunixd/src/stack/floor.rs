//! The engine's rollback-floor seam over `VolumeDir` (ADR-31, rule R4).
//!
//! `lunix-engine` takes the floor through its own `RollbackFloorStore` so it
//! never depended on the iface change; the iface now has
//! `VolumeDir::rollback_floor` / `raise_rollback_floor` and this adapter is the
//! only floor store the daemon wires. (A blanket `impl<T: VolumeDir>` is not
//! possible here: both traits are foreign to this crate.)

use std::sync::Arc;

use lunix_engine::RollbackFloorStore;
use lunix_format::local::RollbackFloor;
use lunix_iface::store::VolumeDir;
use lunix_iface::Result;

/// [`RollbackFloorStore`] over a volume directory's `state/pins` v2.
pub struct DirFloor(pub Arc<dyn VolumeDir>);

impl RollbackFloorStore for DirFloor {
    fn load(&self) -> Result<Option<RollbackFloor>> {
        self.0.rollback_floor()
    }

    fn raise(&self, floor: &RollbackFloor) -> Result<()> {
        self.0.raise_rollback_floor(floor)
    }
}
