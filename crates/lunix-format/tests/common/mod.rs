//! Shared helpers of the `lunix-format` integration tests.
//!
//! * golden files (`tests/golden/<name>` plus a `.hex` rendering);
//! * the negative suite: truncation, mutation, canonical re-encode and the
//!   allocation bound (decoding L bytes allocates at most `2 * L + 64 KiB`);
//! * reference hashing for tests (BLAKE3, HMAC-SHA-256): `lunix-format`
//!   itself computes none.
#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fmt::Write as _;
use std::path::PathBuf;

use hmac::{Hmac, KeyInit, Mac};
use lunix_format::FormatError;
use sha2::Sha256;

/// Proptest cases per type (plan M1.A step 5).
pub const CASES: u32 = 10_000;
/// Random byte mutations per golden (plan M1.A step 5).
pub const MUTATIONS: usize = 10_000;

// ---------------------------------------------------------------------------
// Counting allocator (per thread, so parallel tests do not disturb each other).

thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

/// Counts the bytes requested from the system allocator on this thread.
pub struct Counting;

fn bump(n: usize) {
    let _ = ALLOCATED.try_with(|c| c.set(c.get().saturating_add(n)));
}

// SAFETY: delegates every operation to `System`; the counter is thread-local
// and const-initialised, so counting never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump(l.size());
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump(l.size());
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        bump(new);
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// Runs `f` and returns its result and the bytes it requested on this thread.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = ALLOCATED.with(Cell::get);
    let r = f();
    let after = ALLOCATED.with(Cell::get);
    (r, after - before)
}

/// The allocation bound of plan M1.A step 5 for an input of `len` bytes.
pub fn alloc_limit(len: usize) -> usize {
    2 * len + 64 * 1024
}

// ---------------------------------------------------------------------------
// Hex and golden files.

/// Parses hex, ignoring ASCII whitespace.
pub fn hex(s: &str) -> Vec<u8> {
    let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert!(digits.len().is_multiple_of(2), "odd number of hex digits");
    digits
        .chunks(2)
        .map(|p| {
            let n = |c: u8| (c as char).to_digit(16).expect("hex digit") as u8;
            n(p[0]) << 4 | n(p[1])
        })
        .collect()
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn regenerating() -> bool {
    std::env::var_os("LUNIX_REGEN_GOLDEN").is_some()
}

fn render_hex(name: &str, bytes: &[u8]) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# lunix-format golden file {name} ({} bytes).",
        bytes.len()
    );
    let _ = writeln!(
        s,
        "# Changing this file is a format change and needs the maintainer."
    );
    let _ = writeln!(
        s,
        "# Regenerate: LUNIX_REGEN_GOLDEN=1 tools/remote.sh cargo test -p lunix-format --test {}",
        env!("CARGO_CRATE_NAME")
    );
    for (i, row) in bytes.chunks(16).enumerate() {
        let _ = write!(s, "{:08x} ", i * 16);
        for b in row {
            let _ = write!(s, " {b:02x}");
        }
        s.push('\n');
    }
    s
}

fn first_diff(a: &[u8], b: &[u8]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(i) => format!("first difference at offset {i} (golden {:02x}, new {:02x})", a[i], b[i]),
        None => format!("lengths differ (golden {}, new {})", a.len(), b.len()),
    }
}

/// Compares `bytes` with the golden file `name` (`*.bin`) and its `.hex`
/// rendering, byte for byte. With `LUNIX_REGEN_GOLDEN` set, writes both.
/// Returns the golden bytes.
pub fn golden(name: &str, bytes: &[u8]) -> Vec<u8> {
    assert!(name.ends_with(".bin"), "golden names end in .bin");
    let bin = golden_dir().join(name);
    let hexp = golden_dir().join(name.replace(".bin", ".hex"));
    let rendered = render_hex(name, bytes);
    if regenerating() {
        std::fs::create_dir_all(golden_dir()).expect("create golden dir");
        std::fs::write(&bin, bytes).expect("write golden");
        std::fs::write(&hexp, &rendered).expect("write golden hex");
        return bytes.to_vec();
    }
    let want = std::fs::read(&bin)
        .unwrap_or_else(|_| panic!("missing golden {name}; run with LUNIX_REGEN_GOLDEN=1"));
    assert!(
        want == bytes,
        "golden {name} differs: {}",
        first_diff(&want, bytes)
    );
    let want_hex = std::fs::read_to_string(&hexp)
        .unwrap_or_else(|_| panic!("missing golden rendering for {name}"));
    assert_eq!(want_hex, rendered, "golden rendering of {name} differs");
    want
}

/// Like [`golden`] for a text file (`*.txt`, `*.json`).
pub fn golden_text(name: &str, text: &str) {
    let path = golden_dir().join(name);
    if regenerating() {
        std::fs::create_dir_all(golden_dir()).expect("create golden dir");
        std::fs::write(&path, text).expect("write golden text");
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name}; run with LUNIX_REGEN_GOLDEN=1"));
    assert_eq!(want, text, "golden {name} differs");
}

// ---------------------------------------------------------------------------
// Negative suite.

/// A small deterministic generator (xorshift64*), so failures reproduce.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % (n as u64)) as usize
    }
}

/// What a decoder under test does with one input: decode, and on success
/// re-encode. `Ok(bytes)` must equal the input (canonical form).
pub type Roundtrip<'a> = &'a dyn Fn(&[u8]) -> Result<Vec<u8>, FormatError>;

