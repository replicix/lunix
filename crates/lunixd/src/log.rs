//! A minimal `tracing` subscriber that writes one line per event to stderr
//! (systemd's journal picks it up). The workspace has no subscriber crate;
//! spans are not tracked, only events at or above the configured level.

use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// The stderr subscriber.
#[derive(Debug)]
pub struct StderrLog {
    max: Level,
    next: AtomicU64,
}

impl StderrLog {
    /// Events at `max` and below (more severe) are written.
    pub fn new(max: Level) -> Self {
        Self {
            max,
            next: AtomicU64::new(1),
        }
    }

    /// Installs it as the global default (once per process; later calls do
    /// nothing).
    pub fn install(max: Level) {
        let _ = tracing::subscriber::set_global_default(Self::new(max));
    }
}

struct Line {
    msg: String,
    fields: String,
}

impl Visit for Line {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.msg.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={:?}", field.name(), value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.msg, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={:?}", field.name(), value);
        }
    }
}

impl Subscriber for StderrLog {
    fn enabled(&self, m: &Metadata<'_>) -> bool {
        *m.level() <= self.max
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::from_level(self.max))
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(self.next.fetch_add(1, Ordering::Relaxed).max(1))
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut line = Line {
            msg: String::new(),
            fields: String::new(),
        };
        event.record(&mut line);
        let m = event.metadata();
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "{ms} {:5} {}: {}{}",
            m.level(),
            m.target(),
            line.msg,
            line.fields
        );
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}
