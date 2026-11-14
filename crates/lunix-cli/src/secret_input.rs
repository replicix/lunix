//! Reading passphrases: from a terminal without echo, or from a file
//! descriptor. Never from the command line or the environment.

use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use lunix_iface::ctl::Secret;

use crate::error::CliError;

/// The longest passphrase the control protocol carries.
pub const MAX_SECRET_LEN: usize = 4096;

/// The passphrase options shared by `volume create`, `attach` and the key
/// commands.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct PassphraseArgs {
    /// Use a passphrase. It is prompted on the terminal without echo, or read
    /// from --passphrase-fd; it is never taken from the command line or the
    /// environment.
    #[arg(long)]
    pub passphrase: bool,

    /// Read the passphrase from this file descriptor (implies --passphrase).
    /// The descriptor must be a file or a pipe; one trailing newline is
    /// removed.
    #[arg(long, value_name = "FD")]
    pub passphrase_fd: Option<u32>,
}

impl PassphraseArgs {
    /// True when a passphrase was asked for.
    pub fn wanted(&self) -> bool {
        self.passphrase || self.passphrase_fd.is_some()
    }

    /// Reads the passphrase when one was asked for. A terminal prompt asks
    /// twice when `confirm` is set.
    ///
    /// # Errors
    /// `Usage` when there is no terminal and no descriptor, the secret is
    /// empty or too long, or the two entries differ; `Other` on I/O failure.
    pub fn read(&self, confirm: bool) -> Result<Option<Secret>, CliError> {
        if !self.wanted() {
            return Ok(None);
        }
        let secret = match self.passphrase_fd {
            Some(fd) => read_from_fd(fd)?,
            None => {
                let first = prompt_tty("Passphrase: ")?;
                if confirm {
                    let again = prompt_tty("Repeat passphrase: ")?;
                    if first != again {
                        return Err(CliError::Usage("the passphrases do not match".to_owned()));
                    }
                }
                first
            }
        };
        Ok(Some(secret))
    }
}

/// Reads a secret from a stream: everything up to end of input (or, with
/// `line`, up to the first newline), minus one trailing newline.
///
/// # Errors
/// `Usage` for an empty or over-long secret; `Other` on I/O failure.
pub fn read_secret<R: Read>(r: &mut R, line: bool) -> Result<Secret, CliError> {
    // One allocation, sized for the longest secret plus the newline and one
    // byte of overrun, so the buffer never reallocates and leaves a copy.
    let mut buf: Vec<u8> = Vec::with_capacity(MAX_SECRET_LEN + 3);
    let mut byte = [0u8; 1];
    let mut over = false;
    loop {
        match r.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if line && byte[0] == b'\n' {
                    break;
                }
                if buf.len() > MAX_SECRET_LEN + 1 {
                    over = true;
                    break;
                }
                buf.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(CliError::Other(format!("reading the passphrase: {e}"))),
        }
    }
    // One trailing newline; a carriage return goes only together with it.
    if buf.ends_with(b"\r\n") {
        buf.truncate(buf.len() - 2);
    } else if buf.ends_with(b"\n") {
        buf.pop();
    }
    let secret = Secret::new(buf);
    if over || secret.len() > MAX_SECRET_LEN {
        return Err(CliError::Usage(format!(
            "the passphrase is longer than {MAX_SECRET_LEN} bytes"
        )));
    }
    if secret.is_empty() {
        return Err(CliError::Usage("the passphrase is empty".to_owned()));
    }
    Ok(secret)
}

fn read_from_fd(fd: u32) -> Result<Secret, CliError> {
    if fd == 0 {
        if std::io::stdin().is_terminal() {
            return Err(on_terminal(fd));
        }
        return read_secret(&mut std::io::stdin().lock(), false);
    }
    let mut f = File::open(format!("/proc/self/fd/{fd}")).map_err(|e| {
        CliError::Usage(format!(
            "cannot read the passphrase from file descriptor {fd}: {e} (it must be an open file or pipe)"
        ))
    })?;
    if f.is_terminal() {
        return Err(on_terminal(fd));
    }
    read_secret(&mut f, false)
}

