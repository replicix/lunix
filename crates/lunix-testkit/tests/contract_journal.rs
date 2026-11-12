//! `MemJournal` passes the journal contract suite.

use lunix_iface::contract::journal::JournalFixture;
use lunix_iface::journal::{Journal, LaneId};
use lunix_testkit::{MemJournal, MemJournalConfig};

struct Fix {
    journal: MemJournal,
    seed: u64,
}

impl Fix {
    fn new() -> Self {
        Self {
            journal: MemJournal::new(MemJournalConfig::with_lanes(2)),
            seed: 1,
        }
    }
}

impl JournalFixture for Fix {
    fn journal(&self) -> &dyn Journal {
        &self.journal
    }

    fn crash_and_reopen(&mut self) {
        self.seed += 1;
        let image = self.journal.crash(self.seed);
        self.journal = MemJournal::from_image(&image);
    }

    fn fail_next_append(&mut self, lane: LaneId) -> bool {
        self.journal.fail_next_append(lane);
        true
    }
}

lunix_iface::contract::journal!(Fix::new);

/// The same suite on a journal with tiny ring files, so records spread over
/// several files per lane.
struct SmallFiles(Fix);

impl JournalFixture for SmallFiles {
    fn journal(&self) -> &dyn Journal {
        self.0.journal()
    }
    fn crash_and_reopen(&mut self) {
        self.0.seed += 1;
        let image = self.0.journal.crash(self.0.seed);
        self.0.journal = MemJournal::from_image(&image);
    }
    fn fail_next_append(&mut self, lane: LaneId) -> bool {
        self.0.fail_next_append(lane)
    }
}

impl SmallFiles {
    fn new() -> Self {
        let cfg = MemJournalConfig {
            file_bytes: lunix_testkit::journal::MIN_FILE_BYTES,
            ..MemJournalConfig::with_lanes(2)
        };
        Self(Fix {
            journal: MemJournal::new(cfg),
            seed: 100,
        })
    }
}

lunix_iface::contract::journal!(contract_journal_small_files, SmallFiles::new);
