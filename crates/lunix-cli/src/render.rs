//! Human-readable output of the responses.

use std::io::{self, Write};

use lunix_format::local::FreeFloor;
use lunix_format::local::OnFull;
use lunix_iface::ctl::{
    Attached, GcPressure, SpaceBody, SpaceFigures, ThrottleStatus, VolumeCreated, VolumeMode,
    VolumeState, VolumeStatusBody, VolumeSummary,
};
use lunix_iface::gc::{GcPassSummary, GcState, GcStatusSnapshot};
use lunix_iface::space::ThrottleState;

use crate::error::sanitize;
use crate::parse::{format_bytes, format_duration_ms, format_utc_ms};

/// The lowercase name of a mode.
pub fn mode_name(m: VolumeMode) -> &'static str {
    match m {
        VolumeMode::Encrypted => "encrypted",
        VolumeMode::Integrity => "integrity",
        VolumeMode::Plain => "plain",
    }
}

fn state_name(s: VolumeState) -> &'static str {
    match s {
        VolumeState::Detached => "detached",
        VolumeState::Attached => "attached",
        VolumeState::Recovering => "recovering",
        VolumeState::ReadOnly => "read-only",
    }
}

fn gc_state_name(s: GcState) -> &'static str {
    match s {
        GcState::Idle => "idle",
        GcState::Marking => "marking",
        GcState::Condemning => "condemning",
        GcState::Grace => "grace",
        GcState::Rechecking => "rechecking",
        GcState::Deleting => "deleting",
        GcState::Compacting => "compacting",
    }
}

fn kv(out: &mut dyn Write, key: &str, value: &str) -> io::Result<()> {
    writeln!(out, "{key:<17}{}", sanitize(value))
}

fn size_with_bytes(n: u64) -> String {
    if n < 1024 {
        format_bytes(n)
    } else {
        format!("{} ({n} bytes)", format_bytes(n))
    }
}

/// `volume create` result.
pub fn created(out: &mut dyn Write, v: &VolumeCreated) -> io::Result<()> {
    writeln!(
        out,
        "created volume {} ({}, uuid {})",
        sanitize(&v.name),
        mode_name(v.mode),
        sanitize(&v.uuid)
    )?;
    kv(out, "directory", &v.dir)?;
    if let Some(k) = &v.key_file {
        kv(out, "key file", k)?;
        writeln!(
            out,
            "Keep a copy of the key file somewhere safe: attach reads it, and the \
             volume cannot be attached without it."
        )?;
    }
    Ok(())
}

/// `volume list` result.
pub fn volume_list(out: &mut dyn Write, vols: &[VolumeSummary]) -> io::Result<()> {
    if vols.is_empty() {
        return writeln!(out, "no volumes");
    }
    let rows: Vec<[String; 5]> = vols
        .iter()
        .map(|v| {
            [
                sanitize(&v.name),
                mode_name(v.mode).to_owned(),
                if v.size_bytes == 0 {
                    "-".to_owned()
                } else {
                    format_bytes(v.size_bytes)
                },
                v.device.as_deref().map_or_else(|| "-".to_owned(), sanitize),
                sanitize(&v.dir),
            ]
        })
        .collect();
    let head = ["NAME", "MODE", "SIZE", "DEVICE", "DIRECTORY"];
    let mut w = head.map(str::len);
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let line = |out: &mut dyn Write, r: &[String; 5]| -> io::Result<()> {
        writeln!(
            out,
            "{:<w0$}  {:<w1$}  {:>w2$}  {:<w3$}  {}",
            r[0],
            r[1],
            r[2],
            r[3],
            r[4],
            w0 = w[0],
            w1 = w[1],
            w2 = w[2],
            w3 = w[3]
        )
    };
    line(out, &head.map(str::to_owned))?;
    for r in &rows {
        line(out, r)?;
    }
    Ok(())
}

/// `attach` result.
pub fn attached(out: &mut dyn Write, name: &str, a: &Attached) -> io::Result<()> {
    writeln!(
        out,
        "attached {} as {} ({} queues, depth {}{})",
        sanitize(name),
        sanitize(&a.device),
        a.queues,
        a.depth,
        if a.recovered {
            ", recovered an existing device"
        } else {
            ""
        }
    )
}

