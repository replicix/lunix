//! Key types lack `Debug`, `Clone` and `Copy` (compile-time checks; the
//! doctests in `secret.rs` add `compile_fail` forms). A line in this file
//! stops compiling the day one of those traits is derived.

use lunix_crypto::{
    AddressingKey, Dek, Ik, KeyFile, Kmk, MacKey, MetaKey, Root, Secret, TagKey, VolumeKeys,
};

/// Fails to compile if `$t: $tr`: the ambiguity trick of `static_assertions`.
macro_rules! assert_not_impl {
    ($t:ty: $($tr:path),+ $(,)?) => {
        $(
            const _: fn() = || {
                trait AmbiguousIfImpl<A> {
                    fn some_item() {}
                }
                impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
                impl<T: ?Sized + $tr> AmbiguousIfImpl<u8> for T {}
                let _ = <$t as AmbiguousIfImpl<_>>::some_item;
            };
        )+
    };
}

assert_not_impl!(Secret: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(Kmk: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(Ik: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(Dek: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(MetaKey: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(AddressingKey: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(MacKey: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(TagKey: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(Root: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(KeyFile: std::fmt::Debug, std::fmt::Display, Clone, Copy);
assert_not_impl!(VolumeKeys: std::fmt::Debug, std::fmt::Display, Clone, Copy);

/// The one key-ish type that may be duplicated is the opaque handle of
/// `lunix-iface`, which holds an `Arc` and prints only its mode.
#[test]
fn the_opaque_handle_is_cloneable_and_redacted() {
    fn is_clone<T: Clone>() {}
    is_clone::<lunix_iface::crypto::Keys>();
}
