//! Tests over the testkit fakes (plan M17.A gate). `tests/` of the crate is
//! M17.B's; these live in the library so they can drive the private state.

mod batch;
mod crash;
mod equivalence;
mod liveness;
mod oracle;
mod pipeline;
mod recheck;
mod refs;
mod review;
mod scale;
mod scenarios;
pub(crate) mod sim;
mod toy;
mod units;
