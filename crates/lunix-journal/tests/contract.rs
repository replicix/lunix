//! The `lunix-iface` journal contract suite against the real journal.

mod common;

use common::{open_journal, scratch, small_cfg};
use lunix_iface::contract::journal::JournalFixture;
use lunix_iface::journal::{Journal, LaneId};
use lunix_journal::{FileJournal, JournalConfig};

struct Fix {
    _dir: tempfile::TempDir,
    cfg: JournalConfig,
    j: Option<FileJournal>,
}

impl Fix {
    fn new() -> Self {
        let dir = scratch();
        let cfg = small_cfg(dir.path().join("journal"), 2, 2);
        let j = Some(open_journal(&cfg));
        Self { _dir: dir, cfg, j }
    }
}

impl JournalFixture for Fix {
    fn journal(&self) -> &dyn Journal {
        self.j.as_ref().expect("journal")
    }

    fn crash_and_reopen(&mut self) {
        // Lanes were dropped by the test; dropping the handle stops the
        // coordinator. Everything written is in the files, as after a kill.
        self.j = None;
        self.j = Some(open_journal(&self.cfg));
    }

    fn fail_next_append(&mut self, lane: LaneId) -> bool {
        self.j.as_ref().expect("journal").fail_next_append(lane);
        true
    }
}

lunix_iface::contract::journal!(Fix::new);
