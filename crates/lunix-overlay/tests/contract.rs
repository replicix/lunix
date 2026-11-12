//! The `lunix_iface::contract` overlay suite against the real overlay, with the
//! default options and with tiny stripes and few shards so that every range
//! crosses shards.

use lunix_overlay::{OverlayOptions, ShardedOverlay};

lunix_iface::contract::overlay!(ShardedOverlay::new);

lunix_iface::contract::overlay!(tiny_stripes, |chunk_blocks| {
    ShardedOverlay::with_options(OverlayOptions {
        shards: 3,
        stripe_chunks: 1,
        ..OverlayOptions::new(chunk_blocks)
    })
    .expect("options")
});
