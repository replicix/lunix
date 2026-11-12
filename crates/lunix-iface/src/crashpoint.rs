//! Named crash points (cross-cutting rule 6).
//!
//! `crashpoint!("journal.after_fdatasync");` marks a durable step. With the
//! `crashpoints` cargo feature **off** (the default) the macro expands to a
//! compile-time type check of its argument and generates no code. With it on,
//! the name is registered statically (so `lunixd --list-crashpoints` can print
//! it and M15 can loop over the list) and, when `LUNIX_CRASH_AT=<name>[:<hit>]`
//! is set, the process sends itself `SIGKILL` at the `hit`th execution of that
//! point (default 1, counted from 1, across all threads).
//!
//! `LUNIX_CRASH_COUNT=<path>` makes [`dump_counts`] (called by a clean exit
//! path of a counting run) write `name hits` lines, so a supervisor can pick the
//! middle and last hit of a workload for the kill runs.
//!
//! Names are `[a-z0-9_.]+`, unique across the program ([`check_registry`]).

/// Declares a crash point. The argument must be a string literal.
#[cfg(feature = "crashpoints")]
#[macro_export]
macro_rules! crashpoint {
    ($name:literal) => {{
        static POINT: $crate::crashpoint::CrashPoint =
            $crate::crashpoint::CrashPoint::new($name, ::core::file!(), ::core::line!());
        $crate::crashpoint::__private::submit! {
            $crate::crashpoint::CrashPointRef(&POINT)
        }
        $crate::crashpoint::hit(&POINT);
    }};
}

/// Declares a crash point (compiled out: the `crashpoints` feature is off).
#[cfg(not(feature = "crashpoints"))]
#[macro_export]
macro_rules! crashpoint {
    ($name:literal) => {{
        const _: &str = $name;
    }};
}

#[cfg(feature = "crashpoints")]
pub use enabled::*;

#[cfg(feature = "crashpoints")]
mod enabled {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;

    #[doc(hidden)]
    pub mod __private {
        pub use inventory::submit;
    }

    /// One registered crash point.
    #[derive(Debug)]
    pub struct CrashPoint {
        /// The name.
        pub name: &'static str,
        /// The source file of the call site.
        pub file: &'static str,
        /// The line of the call site.
        pub line: u32,
        hits: AtomicU64,
    }

    impl CrashPoint {
        /// For the macro.
        #[doc(hidden)]
        pub const fn new(name: &'static str, file: &'static str, line: u32) -> Self {
            Self {
                name,
                file,
                line,
                hits: AtomicU64::new(0),
            }
        }
    }

    /// What the registry collects: a reference to the site's static.
    #[derive(Debug)]
    pub struct CrashPointRef(pub &'static CrashPoint);

    inventory::collect!(CrashPointRef);

    /// Why the registry is not acceptable.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RegistryError {
        /// Two sites use one name.
        Duplicate(String),
        /// A name outside `[a-z0-9_.]+`.
        BadName(String),
    }

    impl fmt::Display for RegistryError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Duplicate(n) => write!(f, "crash point {n:?} is declared twice"),
                Self::BadName(n) => write!(f, "crash point name {n:?} is not [a-z0-9_.]+"),
            }
        }
    }

    impl std::error::Error for RegistryError {}

    /// Every registered name, sorted (duplicates listed twice).
    pub fn registered() -> Vec<&'static str> {
        let mut v: Vec<&'static str> = inventory::iter::<CrashPointRef>
            .into_iter()
            .map(|c| c.0.name)
            .collect();
        v.sort_unstable();
        v
    }

    /// Checks that every name is well formed and unique.
    ///
    /// # Errors
    /// The first violation found.
    pub fn check_registry() -> Result<(), RegistryError> {
        let names = registered();
        for n in &names {
            let ok = !n.is_empty()
                && n.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.'
                });
            if !ok {
                return Err(RegistryError::BadName((*n).to_owned()));
            }
        }
        for w in names.windows(2) {
            if w[0] == w[1] {
                return Err(RegistryError::Duplicate(w[0].to_owned()));
            }
        }
        Ok(())
    }

    #[derive(Debug)]
    struct Config {
        target: Option<(String, u64)>,
        count_path: Option<String>,
    }

    fn parse_target(s: &str) -> Option<(String, u64)> {
        let (name, nth) = match s.rsplit_once(':') {
            Some((n, h)) => (n, h.parse::<u64>().ok().filter(|h| *h >= 1)?),
            None => (s, 1),
        };
        (!name.is_empty()).then(|| (name.to_owned(), nth))
    }

    fn config() -> &'static Config {
        static CONFIG: OnceLock<Config> = OnceLock::new();
        CONFIG.get_or_init(|| Config {
            target: std::env::var("LUNIX_CRASH_AT")
                .ok()
                .and_then(|s| parse_target(&s)),
            count_path: std::env::var("LUNIX_CRASH_COUNT").ok(),
        })
    }

    /// Called by the macro. A no-op unless `LUNIX_CRASH_AT` or
    /// `LUNIX_CRASH_COUNT` is set.
    #[doc(hidden)]
    pub fn hit(point: &'static CrashPoint) {
        let cfg = config();
        if cfg.target.is_none() && cfg.count_path.is_none() {
            return;
        }
        let name = point.name;
        let n = point.hits.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((target, nth)) = &cfg.target {
            if target == name && n == *nth {
                die();
            }
        }
    }

    fn die() -> ! {
        let _ =
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL);
        // SIGKILL cannot be caught; this is only reached if delivery is delayed.
        loop {
            std::thread::park();
        }
    }

    /// The hit counts so far, by name.
    pub fn counts() -> BTreeMap<&'static str, u64> {
        inventory::iter::<CrashPointRef>
            .into_iter()
            .map(|c| (c.0.name, c.0.hits.load(Ordering::SeqCst)))
            .collect()
    }

    /// Writes `name hits` lines to `LUNIX_CRASH_COUNT` if it is set.
    ///
    /// # Errors
    /// The write error.
    pub fn dump_counts() -> std::io::Result<()> {
        let Some(path) = &config().count_path else {
            return Ok(());
        };
        let mut s = String::new();
        for (name, hits) in counts() {
            s.push_str(&format!("{name} {hits}\n"));
        }
        std::fs::write(path, s)
    }
}

/// Without the feature there is no registry: the list is empty.
#[cfg(not(feature = "crashpoints"))]
pub fn registered() -> Vec<&'static str> {
    Vec::new()
}
