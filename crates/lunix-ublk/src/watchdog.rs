//! The timeout wrapper of rule W4.
//!
//! A control command that blocks (a `DEL_DEV` with a mounted file system, a
//! `START_DEV` waiting for queues that never fetch) must not block the caller
//! for ever. The command runs on a helper thread; the caller waits with a
//! deadline. On expiry the caller gets [`UblkError::Timeout`] and the helper
//! is left alone: it may still be inside the kernel, and nothing is signalled
//! or killed (W4). Everything the helper owns stays alive with it.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use crate::error::{Result, UblkError};

/// Runs `f` on a helper thread named after `what` and waits at most `timeout`.
///
/// # Errors
/// [`UblkError::Timeout`] when `f` has not returned in time (the helper keeps
/// running); [`UblkError::Io`] when the thread cannot be spawned or `f`
/// panicked.
pub fn run_with_timeout<T, F>(what: &'static str, timeout: Duration, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(format!("ublk-ctl-{what}"))
        .spawn(move || {
            // The receiver may be gone after a timeout; the value is dropped.
            let _ = tx.send(f());
        })?;
    match rx.recv_timeout(timeout) {
        Ok(v) => Ok(v),
        Err(RecvTimeoutError::Timeout) => Err(UblkError::Timeout {
            op: what,
            after: timeout,
        }),
        Err(RecvTimeoutError::Disconnected) => Err(UblkError::Io(std::io::Error::other(
            "control helper thread panicked",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn a_fast_call_returns_its_value() {
        let v = run_with_timeout("fast", Duration::from_secs(5), || 41 + 1).unwrap();
        assert_eq!(v, 42);
    }

    #[test]
    fn a_stuck_call_is_reported_at_the_timeout_and_the_caller_goes_on() {
        let release = Arc::new(AtomicBool::new(false));
        let r2 = Arc::clone(&release);
        let started = Instant::now();
        let err = run_with_timeout("stuck", Duration::from_millis(200), move || {
            while !r2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
        })
        .unwrap_err();
        let waited = started.elapsed();
        assert!(
            matches!(err, UblkError::Timeout { op: "stuck", .. }),
            "{err}"
        );
        assert!(waited >= Duration::from_millis(200), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
        // The caller can run other work, and the helper is not killed.
        assert_eq!(
            run_with_timeout("other", Duration::from_secs(5), || 7).unwrap(),
            7
        );
        release.store(true, Ordering::SeqCst);
    }

    #[test]
    fn a_panicking_call_is_an_error_not_a_hang() {
        let err = run_with_timeout::<(), _>("boom", Duration::from_secs(5), || panic!("boom"))
            .unwrap_err();
        assert!(matches!(err, UblkError::Io(_)), "{err}");
    }
}
