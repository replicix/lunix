//! Human-readable output of the responses.

use std::io::{self, Write};

use lunix_format::local::FreeFloor;
use lunix_format::local::OnFull;
use lunix_iface::ctl::{
    Attached, SpaceBody, SpaceFigures, VolumeCreated, VolumeMode, VolumeState, VolumeStatusBody,
    VolumeSummary,
};
use lunix_iface::gc::{GcPassSummary, GcState, GcStatusSnapshot};

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
    figures(out, &s.space)
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
    kv(
        out,
        "cache max",
        &b.settings
            .cache_max
            .map_or_else(|| "none".to_owned(), format_bytes),
    )?;
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
    )
}
