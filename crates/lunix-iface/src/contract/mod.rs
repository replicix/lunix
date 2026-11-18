//! Contract-test suites that every implementation of a trait runs, fake and
//! real alike (M1.B1 step 11).
//!
//! Each suite is a macro taking a constructor and expanding to a module of
//! `#[test]` functions:
//!
//! ```ignore
//! lunix_iface::contract::overlay!(|chunk_blocks| MyOverlay::new(chunk_blocks));
//! lunix_iface::contract::journal!(my_journal_tests, || MyJournalFixture::new());
//! ```
//!
//! The optional leading identifier names the generated module (default
//! `contract_<trait>`), so one file can run a suite twice. The test bodies are
//! ordinary functions in the submodules below (`overlay::newest_write_wins`
//! and so on); the macros only wire the constructor, so the logic is compiled,
//! linted and documented like the rest of the crate.
//!
//! A suite checks what the trait documents and no more: ordering, the
//! non-blocking versus blocking split where it is observable, error classes,
//! and the crash-model invariants (what a flush guarantees, what recovery may
//! return). Behaviour a trait leaves to the implementation is not asserted.
//! The crypto suite needs a fixture that can compress for the codecs it offers.

pub mod crypto;
pub mod index;
pub mod journal;
pub mod map;
pub mod overlay;
pub mod space;
pub mod store;

use std::time::{Duration, Instant};

