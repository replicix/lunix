//! Constant-time comparison of secrets, MACs and tags.

use aws_lc_rs::constant_time::verify_slices_are_equal;

/// True if `a` and `b` have the same length and contents. Takes time that
/// depends only on the lengths.
pub(crate) fn eq(a: &[u8], b: &[u8]) -> bool {
    verify_slices_are_equal(a, b).is_ok()
}
