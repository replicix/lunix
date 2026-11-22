//! A simulator of the space gate (ADR-32) for one question: does a policy
//! keep a first fill at a small cap out of `stop`?
//!
//! The real gate (`lunix-space`: ledger, throttle, pace, full latch) and the
//! real pressure rules (`lunix-engine`: `Pressure`, `PressureTracker`,
//! `RingPacer`, `DrainHistory`) run on a simulated clock. The workload, the
//! journal ring, the cuts, the store, the collector and the device are
//! modelled in discrete time. See `docs/plans/v1/reports/p1-spacesim.md`.

pub mod calibration;
pub mod experiments;
pub mod gc;
pub mod params;
pub mod policy;
pub mod report;
pub mod rng;
pub mod sim;
pub mod store;

pub use params::{Device, Pattern, Scenario};
pub use policy::{policy_by_name, Policy, POLICY_NAMES};
pub use report::{CellReport, RunReport};
pub use sim::run;