/// Polls `cond` every millisecond until it holds or `timeout` passes.
///
/// # Panics
/// With `what` when the condition never holds: a contract test failing.
#[track_caller]
pub fn wait_until(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Runs the crypto contract suite: `crypto!([name,] || fixture)`.
#[macro_export]
#[doc(hidden)]
macro_rules! contract_crypto_suite {
    ($make:expr) => {
        $crate::contract_crypto_suite!(contract_crypto, $make);
    };
    ($m:ident, $make:expr) => {
        mod $m {
            use super::*;
            $crate::__contract_tests!(
                crypto,
                $make,
                [
                    extent_round_trip_per_codec,
                    any_flipped_byte_fails,
                    wrong_expectations_fail,
                    chunk_id_is_stable_across_seals,
                    meta_objects_are_bound_to_kind_and_name,
                    commit_round_trips_and_is_bound_to_its_seq,
                    verify_chain_accepts_good_and_rejects_broken,
                    verify_chain_rejects_a_decreasing_epoch,
                    commit_parent_hash_binds_the_parent_object,
                ]
            );
        }
    };
}

/// Runs the journal contract suite: `journal!([name,] || fixture)`.
#[macro_export]
#[doc(hidden)]
macro_rules! contract_journal_suite {
    ($make:expr) => {
        $crate::contract_journal_suite!(contract_journal, $make);
    };
    ($m:ident, $make:expr) => {
        mod $m {
            use super::*;
            $crate::__contract_tests!(
                journal,
                $make,
                [
                    empty_journal_recovers_nothing,
                    appended_records_complete_and_read_back,
                    holes_carry_no_data,
                    watermark_follows_completions,
                    group_flush_makes_completed_records_durable,
                    flush_waits_for_an_outstanding_slot,
                    failed_append_poisons_the_journal,
                    a_dropped_slot_is_detected_and_poisons,
                    a_foreign_slot_is_refused,
                    recover_filters_by_floor,
                    recover_is_idempotent,
                    recovery_after_crash_is_a_contiguous_prefix,
                    reclaim_keeps_records_above_the_tail,
                ]
            );
        }
    };
}

/// Runs the overlay contract suite: `overlay!([name,] |chunk_blocks| overlay)`.
#[macro_export]
#[doc(hidden)]
macro_rules! contract_overlay_suite {
    ($make:expr) => {
        $crate::contract_overlay_suite!(contract_overlay, $make);
    };
    ($m:ident, $make:expr) => {
        mod $m {
            use super::*;
            $crate::__contract_tests_chunks!(
                overlay,
                $make,
                [
                    newest_write_wins,
                    lookup_tiles_the_range,
                    hole_hides_data,
                    cut_is_frozen,
                    cut_keeps_versions_shadowed_by_newer_writes,
                    cut_groups_by_chunk,
                    release_keeps_later_overwrites,
                    dirty_bytes_follow_inserts,
                ]
            );
        }
    };
}

/// Runs the map contract suite: `map!([name,] |chunk_count| fixture)`.
#[macro_export]
#[doc(hidden)]
macro_rules! contract_map_suite {
    ($make:expr) => {
        $crate::contract_map_suite!(contract_map, $make);
    };
    ($m:ident, $make:expr) => {
        mod $m {
            use super::*;
            $crate::__contract_tests_count!(
                map,
                $make,
                [
                    empty_map_has_only_holes,
                    transaction_is_atomic_and_exclusive,
                    seal_and_reopen_preserve_content,
                    root_is_independent_of_update_order,
                    rehint_applies_only_to_the_old_hint,
                    walks_agree_with_content,
                    grow_keeps_content,
                ]
            );
        }
    };
}

/// Runs the index contract suite: `index!([name,] || fixture)`.
#[macro_export]
#[doc(hidden)]
macro_rules! contract_index_suite {
    ($make:expr) => {
        $crate::contract_index_suite!(contract_index, $make);
    };
    ($m:ident, $make:expr) => {
        mod $m {
            use super::*;
            $crate::__contract_tests!(
                index,
                $make,
                [
                    insert_then_dedup_hit_is_verified,
                    prefix_lookup_returns_full_ids,
                    condemned_entries_never_dedup,
                    relocate_follows_newest_wins,
                    persisted_hits_are_hinted,
                    relocated_entries_wait_for_their_segment,
                    all_runs_lists_every_run_file,
                    drop_segment_removes_entries,
                    catch_up_ingests_only_named_segments,
                    rebuild_equals_incremental,
                ]
            );
        }
    };
}

/// Runs the store contract suite: `store!([name,] || fixture)`.
#[macro_export]
#[doc(hidden)]
macro_rules! contract_store_suite {
    ($make:expr) => {
        $crate::contract_store_suite!(contract_store, $make);
    };
    ($m:ident, $make:expr) => {
        mod $m {
            use super::*;
            $crate::__contract_tests!(
                store,
                $make,
                [
                    segment_round_trip,
                    abandoned_segment_is_not_listed,
                    segment_delete_is_idempotent,
                    ids_are_never_reused,
                    pack_round_trip,
                    commits_are_create_only,
                    commit_delete_respects_floor_and_tip,
                    set_aside_moves_only_the_tip,
                    checkpoint_is_a_double_slot_hint,
                    evidence_is_create_only,
                    evidence_can_be_deleted,
                    condemned_list_round_trips,
                    sweep_deletes_only_unnamed_orphans_above_the_mark,
                    rollback_floor_is_raise_only,
                    reset_rollback_floor_is_refused_without_the_option,
                ]
            );
        }
    };
}

/// Runs the space contract suite: `space!([name,] |setup, clock| gate)`.
#[macro_export]
#[doc(hidden)]
macro_rules! contract_space_suite {
    ($make:expr) => {
        $crate::contract_space_suite!(contract_space, $make);
    };
    ($m:ident, $make:expr) => {
        mod $m {
            use super::*;
            $crate::__contract_tests_space!(
                space,
                $make,
                [
                    reserve_and_release_are_exact,
                    transfer_moves_between_kinds,
                    error_mode_refuses_without_state_change,
                    wait_mode_parks_and_grants_fifo,
                    timeout_expires_waiters_on_the_clock,
                    headroom_is_not_for_writes,
                    fatal_io_makes_the_volume_read_only,
                    ladder_frees_space_for_a_parked_write,
                ]
            );
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __contract_tests {
    ($suite:ident, $make:expr, [$($name:ident),* $(,)?]) => {
        $(
            #[test]
            fn $name() {
                #[allow(clippy::redundant_closure_call)]
                let mut fixture = ($make)();
                $crate::contract::$suite::$name(&mut fixture);
            }
        )*
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __contract_tests_chunks {
    ($suite:ident, $make:expr, [$($name:ident),* $(,)?]) => {
        $(
            #[test]
            fn $name() {
                for chunk_blocks in [4u64, 16] {
                    #[allow(clippy::redundant_closure_call)]
                    let overlay = ($make)(chunk_blocks);
                    $crate::contract::$suite::$name(&overlay, chunk_blocks);
                }
            }
        )*
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __contract_tests_count {
    ($suite:ident, $make:expr, [$($name:ident),* $(,)?]) => {
        $(
            #[test]
            fn $name() {
                #[allow(clippy::redundant_closure_call)]
                let make = |chunks: u64| ($make)(chunks);
                $crate::contract::$suite::$name(&make);
            }
        )*
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __contract_tests_space {
    ($suite:ident, $make:expr, [$($name:ident),* $(,)?]) => {
        $(
            #[test]
            fn $name() {
                #[allow(clippy::redundant_closure_call)]
                let make = |setup: $crate::contract::space::SpaceSetup,
                            clock: ::std::sync::Arc<$crate::clock::ManualClock>| {
                    ($make)(setup, clock)
                };
                $crate::contract::$suite::$name(&make);
            }
        )*
    };
}

pub use crate::contract_crypto_suite as crypto;
pub use crate::contract_index_suite as index;
pub use crate::contract_journal_suite as journal;
pub use crate::contract_map_suite as map;
pub use crate::contract_overlay_suite as overlay;
pub use crate::contract_space_suite as space;
pub use crate::contract_store_suite as store;
