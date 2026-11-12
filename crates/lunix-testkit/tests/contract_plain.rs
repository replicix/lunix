//! `PlainCodec` passes the crypto contract suite, keyed and unkeyed.

mod common;

use common::TestCommits;
use lunix_format::{Codec, Level};
use lunix_iface::contract::crypto::CryptoFixture;
use lunix_iface::crypto::{CommitCodec, ExtentCodec, MetaCodec};
use lunix_iface::store::CommitStore;
use lunix_testkit::PlainCodec;

struct Fix {
    codec: PlainCodec,
    commits: TestCommits,
}

impl Fix {
    fn keyed() -> Self {
        Self {
            codec: PlainCodec::new(),
            commits: TestCommits::default(),
        }
    }

    fn unkeyed() -> Self {
        Self {
            codec: PlainCodec::unkeyed(),
            commits: TestCommits::default(),
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
        vec![(Codec::Raw, Level(0))]
    }
    fn compress(&self, _codec: Codec, plain: &[u8]) -> Vec<u8> {
        plain.to_vec()
    }
}

lunix_iface::contract::crypto!(contract_crypto_keyed, Fix::keyed);
lunix_iface::contract::crypto!(contract_crypto_unkeyed, Fix::unkeyed);
