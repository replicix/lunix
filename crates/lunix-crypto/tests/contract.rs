//! The crypto contract suite of `lunix-iface`, run against the real codecs of
//! all three volume modes.

mod common;

use common::*;
use lunix_format::{Codec, CryptoMode, Level};
use lunix_iface::contract::crypto::CryptoFixture;
use lunix_iface::crypto::{CommitCodec, ExtentCodec, MetaCodec};
use lunix_iface::store::CommitStore;

struct Fix {
    vol: Vol,
    store: MemCommits,
}

impl Fix {
    fn new(mode: CryptoMode) -> Self {
        Self {
            vol: volume(mode, 16_384, 3),
            store: MemCommits::new(),
        }
    }
}

impl CryptoFixture for Fix {
    fn extents(&self) -> &dyn ExtentCodec {
        &**self.vol.crypto.extents()
    }

    fn meta(&self) -> &dyn MetaCodec {
        &**self.vol.crypto.meta()
    }

    fn commits(&self) -> &dyn CommitCodec {
        &**self.vol.crypto.meta()
    }

    fn store(&self) -> &dyn CommitStore {
        &self.store
    }

    fn codecs(&self) -> Vec<(Codec, Level)> {
        vec![
            (Codec::Raw, Level(0)),
            (Codec::Lz4, Level(0)),
            (Codec::Zstd, Level(3)),
        ]
    }

    fn compress(&self, codec: Codec, plain: &[u8]) -> Vec<u8> {
        compress(codec, plain)
    }
}

lunix_iface::contract::crypto!(encrypted, || Fix::new(CryptoMode::Encrypted));
lunix_iface::contract::crypto!(integrity, || Fix::new(CryptoMode::Integrity));
lunix_iface::contract::crypto!(plain, || Fix::new(CryptoMode::Plain));
