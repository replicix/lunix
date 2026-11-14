//! Warnings about plain volumes (D16, DESIGN §16).

use std::io::{BufRead, Write};

use lunix_iface::ctl::{Request, Response, VolumeMode};

use crate::client::Client;
use crate::error::CliError;

/// Printed on stderr by `volume create --plain`.
pub const PLAIN_CREATE_WARNING: &str = "\
warning: this is a plain volume. Plain mode detects corruption only: its
checksums are unkeyed, so it protects against nobody who can write to the
volume's storage. It has no key material and no key slots, and its mode can
never be changed. Use the default integrity mode or --encrypt to detect
tampering.";

/// Printed (and confirmed) before a backend is added to a plain volume. The
/// `backend` command arrives in phase 2; the text lives here now so that both
/// phases say the same thing.
pub const PLAIN_BACKEND_WARNING: &str = "\
warning: this volume is plain. Plain mode detects corruption only: with a
backend added, anyone who can write to the bucket can change the volume's
contents without detection, and nothing in the volume can be authenticated.
The mode is fixed in the descriptor and cannot be changed later.";

/// Asks the daemon for the volume's mode and returns the backend warning when
/// it is plain.
///
/// # Errors
/// Whatever the status call returns, and `Protocol` for an unexpected reply.
pub fn check_plain_backend(
    client: &mut Client,
    name: &str,
) -> Result<Option<&'static str>, CliError> {
    match client.call(Request::VolumeStatus(lunix_iface::ctl::NameParam {
        name: name.to_owned(),
    }))? {
        Response::VolumeStatus(s) => {
            Ok((s.mode == VolumeMode::Plain).then_some(PLAIN_BACKEND_WARNING))
        }
        _ => Err(CliError::Protocol(
            "unexpected reply to a status request".to_owned(),
        )),
    }
}

/// Prints `warning` and asks to continue unless `yes`; true to go ahead. Only
/// a line starting with `y` or `Y` is a yes.
///
/// # Errors
/// I/O failure on the streams.
pub fn confirm_plain_backend<R: BufRead, W: Write>(
    warning: &str,
    yes: bool,
    input: &mut R,
    err: &mut W,
) -> Result<bool, CliError> {
    writeln!(err, "{warning}")?;
    if yes {
        return Ok(true);
    }
    write!(err, "Continue? [y/N] ")?;
    err.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(matches!(
        line.trim_start().bytes().next(),
        Some(b'y' | b'Y')
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn confirm_accepts_only_yes() {
        for (input, want) in [
            ("y\n", true),
            ("Yes\n", true),
            ("n\n", false),
            ("\n", false),
            ("", false),
        ] {
            let mut err = Vec::new();
            let got =
                confirm_plain_backend("w", false, &mut Cursor::new(input.as_bytes()), &mut err)
                    .unwrap();
            assert_eq!(got, want, "{input:?}");
        }
        let mut err = Vec::new();
        assert!(confirm_plain_backend("w", true, &mut Cursor::new(&b""[..]), &mut err).unwrap());
        assert_eq!(String::from_utf8(err).unwrap(), "w\n");
    }
}
