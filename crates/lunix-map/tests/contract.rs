//! `RadixMap` (on `MemPacks`) passes the map contract suite at both fanouts.

mod common;

use common::Fix;
use lunix_iface::contract::map::MapFixture;
use lunix_iface::map::ChunkNo;

fn fan512(chunks: u64) -> Fix {
    Fix::new(512, chunks)
}

fn fan256(chunks: u64) -> Fix {
    Fix::new(256, chunks)
}

lunix_iface::contract::map!(contract_512, fan512);
lunix_iface::contract::map!(contract_256, fan256);

#[test]
fn reads_past_the_end_are_invalid() {
    let f = fan512(10);
    assert!(f.map().get(ChunkNo(10)).is_err());
}
