//! Truncation and bit-flip sweeps over sealed containers: every damage is a
//! typed error, never a panic and never data.

use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use lunix_format::segment::TRAILER_LEN;
use lunix_format::CryptoMode;
use lunix_iface::store::{PackRead, PackStore, SegmentStore};
use lunix_iface::ErrorKind;

use super::codec::open_extent;
use super::{cid, create, data_extent, node_extent, open, tempdir, test_opts};
use crate::LocalVolume;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

struct Sealed {
    dir: tempfile::TempDir,
    path: PathBuf,
    original: Vec<u8>,
    id: lunix_format::SegId,
    first: lunix_format::Hint,
    mode: CryptoMode,
}

fn sealed_segment(mode: CryptoMode, n: u32) -> Sealed {
    let dir = tempdir();
    let vol = create(dir.path(), mode, 16 * 1024, test_opts());
    let mut w = vol.open_segment().expect("open");
    let mut first = None;
    for i in 0..n {
        let id = cid(i);
        let e = data_extent(mode, 16 * 1024, &id, 200 + i as usize % 50);
        let h = w.append_extent(&id, None, &e).expect("append");
        first.get_or_insert(h);
    }
    let id = w.id();
    w.seal(None).expect("seal");
    drop(vol);
    let path = dir
        .path()
        .join(lunix_format::paths::local::segment_sealed(id));
    let original = std::fs::read(&path).expect("read back");
    Sealed {
        dir,
        path,
        original,
        id,
        first: first.expect("one extent"),
        mode,
    }
}

impl Sealed {
    fn reopen(&self) -> LocalVolume {
        open(self.dir.path(), self.mode, test_opts()).expect("reopen")
    }

    fn write(&self, bytes: &[u8]) {
        std::fs::write(&self.path, bytes).expect("write");
    }

    /// Everything a caller can do with the segment fails with `Corrupt`/`Auth`
    /// (or the damaged extent is caught by the tag), and nothing panics.
    fn assert_detected(&self, what: &str) {
        let vol = self.reopen();
        let footer = vol.footer(self.id);
        let verify = vol.verify_segment(self.id);
        let mut out = Vec::new();
        let read = vol.read_extent(&self.first, &mut out);
        let tag_ok = read.is_ok() && open_extent(self.mode, &out).is_ok();
        let detected = footer.is_err() || verify.is_err() || !tag_ok;
        assert!(detected, "{what}: damage went unnoticed");
        for r in [footer.err(), verify.err()].into_iter().flatten() {
            assert!(
                matches!(r.kind(), ErrorKind::Corrupt | ErrorKind::Auth),
                "{what}: unexpected error {r}"
            );
        }
    }
}

#[test]
fn a_truncated_segment_fails_to_open_at_every_4k_boundary_and_100_random_offsets() {
    for mode in [CryptoMode::Plain, CryptoMode::Encrypted] {
        let s = sealed_segment(mode, 60);
        let size = s.original.len();
        assert!(size > 3 * 4096, "the sweep needs several 4 KiB blocks");
        let mut cuts: Vec<usize> = (0..size).step_by(4096).collect();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        cuts.extend((0..100).map(|_| (rng.next() % size as u64) as usize));
        cuts.extend([size - 1, size - TRAILER_LEN, size - TRAILER_LEN - 1]);
        for cut in cuts {
            s.write(&s.original[..cut]);
            let vol = s.reopen();
            let e = vol
                .footer(s.id)
                .expect_err("truncated segment must not open");
            assert_eq!(e.kind(), ErrorKind::Corrupt, "{mode:?} cut at {cut}");
            let mut out = Vec::new();
            let e = vol.read_extent(&s.first, &mut out).expect_err("read");
            assert_eq!(e.kind(), ErrorKind::Corrupt, "{mode:?} cut at {cut}");
            assert!(out.is_empty());
            assert_eq!(
                vol.verify_segment(s.id).expect_err("verify").kind(),
                ErrorKind::Corrupt
            );
        }
    }
}

