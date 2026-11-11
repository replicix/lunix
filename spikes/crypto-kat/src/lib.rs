//! P0.8 spike: a *model* of ADR-11's data-path crypto, built on the real crate
//! choice (`aws-lc-rs`) so the review rests on executable evidence.
//!
//! Throwaway code (plan 00 "Out of scope"): it is not the product's
//! encryption implementation and its structure must not leak into phase 1
//! beyond what `bench/crypto-kat/RESULTS.md` states. Keys live in plain
//! arrays/`Zeroizing`; the tripwire ledger deliberately stores raw keys.

pub mod attack;
pub mod codec;
pub mod extent;
pub mod kdf;
pub mod meta;
pub mod slots;
pub mod tripwire;

pub const KEY_LEN: usize = 32;
pub const SALT_LEN: usize = 24;
pub const ID_LEN: usize = 32;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;
