//! The control-socket client.

use std::os::unix::net::UnixStream;
use std::path::Path;

use lunix_iface::ctl::{
    from_payload, read_frame, to_payload, write_frame, FrameError, Outcome, Request, RequestFrame,
    Response, ResponseFrame, MAX_FRAME, PROTOCOL_VERSION,
};

use crate::error::CliError;

/// One connection to `lunixd`.
pub struct Client {
    stream: UnixStream,
    next_id: u64,
}

impl Client {
    /// Connects to the control socket.
    ///
    /// # Errors
    /// [`CliError::Connect`] with the OS error.
    pub fn connect(path: &Path) -> Result<Self, CliError> {
        let stream = UnixStream::connect(path).map_err(|source| CliError::Connect {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Self { stream, next_id: 1 })
    }

    /// Sends a request and waits for its response. The request is validated
    /// first, so a bad field is a usage error without a round trip.
    ///
    /// # Errors
    /// `Usage` for an invalid request, `Protocol` for a broken stream or a
    /// reply that does not match, `Daemon` for an error response.
    pub fn call(&mut self, req: Request) -> Result<Response, CliError> {
        req.validate().map_err(|e| CliError::Usage(e.to_string()))?;
        let id = self.next_id;
        self.next_id += 1;
        let payload = to_payload(&RequestFrame::new(id, req)).map_err(frame_err)?;
        write_frame(&mut self.stream, &payload, MAX_FRAME).map_err(frame_err)?;
        let reply = read_frame(&mut self.stream, MAX_FRAME).map_err(frame_err)?;
        let frame: ResponseFrame = from_payload(&reply).map_err(frame_err)?;
        if frame.v != PROTOCOL_VERSION {
            return Err(CliError::Protocol(format!(
                "the daemon speaks protocol {}, this client speaks {PROTOCOL_VERSION}",
                frame.v
            )));
        }
        if frame.id != id {
            return Err(CliError::Protocol(
                "the daemon answered a different request".to_owned(),
            ));
        }
        match frame.result {
            Outcome::Ok(r) => Ok(r),
            Outcome::Err(e) => Err(CliError::Daemon(e)),
        }
    }
}

fn frame_err(e: FrameError) -> CliError {
    match e {
        FrameError::Eof | FrameError::Truncated { .. } => {
            CliError::Protocol("the daemon closed the connection".to_owned())
        }
        FrameError::Io(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            ) =>
        {
            CliError::Protocol("the daemon closed the connection".to_owned())
        }
        other => CliError::Protocol(format!("control socket: {other}")),
    }
}