fn on_terminal(fd: u32) -> CliError {
    CliError::Usage(format!(
        "file descriptor {fd} is a terminal, where the passphrase would be echoed; \
         use --passphrase without --passphrase-fd to be prompted"
    ))
}

/// The `stty` binary by absolute path (never through `PATH`).
fn stty_path() -> Result<&'static Path, CliError> {
    ["/usr/bin/stty", "/bin/stty"]
        .into_iter()
        .map(Path::new)
        .find(|p| p.exists())
        .ok_or_else(|| {
            CliError::Usage(
                "cannot turn off terminal echo (no stty found); give the passphrase with --passphrase-fd instead"
                    .to_owned(),
            )
        })
}

/// Turns terminal echo back on when dropped.
struct EchoGuard(File);

impl EchoGuard {
    fn off(tty: &File) -> Result<Self, CliError> {
        let status = Command::new(stty_path()?)
            .arg("-echo")
            .stdin(Stdio::from(tty.try_clone()?))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => Ok(Self(tty.try_clone()?)),
            _ => Err(CliError::Usage(
                "cannot turn off terminal echo; give the passphrase with --passphrase-fd instead"
                    .to_owned(),
            )),
        }
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        if let Ok(tty) = self.0.try_clone() {
            let Ok(stty) = stty_path() else { return };
            let _ = Command::new(stty)
                .arg("echo")
                .stdin(Stdio::from(tty))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn prompt_tty(prompt: &str) -> Result<Secret, CliError> {
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| {
            CliError::Usage(
                "no terminal to ask for a passphrase; give it with --passphrase-fd".to_owned(),
            )
        })?;
    let guard = EchoGuard::off(&tty)?;
    tty.write_all(prompt.as_bytes())?;
    tty.flush()?;
    let secret = read_secret(&mut tty, true);
    let _ = tty.write_all(b"\n");
    drop(guard);
    secret
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn strips_one_trailing_newline() {
        let s = read_secret(&mut Cursor::new(b"hunter2\n".to_vec()), false).unwrap();
        assert_eq!(s.expose(), b"hunter2");
        let s = read_secret(&mut Cursor::new(b"hunter2\r\n".to_vec()), false).unwrap();
        assert_eq!(s.expose(), b"hunter2");
        let s = read_secret(&mut Cursor::new(b"a\nb\n\n".to_vec()), false).unwrap();
        assert_eq!(s.expose(), b"a\nb\n");
        // A lone carriage return belongs to the passphrase.
        let s = read_secret(&mut Cursor::new(b"abc\r".to_vec()), false).unwrap();
        assert_eq!(s.expose(), b"abc\r");
        let s = read_secret(&mut Cursor::new(b"abc\r\r\n".to_vec()), false).unwrap();
        assert_eq!(s.expose(), b"abc\r");
    }

    #[test]
    fn line_mode_stops_at_the_first_newline() {
        let s = read_secret(&mut Cursor::new(b"one\ntwo\n".to_vec()), true).unwrap();
        assert_eq!(s.expose(), b"one");
    }

    #[test]
    fn empty_and_oversized_are_usage_errors() {
        for input in [b"".to_vec(), b"\n".to_vec()] {
            let e = read_secret(&mut Cursor::new(input), false).unwrap_err();
            assert_eq!(e.exit_code(), 2);
        }
        let e = read_secret(&mut Cursor::new(vec![b'x'; MAX_SECRET_LEN + 10]), false).unwrap_err();
        assert_eq!(e.exit_code(), 2);
        let ok = read_secret(&mut Cursor::new(vec![b'x'; MAX_SECRET_LEN]), false).unwrap();
        assert_eq!(ok.len(), MAX_SECRET_LEN);
    }

    #[test]
    fn errors_never_contain_the_secret() {
        let e = read_secret(&mut Cursor::new(b"canary-9d41".to_vec()), false).map(|_| ());
        assert!(e.is_ok());
        let e = read_secret(&mut Cursor::new(vec![b'q'; MAX_SECRET_LEN + 10]), false).unwrap_err();
        assert!(!e.to_string().contains("qqq"));
    }
}