fn check_one(name: &str, rt: Roundtrip<'_>, input: &[u8]) {
    let (res, allocated) = measure(|| rt(input));
    assert!(
        allocated <= alloc_limit(input.len()),
        "{name}: decoding {} bytes allocated {allocated} bytes (limit {})",
        input.len(),
        alloc_limit(input.len())
    );
    if let Ok(again) = res {
        assert!(
            again == input,
            "{name}: accepted a non-canonical input ({})",
            first_diff(input, &again)
        );
    }
}

/// The golden itself must decode and re-encode to itself.
pub fn assert_roundtrip(name: &str, rt: Roundtrip<'_>, golden: &[u8]) {
    let again = rt(golden).unwrap_or_else(|e| panic!("{name}: golden does not decode: {e}"));
    assert!(
        again == golden,
        "{name}: re-encoding differs ({})",
        first_diff(golden, &again)
    );
}

/// Every strict prefix of `golden` fails cleanly (and within the allocation
/// bound).
pub fn all_prefixes_fail(name: &str, rt: Roundtrip<'_>, golden: &[u8]) {
    for n in 0..golden.len() {
        let (res, allocated) = measure(|| rt(&golden[..n]));
        assert!(res.is_err(), "{name}: prefix of {n} bytes decoded");
        assert!(
            allocated <= alloc_limit(n),
            "{name}: prefix of {n} bytes allocated {allocated}"
        );
    }
}

/// [`MUTATIONS`] random mutations of `golden`: none panics, none allocates
/// beyond the bound, and every accepted mutant is its own canonical encoding.
pub fn mutations_are_safe(name: &str, rt: Roundtrip<'_>, golden: &[u8]) {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (golden.len() as u64) << 7);
    let mut buf = golden.to_vec();
    for _ in 0..MUTATIONS {
        buf.clear();
        buf.extend_from_slice(golden);
        let n = 1 + rng.below(3);
        for _ in 0..n {
            if buf.is_empty() {
                break;
            }
            let i = rng.below(buf.len());
            match rng.below(5) {
                0 => buf[i] ^= 1 << rng.below(8),
                1 => buf[i] = rng.next() as u8,
                2 => buf[i] = 0xff,
                3 => buf[i] = 0,
                _ => {
                    // a window of garbage
                    let end = (i + 1 + rng.below(8)).min(buf.len());
                    for b in &mut buf[i..end] {
                        *b = rng.next() as u8;
                    }
                }
            }
        }
        if rng.below(16) == 0 {
            // also change the length
            if rng.below(2) == 0 && !buf.is_empty() {
                buf.truncate(rng.below(buf.len()));
            } else {
                buf.push(rng.next() as u8);
            }
        }
        check_one(name, rt, &buf);
    }
}

/// The whole negative suite for one golden: round trip, prefixes, mutations.
pub fn negative_suite(name: &str, rt: Roundtrip<'_>, golden: &[u8]) {
    assert_roundtrip(name, rt, golden);
    all_prefixes_fail(name, rt, golden);
    mutations_are_safe(name, rt, golden);
}

/// The suite for a format whose prefixes may legitimately decode (a scanner
/// over a stream): round trip and mutations only.
pub fn stream_suite(name: &str, rt: Roundtrip<'_>, golden: &[u8]) {
    assert_roundtrip(name, rt, golden);
    mutations_are_safe(name, rt, golden);
}

/// Asserts that decoding `input` fails.
pub fn assert_rejects(name: &str, rt: Roundtrip<'_>, input: &[u8]) {
    assert!(rt(input).is_err(), "{name}: should have been rejected");
}

// ---------------------------------------------------------------------------
// Reference crypto for tests.

pub fn blake3_256(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

pub fn blake3_keyed(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    *blake3::keyed_hash(key, data).as_bytes()
}

pub fn blake3_128(data: &[u8]) -> [u8; 16] {
    blake3_256(data)[..16].try_into().expect("16 bytes")
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("hmac key");
    m.update(data);
    m.finalize().into_bytes().into()
}

/// `keyed BLAKE3(root, "lunix kdf v1\0" ‖ family ‖ purpose ‖ args)`.
pub fn purpose_key(root: &[u8; 32], msg: &[u8]) -> [u8; 32] {
    blake3_keyed(root, msg)
}

/// One value of `tests/vectors/kat-vectors.json` as bytes.
pub fn kat(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/kat-vectors.json");
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("kat file")).expect("kat json");
    hex(v[name].as_str().unwrap_or_else(|| panic!("kat {name} is not a string")))
}

pub fn kat32(name: &str) -> [u8; 32] {
    kat(name).try_into().expect("32 bytes")
}

pub fn kat16(name: &str) -> [u8; 16] {
    kat(name).try_into().expect("16 bytes")
}

/// The integrity key of the worked examples' volume B: `60 61 .. 7f`.
pub fn volume_b_ik() -> [u8; 32] {
    std::array::from_fn(|i| 0x60 + i as u8)
}

/// Volume B's id data: uuid `b0b1 .. bf` (version nibble 4 as in the doc) and
/// family `50 .. 5f`.
pub const VOLUME_B_UUID: [u8; 16] = [
    0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb5, 0x46, 0xb7, 0x88, 0xb9, 0xba, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf,
];
pub const VOLUME_B_FAMILY: [u8; 16] = [
    0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f,
];
pub const VOLUME_A_UUID: [u8; 16] = [
    0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0x46, 0xa7, 0x88, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae, 0xaf,
];
pub const VOLUME_A_FAMILY: [u8; 16] = [
    0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f,
];
/// 2026-11-12T00:00:00Z in milliseconds.
pub const CREATED_AT: u64 = 1_794_441_600_000;