/// The note printed on stderr after the first attach on a host.
pub const FIRST_ATTACH_NOTE: &str = "\
note: this host had no rollback floor for this volume. A rollback of its history
before this attach cannot be detected; the floor is recorded now, and later
rollbacks are refused.";

/// `status` result for one volume.
pub fn status(out: &mut dyn Write, s: &VolumeStatusBody) -> io::Result<()> {
    kv(out, "volume", &format!("{} (uuid {})", s.name, s.uuid))?;
    kv(out, "state", state_name(s.state))?;
    kv(out, "mode", mode_name(s.mode))?;
    kv(
        out,
        "size",
        &if s.size_bytes == 0 {
            "unknown (sealed in the commits)".to_owned()
        } else {
            size_with_bytes(s.size_bytes)
        },
    )?;
    kv(out, "chunk size", &format_bytes(u64::from(s.chunk_size)))?;
    kv(out, "device", s.device.as_deref().unwrap_or("-"))?;
    let last = match s.last_commit_ms {
        Some(ms) => format!("seq {}, {}", s.last_commit_seq, format_utc_ms(ms)),
        None => format!("seq {}", s.last_commit_seq),
    };
    kv(out, "last commit", &last)?;
    kv(
        out,
        "rollback floor",
        &match &s.rollback_floor {
            Some(f) => format!("seq {}, tip {}", f.max_commit_seq, f.tip_hash),
            None => "none recorded on this host".to_owned(),
        },
    )?;
    kv(
        out,
        "journal",
        &format!(
            "{} of {} used",
            format_bytes(s.journal_used),
            format_bytes(s.journal_capacity)
        ),
    )?;
    let ratio = |r: Option<f64>| r.map_or_else(|| "n/a".to_owned(), |r| format!("{r:.2}"));
    kv(out, "dedup ratio", &ratio(s.dedup_ratio))?;
    kv(out, "compression", &ratio(s.compression_ratio))?;
    let st = &s.stats;
    kv(
        out,
        "reads",
        &format!("{} ops, {}", st.ops_read, format_bytes(st.bytes_read)),
    )?;
    kv(
        out,
        "writes",
        &format!("{} ops, {}", st.ops_write, format_bytes(st.bytes_written)),
    )?;
    kv(
        out,
        "other ops",
        &format!(
            "{} flush, {} discard, {} write-zeroes",
            st.ops_flush, st.ops_discard, st.ops_write_zeroes
        ),
    )?;
    kv(
        out,
        "chunks",
        &format!(
            "{} total, {} zero, {} dedup hits",
            st.chunks_total, st.chunks_zero, st.dedup_hits
        ),
    )?;
    kv(out, "commits", &st.commits.to_string())?;
    if st.extent_auth_failures > 0 {
        kv(
            out,
            "AUTH FAILURES",
            &format!("{} extents failed verification", st.extent_auth_failures),
        )?;
    }
    if st.read_revalidated > 0 || st.read_revalidate_exhausted > 0 {
        kv(
            out,
            "read revalidations",
            &format!(
                "{} retried after a stale entry, {} gave up (EIO)",
                st.read_revalidated, st.read_revalidate_exhausted
            ),
        )?;
    }
    figures(out, &s.space)?;
    throttle(out, &s.throttle)
}

/// Milliseconds for the throttle line: `850 ms`, `42 s`, `2.4 s`; the long
/// ones in the shared `1m`, `2h` form.
fn human_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms >= 100_000 {
        format_duration_ms(ms)
    } else if ms.is_multiple_of(1_000) {
        format!("{} s", ms / 1_000)
    } else {
        format!("{:.1} s", ms as f64 / 1_000.0)
    }
}

