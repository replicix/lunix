//! The store contract suite of `lunix-iface`, run against three volume kinds.

use lunix_format::CryptoMode;

use super::Fixture;

lunix_iface::contract::store!(plain_16k, || Fixture::new(CryptoMode::Plain, 16 * 1024));
lunix_iface::contract::store!(integrity_64k, || Fixture::new(
    CryptoMode::Integrity,
    64 * 1024
));
lunix_iface::contract::store!(encrypted_16k, || Fixture::new(
    CryptoMode::Encrypted,
    16 * 1024
));
