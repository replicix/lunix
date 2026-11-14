//! Mark, liveness, compaction, condemn, grace, recheck and delete: the
//! phase 1 garbage collector of a local volume (plan M17, DESIGN section 11,
//! `docs/reference/gc.md`).
//!
//! [`Gc`] implements [`lunix_iface::gc::Collector`]. The engine's coordinator
//! calls [`Collector::step`](lunix_iface::gc::Collector::step) between cuts
//! with a [`GcView`](lunix_iface::gc::GcView) of the durable tip; the space
//! ladder's compact step calls
//! [`Collector::urgent`](lunix_iface::gc::Collector::urgent) through the
//! coordinator; `lunix gc --dry-run` calls
//! [`Collector::dry_run`](lunix_iface::gc::Collector::dry_run).
//!
//! What the wiring must do besides calling the trait (M12):
//!
//! * construct the collector with the volume directory as its
//!   [`FloorSource`] (the rollback floor of ADR-31 is not in the view);
//! * hand **every** [`Moved`](lunix_iface::gc::Moved) record a step, an
//!   urgent pass or a failed-then-retried step returns to the next cut
//!   (`MapTxn::rehint`) and name the output segments in that FULL: pending
//!   moves are a cut trigger even on an idle volume;
//! * never call `step` during a cut or with a dirty map.
//!
//! [`liveness`] is the read-only mark that fsck (M17.B) compares with, and
//! [`CondemnedList`] decodes `state/gc/condemned` for it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod collector;
pub mod compact;
pub mod condemned;
pub mod config;
mod evidence;
mod footers;
pub mod mark;
mod points;

pub use collector::Gc;
pub use condemned::{CondemnedEntry, CondemnedList, ObjectRef};
pub use config::{grace_floor, FloorSource, GcConfig, GcHooks, NoHooks};
pub use evidence::Action;
pub use mark::{liveness, LiveExtent, Liveness, Root};
pub use points::CRASH_POINTS;

#[cfg(test)]
mod tests;
