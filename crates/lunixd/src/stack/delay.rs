//! Tests only: a segment store whose guest reads can be made to take longer,
//! so that a read part outlives the collector's grace (milestone pgrace).

use std::sync::Arc;
use std::time::Duration;

use lunix_format::segment::FooterBody;
use lunix_format::{Hint, SegId};
use lunix_iface::store::{ExtentLocation, SegmentInfo, SegmentStore, SegmentWriter};
use lunix_iface::Result;
use lunix_store::LocalVolume;

/// How long a guest read of one extent is held: zero for most calls.
pub type ReadDelay = Arc<dyn Fn() -> Duration + Send + Sync>;

/// The name prefix of the engine's read helper threads, the only readers the
/// delay applies to (the destage and the collector read under the coordinator
/// mutex, which a held read would stall).
const GUEST_READERS: &str = "lunix-read-";

/// A [`LocalVolume`] whose `read_extent` on a guest read helper waits first
/// (the part holds its hint while its segment may be collected) or reads and
/// then waits (the part holds the file open across the delete).
pub(crate) struct DelayedSegments {
    inner: LocalVolume,
    delay: ReadDelay,
}

impl DelayedSegments {
    pub(crate) fn new(inner: LocalVolume, delay: ReadDelay) -> Self {
        Self { inner, delay }
    }
}

fn is_guest_reader() -> bool {
    std::thread::current()
        .name()
        .is_some_and(|n| n.starts_with(GUEST_READERS))
}

impl SegmentStore for DelayedSegments {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        self.inner.open_segment()
    }

    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
        if !is_guest_reader() {
            return self.inner.read_extent(hint, out);
        }
        let d = (self.delay)();
        if d.is_zero() {
            return self.inner.read_extent(hint, out);
        }
        // Odd milliseconds hold the file open, even ones hold the hint.
        if d.as_millis() % 2 == 1 {
            let r = self.inner.read_extent(hint, out);
            std::thread::sleep(d);
            r
        } else {
            std::thread::sleep(d);
            self.inner.read_extent(hint, out)
        }
    }

    fn locate(&self, hint: &Hint) -> Result<ExtentLocation> {
        self.inner.locate(hint)
    }

    fn footer(&self, seg: SegId) -> Result<FooterBody> {
        self.inner.footer(seg)
    }

    fn list(&self) -> Result<Vec<SegmentInfo>> {
        SegmentStore::list(&self.inner)
    }

    fn delete(&self, seg: SegId) -> Result<u64> {
        SegmentStore::delete(&self.inner, seg)
    }
}