/// The write throttle (ADR-32): one line when the state is not `open`, for
/// example `slowdown for 42 s, 38 MiB/s, headroom 3.1 GiB of 48 GiB, 17
/// writes waiting, longest wait 2.4 s`; an open volume that was throttled
/// since the attach says how much.
fn throttle(out: &mut dyn Write, t: &ThrottleStatus) -> io::Result<()> {
    if t.state == ThrottleState::Open {
        // The store pace (P1') can pace while the raw headroom is above the
        // soft mark: the state is open, the rate is not empty.
        if let Some(r) = t.rate {
            kv(
                out,
                "throttle",
                &format!(
                    "open, paced above soft, {}/s, headroom {} of {}, longest wait {}",
                    format_bytes(r),
                    format_bytes(t.headroom),
                    format_bytes(t.limit),
                    human_ms(t.delay_max_ms)
                ),
            )?;
            return Ok(());
        }
        if t.throttled_ms > 0 || t.delayed_writes > 0 || t.enospc > 0 {
            kv(
                out,
                "throttle",
                &format!(
                    "open; since the attach {} throttled, {} writes delayed (longest {}), {} refused with ENOSPC",
                    human_ms(t.throttled_ms),
                    t.delayed_writes,
                    human_ms(t.delay_max_ms),
                    t.enospc
                ),
            )?;
        }
        return Ok(());
    }
    let mut line = format!("{} for {}", t.state.name(), human_ms(t.state_for_ms));
    if let Some(r) = t.rate {
        line.push_str(&format!(", {}/s", format_bytes(r)));
    }
    line.push_str(&format!(
        ", headroom {} of {}",
        format_bytes(t.headroom),
        format_bytes(t.limit)
    ));
    let waiting = t.paced_waiters + t.space_waiters;
    if waiting > 0 {
        line.push_str(&format!(", {waiting} writes waiting"));
    }
    line.push_str(&format!(", longest wait {}", human_ms(t.delay_max_ms)));
    if t.gc_pressure != GcPressure::Normal {
        line.push_str(&format!(
            ", gc {}",
            match t.gc_pressure {
                GcPressure::Normal => "normal",
                GcPressure::Raised => "raised",
                GcPressure::Urgent => "urgent",
            }
        ));
    }
    kv(out, "throttle", &line)
}

fn figures(out: &mut dyn Write, f: &SpaceFigures) -> io::Result<()> {
    kv(
        out,
        "space used",
        &format!("{} of {}", format_bytes(f.used), format_bytes(f.limit)),
    )?;
    kv(out, "host free", &format_bytes(f.fs_free))?;
    kv(
        out,
        "space by kind",
        &format!(
            "segments {}, cache {}, map {}, index {}, commits {}, journal {}, headroom {}",
            format_bytes(f.segments),
            format_bytes(f.cache),
            format_bytes(f.map),
            format_bytes(f.index),
            format_bytes(f.commits),
            format_bytes(f.journal),
            format_bytes(f.headroom_held)
        ),
    )?;
    if f.waiters > 0 {
        kv(out, "waiting writers", &f.waiters.to_string())?;
    }
    Ok(())
}

/// `space` result.
pub fn space(out: &mut dyn Write, name: &str, b: &SpaceBody) -> io::Result<()> {
    kv(out, "volume", name)?;
    kv(
        out,
        "floor",
        &match &b.settings.free_floor {
            FreeFloor::Bytes(n) => format_bytes(*n),
            FreeFloor::Percent(p) => p.clone(),
        },
    )?;
    kv(
        out,
        "on full",
        match b.settings.on_full {
            OnFull::Wait => "wait",
            OnFull::Error => "error",
        },
    )?;
    kv(
        out,
        "full timeout",
        &b.settings
            .full_timeout_ms
            .map_or_else(|| "none".to_owned(), format_duration_ms),
    )?;
    let cap = match (b.settings.cache_max_percent, b.settings.cache_max) {
        (Some(p), _) => format!("{p} % of the size"),
        (None, Some(c)) => format_bytes(c),
        (None, None) => "none".to_owned(),
    };
    kv(out, "cache max", &cap)?;
    kv(out, "headroom", &format_bytes(b.settings.headroom))?;
    figures(out, &b.figures)
}

/// `gc` result.
pub fn gc_summary(out: &mut dyn Write, name: &str, g: &GcPassSummary) -> io::Result<()> {
    if g.dry_run {
        writeln!(out, "gc dry run for {} (nothing changed):", sanitize(name))?;
    } else {
        writeln!(out, "gc pass {} finished for {}:", g.pass, sanitize(name))?;
    }
    let (freed, moved) = if g.dry_run {
        ("would free", "would rewrite")
    } else {
        ("freed", "rewrote")
    };
    kv(
        out,
        "condemned",
        &format!("{} segments", g.segments_condemned),
    )?;
    if !g.dry_run {
        kv(out, "deleted", &format!("{} segments", g.segments_deleted))?;
    }
    kv(out, "map packs", &g.packs_deleted.to_string())?;
    kv(out, "index runs", &g.runs_deleted.to_string())?;
    kv(out, "commits", &g.commits_deleted.to_string())?;
    kv(
        out,
        "compacted",
        &format!("{} segments", g.segments_compacted),
    )?;
    kv(out, freed, &format_bytes(g.bytes_freed))?;
    kv(out, moved, &format_bytes(g.bytes_moved))
}

