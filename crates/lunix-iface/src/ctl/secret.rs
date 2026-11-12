//! A secret that cannot leak through `Debug` or `Display`.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

/// The longest secret accepted from the wire, in bytes.
pub const MAX_SECRET_LEN: usize = 4096;

/// A passphrase or key bytes. Zeroized on drop, `Debug` prints
/// `Secret(<redacted>)`, there is no `Display`. It serialises as lowercase hex
/// **only** because the control socket must carry it; never log a serialised
/// request.
#[derive(Clone)]
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    /// Wraps bytes (at most [`MAX_SECRET_LEN`] are meaningful on the wire).
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Copies a string.
    pub fn from_text(s: &str) -> Self {
        Self::new(s.as_bytes().to_vec())
    }

    /// The bytes, for the code that actually uses them.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl PartialEq for Secret {
    /// Compares without early exit (the tests compare round-tripped requests).
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (self.expose(), other.expose());
        let mut diff = a.len() ^ b.len();
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= usize::from(x ^ y);
        }
        diff == 0
    }
}

impl Eq for Secret {}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut hex = Zeroizing::new(String::with_capacity(self.len() * 2));
        for &b in self.expose() {
            hex.push(char::from(DIGITS[usize::from(b >> 4)]));
            hex.push(char::from(DIGITS[usize::from(b & 15)]));
        }
        s.serialize_str(&hex)
    }
}

struct HexVisitor;

impl Visitor<'_> for HexVisitor {
    type Value = Secret;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a lowercase hex string")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Secret, E> {
        let b = v.as_bytes();
        if !b.len().is_multiple_of(2) || b.len() / 2 > MAX_SECRET_LEN {
            return Err(E::custom("bad secret encoding"));
        }
        let nib = |c: u8| -> Result<u8, E> {
            match c {
                b'0'..=b'9' => Ok(c - b'0'),
                b'a'..=b'f' => Ok(c - b'a' + 10),
                _ => Err(E::custom("bad secret encoding")),
            }
        };
        let mut out = Zeroizing::new(Vec::with_capacity(b.len() / 2));
        for pair in b.as_chunks::<2>().0 {
            out.push(nib(pair[0])? << 4 | nib(pair[1])?);
        }
        Ok(Secret(out))
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(HexVisitor)
    }
}