#[test]
fn a_flipped_byte_in_trailer_footer_or_extents_is_detected() {
    for mode in [CryptoMode::Plain, CryptoMode::Encrypted] {
        let s = sealed_segment(mode, 20);
        let size = s.original.len();
        let footer_off =
            u64::from_le_bytes(s.original[size - 64..size - 56].try_into().unwrap()) as usize;
        let ovh = mode.extent_ovh() as usize;
        // Every byte of the trailer and of the footer envelope, and every
        // byte of the first two extents (header, id, clen, payload, tag).
        let mut offsets: Vec<usize> = (footer_off..size).collect();
        offsets.extend(0..2 * (ovh + 260));
        // Plus a spread through the rest of the extent region.
        offsets.extend((0..footer_off).step_by(97));
        for off in offsets {
            let mut damaged = s.original.clone();
            damaged[off] ^= 0x40;
            s.write(&damaged);
            s.assert_detected(&format!("{mode:?} byte {off}"));
        }
        // And a flipped bit in the middle of a payload: the store returns the
        // bytes, the codec's tag refuses them.
        let mut damaged = s.original.clone();
        damaged[s.first.off as usize + ovh - 16 + 3] ^= 1;
        s.write(&damaged);
        let vol = s.reopen();
        let mut out = Vec::new();
        vol.read_extent(&s.first, &mut out)
            .expect("bytes come back");
        assert_eq!(
            open_extent(mode, &out).expect_err("tag").kind(),
            ErrorKind::Auth
        );
    }
}

#[test]
fn a_flipped_payload_byte_on_a_plain_volume_is_an_error_not_data() {
    let s = sealed_segment(CryptoMode::Plain, 3);
    let mut damaged = s.original.clone();
    damaged[s.first.off as usize + 60] ^= 0xff;
    s.write(&damaged);
    let vol = s.reopen();
    let mut out = Vec::new();
    vol.read_extent(&s.first, &mut out).expect("read");
    assert!(open_extent(CryptoMode::Plain, &out).is_err());
}

#[test]
fn a_hint_that_does_not_point_at_an_extent_is_corrupt() {
    let s = sealed_segment(CryptoMode::Plain, 5);
    let vol = s.reopen();
    let mut out = Vec::new();
    let mut h = s.first;
    h.off += 7;
    assert_eq!(
        vol.read_extent(&h, &mut out)
            .expect_err("misaligned")
            .kind(),
        ErrorKind::Corrupt
    );
    let mut h = s.first;
    h.off = u32::MAX - 10;
    assert_eq!(
        vol.read_extent(&h, &mut out).expect_err("outside").kind(),
        ErrorKind::Corrupt
    );
    let mut h = s.first;
    h.clen = 0;
    assert_eq!(
        vol.read_extent(&h, &mut out).expect_err("empty").kind(),
        ErrorKind::Corrupt
    );
    assert_eq!(
        vol.locate(&h).expect_err("locate").kind(),
        ErrorKind::Corrupt
    );
    assert!(out.is_empty());
}

#[test]
fn damaged_map_packs_are_detected_too() {
    let mode = CryptoMode::Integrity;
    let dir = tempdir();
    let vol = create(dir.path(), mode, 16 * 1024, test_opts());
    let mut p = vol.open_pack().expect("pack");
    let mut first = None;
    for i in 0..10 {
        let h = cid(i).0;
        let loc = p.append_node(&h, &node_extent(mode, &h, 90)).expect("node");
        first.get_or_insert(loc);
    }
    let pack = p.id();
    p.seal().expect("seal");
    drop(vol);
    let path = dir
        .path()
        .join(lunix_format::paths::local::map_pack_sealed(pack));
    let original = std::fs::read(&path).expect("read");
    for cut in (0..original.len()).step_by(61) {
        std::fs::write(&path, &original[..cut]).expect("truncate");
        let vol = open(dir.path(), mode, test_opts()).expect("open");
        assert_eq!(
            vol.pack_footer(pack).expect_err("cut").kind(),
            ErrorKind::Corrupt
        );
    }
    for off in (0..original.len()).step_by(7) {
        let mut d = original.clone();
        d[off] ^= 1;
        let f = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open file");
        f.write_all_at(&d, 0).expect("write");
        let vol = open(dir.path(), mode, test_opts()).expect("open");
        let mut out = Vec::new();
        let read = vol.read_node(first.expect("loc"), &mut out);
        let ok = vol.pack_footer(pack).is_ok()
            && vol.verify_pack(pack).is_ok()
            && read.is_ok()
            && open_extent(mode, &out).is_ok();
        assert!(!ok, "byte {off}: damage went unnoticed");
    }
}
