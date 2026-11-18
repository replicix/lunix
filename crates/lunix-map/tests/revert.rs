//! A walk of an older root reports that root's own stored nodes, also when
//! the tip holds a node of the same hash stored elsewhere (the content went
//! `X -> Y -> X` and was sealed again). An interior node's hash covers its
//! child hashes, not their locations, so the tip's resident copy may not
//! stand in for the older root's (M17.B stack review B-1: GC would miss the
//! older root's pack and delete it).

mod common;

use common::{set_all, Fix};
use lunix_format::commit::MapRoot;
use lunix_iface::map::{BlockMap, NodeRef};

fn nodes(map: &dyn BlockMap, root: &MapRoot) -> Vec<NodeRef> {
    let mut out = Vec::new();
    map.walk_nodes(root, &mut |n| out.push(*n)).expect("walk");
    out
}

#[test]
fn a_reverted_subtree_walks_with_the_older_roots_own_locations() {
    let chunks = 256 * 4;
    let f = Fix::new(256, chunks);
    set_all(&f.map, [0], 1);
    let _r1 = f.seal().root;
    set_all(&f.map, [256], 1);
    let r2 = f.seal().root;
    // Leaf 0 changes, then goes back to what it was: sealed again elsewhere.
    set_all(&f.map, [0], 2);
    let _r3 = f.seal().root;
    set_all(&f.map, [0], 1);
    let r4 = f.seal().root;
    assert_eq!(r2.hash, r4.hash, "same content, same root hash");
    assert_ne!(r2.pack, r4.pack, "stored twice");

    let fresh = f.open_at(&r2, chunks);
    let from_disk = nodes(&fresh, &r2);
    let live = nodes(&f.map, &r2);
    assert_eq!(
        live, from_disk,
        "the live map reports the older root's own node locations"
    );
    // And the tip's own walk is its own.
    assert_eq!(nodes(&f.map, &r4), nodes(&f.open_at(&r4, chunks), &r4));
}