/// `gc --status` result.
pub fn gc_status(out: &mut dyn Write, name: &str, g: &GcStatusSnapshot) -> io::Result<()> {
    kv(out, "volume", name)?;
    kv(out, "state", g.state.map_or("idle", gc_state_name))?;
    kv(
        out,
        "condemned",
        &format!("{} waiting out the grace period", g.condemned_pending),
    )?;
    kv(
        out,
        "moved pending",
        &format!("{} segments", g.moved_pending),
    )?;
    kv(
        out,
        "last pass",
        &match (&g.last_pass, g.last_finished_ms) {
            (Some(p), Some(ms)) => format!(
                "{} at {}, freed {}",
                p.pass,
                format_utc_ms(ms),
                format_bytes(p.bytes_freed)
            ),
            (Some(p), None) => format!("{}, freed {}", p.pass, format_bytes(p.bytes_freed)),
            _ => "none".to_owned(),
        },
    )?;
    kv(
        out,
        "totals",
        &format!(
            "{} passes, {} freed, {} compacted, live {}, dead {}",
            g.stats.passes,
            format_bytes(g.stats.bytes_freed),
            g.stats.segments_compacted,
            format_bytes(g.stats.live_bytes),
            format_bytes(g.stats.dead_bytes)
        ),
    )?;
    let r = &g.reclaim;
    if r.fast_passes > 0 {
        kv(
            out,
            "recheck-only",
            &format!(
                "{} passes, {} segments deleted, {} freed",
                r.fast_passes,
                r.fast_segments_deleted,
                format_bytes(r.fast_bytes_freed)
            ),
        )?;
    }
    if r.fast_passes + r.regular_passes > 0 {
        kv(
            out,
            "pass kinds",
            &format!(
                "{} regular, {} recheck-only",
                r.regular_passes, r.fast_passes
            ),
        )?;
    }
    if let Some(mean) = r.recheck_lag_ms_sum.checked_div(r.recheck_lag_count) {
        kv(
            out,
            "recheck lag",
            &format!(
                "mean {mean} ms, max {} ms over {} deletes",
                r.recheck_lag_ms_max, r.recheck_lag_count
            ),
        )?;
    }
    if let Some(mean) = r.fast_lag_ms_sum.checked_div(r.fast_lag_count) {
        kv(
            out,
            "recheck-only lag",
            &format!(
                "mean {mean} ms, max {} ms over {} deletes",
                r.fast_lag_ms_max, r.fast_lag_count
            ),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod reclaim_tests {
    use super::*;
    use lunix_iface::gc::GcReclaimStats;

    fn render(g: &GcStatusSnapshot) -> String {
        let mut out = Vec::new();
        gc_status(&mut out, "vol1", g).expect("render");
        String::from_utf8(out).expect("utf8")
    }

    #[test]
    fn the_default_reclaim_figures_print_nothing() {
        let t = render(&GcStatusSnapshot::default());
        assert!(!t.contains("recheck") && !t.contains("pass kinds"), "{t}");
    }

    #[test]
    fn the_reclaim_figures_print_their_own_lines() {
        let g = GcStatusSnapshot {
            reclaim: Box::new(GcReclaimStats {
                fast_passes: 4,
                regular_passes: 9,
                fast_segments_deleted: 11,
                fast_bytes_freed: 3 << 20,
                recheck_lag_ms_sum: 10_000,
                recheck_lag_ms_max: 2_500,
                recheck_lag_count: 5,
                fast_lag_ms_sum: 3_000,
                fast_lag_ms_max: 1_500,
                fast_lag_count: 3,
            }),
            ..GcStatusSnapshot::default()
        };
        let t = render(&g);
        assert!(t.contains("9 regular, 4 recheck-only"), "{t}");
        assert!(t.contains("4 passes, 11 segments deleted"), "{t}");
        assert!(
            t.contains("mean 2000 ms, max 2500 ms over 5 deletes"),
            "{t}"
        );
        assert!(
            t.contains("mean 1000 ms, max 1500 ms over 3 deletes"),
            "{t}"
        );
    }
}
