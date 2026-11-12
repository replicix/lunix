//! Map configuration.

use lunix_format::IdLen;

/// Default `map_cache_max`: bytes of encoded leaves the cache may hold.
pub const DEFAULT_MAP_CACHE_MAX: usize = 256 << 20;
/// Default dirty-leaf cap (ADR-8: the overlay cap that forces a seal).
pub const DEFAULT_DIRTY_LEAF_CAP: u64 = 20_000;
/// Default number of leaves the decoded tier may hold.
pub const DEFAULT_DECODED_TIER_LEAVES: usize = 256;

/// What a [`RadixMap`](crate::RadixMap) needs from the volume descriptor and
/// the daemon's configuration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MapConfig {
    /// `map_fanout`: 256 or 512.
    pub fanout: u16,
    /// The volume's id-prefix length.
    pub id_len: IdLen,
    /// `map_cache_max`: budget in bytes for cached encoded leaves (plus the
    /// decoded tier built from them). Interior nodes are resident and not
    /// counted: they are `~1/fanout` of the leaf bytes at most.
    pub map_cache_max: usize,
    /// Dirty leaves after which `MapTxn::set` returns `MapFull` and the engine
    /// must seal. Each dirty leaf is held decoded
    /// (`fanout * size_of::<Entry>()`, 32 KiB at fanout 512), against about
    /// 11 KiB sealed.
    pub dirty_leaf_cap: u64,
    /// Leaves kept decoded after repeated hits; 0 disables the tier.
    pub decoded_tier_leaves: usize,
}

impl MapConfig {
    /// The defaults for `fanout` and `id_len`.
    pub fn new(fanout: u16, id_len: IdLen) -> Self {
        Self {
            fanout,
            id_len,
            map_cache_max: DEFAULT_MAP_CACHE_MAX,
            dirty_leaf_cap: DEFAULT_DIRTY_LEAF_CAP,
            decoded_tier_leaves: DEFAULT_DECODED_TIER_LEAVES,
        }
    }

    /// Sets `map_cache_max`.
    #[must_use]
    pub fn with_cache_max(mut self, bytes: usize) -> Self {
        self.map_cache_max = bytes;
        self
    }

    /// Sets the dirty-leaf cap.
    #[must_use]
    pub fn with_dirty_leaf_cap(mut self, cap: u64) -> Self {
        self.dirty_leaf_cap = cap;
        self
    }

    /// Sets the decoded-tier size in leaves.
    #[must_use]
    pub fn with_decoded_tier(mut self, leaves: usize) -> Self {
        self.decoded_tier_leaves = leaves;
        self
    }
}
