//! The real codecs: output determinism with fresh salts, and key separation.

mod common;

use common::*;
use lunix_iface::map::entry_hint;
use lunix_iface::store::SegmentStore;

const CS: u32 = 16 << 10;

fn raw_extent(r: &Rig, ci: u64) -> Vec<u8> {
    let mut v = Vec::new();
    r.store
        .read_extent(&entry_hint(&r.snapshot()[&ci]), &mut v)
        .unwrap();
    v
}

fn fill(r: &Rig) {
    r.write_chunk(0, &text_bytes(1, CS as usize));
    r.write_chunk(1, &random_bytes(2, CS as usize));
    r.write_chunk(2, &text_bytes(1, CS as usize));
    r.write(3 * CS as usize + BLOCK, &random_bytes(3, 3 * BLOCK));
    r.run_cut(r.last_seq());
}

#[test]
fn same_input_gives_the_same_ids_and_map_and_a_new_ciphertext_per_run() {
    for (name, codec) in codecs(CS) {
        let mk = || {
            let mut c = RigConfig::new(CS, 8);
            c.codec = codec.clone();
            Rig::new(c)
        };
        let (a, b) = (mk(), mk());
        fill(&a);
        fill(&b);
        a.assert_equals(&a.model_at(a.last_seq()));
        assert_eq!(
            a.snapshot(),
            b.snapshot(),
            "{name}: ids, hints and clens equal"
        );
        let (ea, eb) = (raw_extent(&a, 0), raw_extent(&b, 0));
        assert_eq!(ea.len(), eb.len());
        let salted = name.contains("encrypted");
        assert_eq!(
            ea != eb,
            salted,
            "{name}: a fresh salt per seal makes encrypted bytes differ"
        );
        // The id is in the header, whatever the mode.
        assert_eq!(&ea[16..48], &eb[16..48]);
    }
}

#[test]
fn volumes_with_different_keys_have_different_ids_for_identical_data() {
    for flavour in ["real integrity", "real encrypted"] {
        let data = text_bytes(9, CS as usize);
        let mut prefixes = Vec::new();
        for _ in 0..2 {
            let (_, codec) = codecs(CS).into_iter().find(|(n, _)| *n == flavour).unwrap();
            let mut c = RigConfig::new(CS, 4);
            c.codec = codec;
            let r = Rig::new(c);
            r.write_chunk(0, &data);
            r.run_cut(r.last_seq());
            prefixes.push(r.snapshot()[&0].prefix);
        }
        assert_ne!(
            prefixes[0], prefixes[1],
            "{flavour}: independent random roots"
        );
    }
}
