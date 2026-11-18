//! The validated form of [`SpaceSettings`].

use std::time::Duration;

use lunix_format::local::{FreeFloor, OnFull, SpaceSettings};
use lunix_iface::{Error, Result};

/// The free-space floor, resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Floor {
    Bytes(u64),
    Percent(u8),
}

/// `SpaceSettings` after validation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Policy {
    pub floor: Floor,
    pub on_full: OnFull,
    /// The wait at the limit: `space.full_timeout_ms`, or the daemon's
    /// default when that is null (FREEZE rule change F1, ADR-32).
    pub full_timeout: Duration,
    pub cache_max: Option<u64>,
    pub headroom: u64,
}

impl Policy {
    /// Applies the `VolumeJson::validate` rules that concern `space`; a null
    /// `full_timeout_ms` resolves to `default_timeout`.
    pub fn from_settings(s: &SpaceSettings, default_timeout: Duration) -> Result<Self> {
        if s.full_timeout_ms == Some(0) {
            return Err(Error::invalid("space.full_timeout_ms must be at least 1"));
        }
        // The gate knows no volume size: a percentage cap reaches it
        // resolved to bytes (FREEZE F2; `lunixd::stack::effective_space`).
        if s.cache_max_percent.is_some() {
            return Err(Error::invalid(
                "space.cache_max_percent must be resolved to cache_max against the volume size",
            ));
        }
        let floor = match &s.free_floor {
            FreeFloor::Bytes(b) => Floor::Bytes(*b),
            FreeFloor::Percent(p) => {
                let n = p
                    .strip_suffix('%')
                    .filter(|n| {
                        !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit())
                    })
                    .and_then(|n| n.parse::<u8>().ok())
                    .filter(|n| *n <= 100)
                    .ok_or(Error::invalid(
                        "space.free_floor is not N% with N in 0..=100",
                    ))?;
                Floor::Percent(n)
            }
        };
        Ok(Self {
            floor,
            on_full: s.on_full,
            full_timeout: s
                .full_timeout_ms
                .map_or(default_timeout, Duration::from_millis),
            cache_max: s.cache_max,
            headroom: s.headroom,
        })
    }

    /// The floor in bytes on a file system of `total` bytes.
    pub fn floor_bytes(&self, total: u64) -> u64 {
        match self.floor {
            Floor::Bytes(b) => b,
            Floor::Percent(p) => {
                let v = u128::from(total) * u128::from(p) / 100;
                u64::try_from(v).unwrap_or(u64::MAX)
            }
        }
    }
}
