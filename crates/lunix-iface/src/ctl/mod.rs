//! The control protocol between `lunix` and `lunixd` (D13, M12, M13a).
//!
//! * **Framing** ([`frame`]): a `u32` big-endian length, then that many bytes
//!   of one JSON object. The length is checked against a maximum before
//!   anything is allocated (default [`MAX_FRAME`], 1 MiB).
//! * **Messages** ([`msg`]): a request is `{"v":1,"id":7,"method":"attach",
//!   "params":{..}}`; a response is `{"v":1,"id":7,"ok":{"type":"attached",..}}`
//!   or `{"v":1,"id":7,"err":{"code":"busy","message":".."}}`. `v` is the
//!   protocol version; a server answers an unknown `v` with `invalid`.
//! * **Secrets** ([`secret`]): zeroizing, redacted `Debug`, hex on the wire.
//!
//! Unknown request fields are rejected inside `params` (`deny_unknown_fields`
//! on every params struct); an unknown top-level key beside `v`/`id`/`method` is
//! tolerated, because serde cannot combine `flatten` with
//! `deny_unknown_fields`. Unknown error codes decode as `unknown`.
//! Every string and list in a request
//! has a bound checked by [`msg::Request::validate`]; servers call it before
//! acting.

pub mod frame;
pub mod msg;
pub mod secret;

pub use frame::{
    encode_frame, frame_len, from_payload, read_frame, to_payload, write_frame, FrameError,
    MAX_FRAME,
};
pub use msg::*;
pub use secret::Secret;

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;
