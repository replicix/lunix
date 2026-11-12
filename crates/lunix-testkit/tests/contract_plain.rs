//! `PlainCodec` passes the crypto contract suite, keyed and unkeyed.

use lunix_format::{Codec, Level};
use lunix_iface::contract::crypto::CryptoFixture;
use lunix_iface::crypto::{CommitCodec, ExtentCodec, MetaCodec};
use lunix_iface::store::CommitStore;
use lunix_testkit::{MemStore, PlainCodec};

struct Fix {
    codec: PlainCodec,
    commits: MemStore,
}

impl Fix {
    fn keyed() -> Self {
        Self {
            codec: PlainCodec::new(),
            commits: MemStore::new(),
        }
    }

    fn unkeyed() -> Self {
        Self {
            codec: PlainCodec::unkeyed(),
            commits: MemStore::new(),
        }
    }
}

impl CryptoFixture for Fix {
    fn extents(&self) -> &dyn ExtentCodec {
        &self.codec
    }
    fn meta(&self) -> &dyn MetaCodec {
        &self.codec
    }
    fn commits(&self) -> &dyn CommitCodec {
        &self.codec
    }
    fn store(&self) -> &dyn CommitStore {
        &self.commits
    }
    fn codecs(&self) -> Vec<(Codec, Level)> {
        vec![
            (Codec::Raw, Level(0)),
            (Codec::Lz4, Level(0)),
            (Codec::Zstd, Level(3)),
        ]
    }
    fn compress(&self, codec: Codec, plain: &[u8]) -> Vec<u8> {
        match codec {
            Codec::Lz4 => lz4_flex::block::compress(plain),
            Codec::Zstd => zstd::bulk::compress(plain, 3).expect("zstd"),
            _ => plain.to_vec(),
        }
    }
}

lunix_iface::contract::crypto!(contract_crypto_keyed, Fix::keyed);
lunix_iface::contract::crypto!(contract_crypto_unkeyed, Fix::unkeyed);
