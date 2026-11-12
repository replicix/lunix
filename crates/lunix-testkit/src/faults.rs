//! Named fault points for the stateful fakes ([`MemStore`](crate::MemStore),
//! [`MemIndex`](crate::MemIndex)).
//!
//! The real stack kills the process at a `crashpoint!`; a fake lives inside
//! the test process, so a **crash** fault instead marks the fake dead: the
//! operation returns `Io` and so does every later call, while the durable
//! image (taken with the fake's `crash`/`image` method) holds exactly what a
//! kill at that point would have left. Points come in pairs around a durable
//! step (`x.before`, `x.after`): with `.before` the step did not happen, with
//! `.after` it did but its caller never learned of it.
//!
//! A **fail** fault is a one-shot error (`NoSpace` or `Io`) at the point with
//! nothing changed and the fake still alive.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use lunix_iface::{Error, Result};

/// What an armed point does when it fires.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    /// The fake "crashes": this call and every later one fail with `Io`.
    Crash,
    /// This call fails with `NoSpace`; the fake keeps working.
    NoSpace,
    /// This call fails with `Io`; the fake keeps working.
    Io,
}

#[derive(Debug, Default)]
struct Inner {
    hits: BTreeMap<&'static str, u64>,
    armed: Vec<(String, u64, Fault)>,
    dead: bool,
}

/// The registry of points of one fake.
#[derive(Debug, Default)]
pub struct Faults(Mutex<Inner>);

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn crashed() -> Error {
    Error::Io(std::io::Error::other("injected crash"))
}

impl Faults {
    /// Arms `fault` to fire at the `nth` (from 1) execution of `point` counted
    /// from now on; hits so far do not count.
    pub fn arm(&self, point: &str, nth: u64, fault: Fault) {
        let mut g = lock(&self.0);
        let base = g.hits.get(point).copied().unwrap_or(0);
        g.armed.push((point.to_owned(), base + nth.max(1), fault));
    }

    /// Removes every armed fault.
    pub fn disarm(&self) {
        lock(&self.0).armed.clear();
    }

    /// How often each point was reached.
    pub fn hits(&self) -> BTreeMap<&'static str, u64> {
        lock(&self.0).hits.clone()
    }

    /// True once a crash fault fired.
    pub fn is_dead(&self) -> bool {
        lock(&self.0).dead
    }

    /// Marks the fake dead (a crash taken from outside, by `crash()`).
    pub fn kill(&self) {
        lock(&self.0).dead = true;
    }

    /// `Err(Io)` when dead.
    pub fn alive(&self) -> Result<()> {
        if lock(&self.0).dead {
            Err(crashed())
        } else {
            Ok(())
        }
    }

    /// Passes the named point: counts it and fires an armed fault.
    ///
    /// # Errors
    /// `Io` when dead or crashing here; `NoSpace`/`Io` for a fail fault.
    pub fn point(&self, name: &'static str) -> Result<()> {
        let mut g = lock(&self.0);
        if g.dead {
            return Err(crashed());
        }
        let n = {
            let c = g.hits.entry(name).or_insert(0);
            *c += 1;
            *c
        };
        let Some(i) = g.armed.iter().position(|(p, at, _)| p == name && *at == n) else {
            return Ok(());
        };
        let (_, _, fault) = g.armed.remove(i);
        match fault {
            Fault::Crash => {
                g.dead = true;
                Err(crashed())
            }
            Fault::NoSpace => Err(Error::NoSpace),
            Fault::Io => Err(Error::Io(std::io::Error::other("injected i/o error"))),
        }
    }
}
