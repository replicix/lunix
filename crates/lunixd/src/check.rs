//! `lunixd check`: an offline store self-check of a detached volume, the
//! stand-in for `lunix fsck` (M14) in the M12a device smoke test.
//!
//! It takes the volume lock and changes nothing (besides the pid in `LOCK`):
//! the chain from the tip down to `chain_floor` authenticates and links; every
//! segment and map pack re-reads whole (footer and `seg_hash`, R3); every
//! segment a retained commit names exists; every map entry of the tip opens
//! with its id prefix, the chunk size and the id recomputed (R1, `--deep`);
//! no `*.tmp` or `*.open` file is left. The journal is not read: run it on a
//! detached volume, whose final commit covered every record.

use std::path::Path;
use std::sync::Arc;

use lunix_format::commit::Commit;
use lunix_format::volume::VolumeDescriptor;
use lunix_format::ExtentKind;
use lunix_iface::crypto::{CommitCodec, MetaCodec, OpenExpect, OpenOutput};
use lunix_iface::map::{entry_hint, BlockMap, NodeSource};
use lunix_iface::store::{CommitStore, PackRead, SegmentStore, VolumeDir};
use lunix_iface::Error;
use lunix_map::{MapConfig, RadixMap};
use lunix_store::{read_volume_file, LocalVolume, StoreOptions};

use crate::stack::{unlock, AttachCredential, StackError};

/// What the check found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// The tip's `commit_seq`.
    pub tip: u64,
    /// Commits verified (tip down to `chain_floor`).
    pub commits: u64,
    /// Segments re-read whole.
    pub segments: u64,
    /// Map packs re-read whole.
    pub packs: u64,
    /// Map entries whose extent opened.
    pub extents: u64,
    /// Every problem, one line each; empty means clean.
    pub problems: Vec<String>,
}

impl CheckReport {
    /// True when nothing was wrong.
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }
}

struct Packs(LocalVolume);

impl NodeSource for Packs {
    fn load_node(
        &self,
        loc: lunix_format::node::NodeLoc,
        out: &mut Vec<u8>,
    ) -> lunix_iface::Result<()> {
        self.0.read_node(loc, out)
    }
}

/// Checks the detached volume in `voldir`.
///
/// # Errors
/// The volume cannot be unlocked or opened (`Busy` while attached).
pub fn check_volume(voldir: &Path, cred: &AttachCredential) -> Result<CheckReport, StackError> {
    let bytes = read_volume_file(voldir)?;
    let desc = VolumeDescriptor::decode(&bytes).map_err(Error::from)?;
    let ok = unlock(voldir, &desc, cred)?;
    let meta = Arc::clone(ok.crypto.meta());
    let meta_codec: Arc<dyn MetaCodec> = meta.clone();
    let store = LocalVolume::open(voldir, meta_codec, StoreOptions::default())?;
    let mut r = CheckReport::default();
    let Some(tip) = CommitStore::tip(&store)? else {
        r.problems.push("no commit".into());
        return Ok(r);
    };
    let tip_commit = match meta.open_commit(tip, &CommitStore::get(&store, tip)?) {
        Ok(Commit::Full(f)) => f,
        Ok(_) => {
            r.problems
                .push(format!("tip {} is not a FULL commit", tip.0));
            return Ok(r);
        }
        Err(e) => {
            r.problems
                .push(format!("tip {} does not authenticate: {e}", tip.0));
            return Ok(r);
        }
    };
    let mut problems = Vec::new();
    let volume_hash = meta.object_hash(&bytes);
    if let Err(e) = meta.verify_chain(&store, tip, tip_commit.chain_floor, &volume_hash) {
        problems.push(format!("chain below {}: {e}", tip.0));
    }
    let mut commits = 0u64;
    let mut named = Vec::new();
    for s in tip_commit.chain_floor.0..=tip.0 {
        let seq = lunix_format::CommitSeq(s);
        match CommitStore::get(&store, seq).map(|b| meta.open_commit(seq, &b)) {
            Ok(Ok(Commit::Full(f))) => {
                commits += 1;
                named.extend(f.new_segments.iter().copied());
            }
            Ok(Ok(_)) => commits += 1,
            Ok(Err(e)) | Err(e) => problems.push(format!("commit {s}: {e}")),
        }
    }
    for seg in named {
        if let Err(e) = store.footer(seg) {
            problems.push(format!("segment {seg} named by the chain: {e}"));
        }
    }
    let mut segments = 0u64;
    for s in SegmentStore::list(&store)? {
        segments += 1;
        if let Err(e) = store.verify_segment(s.seg) {
            problems.push(format!("segment {}: {e}", s.seg));
        }
    }
    let mut packs = 0u64;
    for p in store.list_packs()? {
        packs += 1;
        if let Err(e) = store.verify_pack(p.pack) {
            problems.push(format!("pack {}: {e}", p.pack));
        }
    }
    let cs = desc.chunk_size;
    let map = RadixMap::open(
        Arc::clone(ok.crypto.extents()),
        Arc::new(Packs(store.clone())),
        &tip_commit.map_root,
        tip_commit.size / u64::from(cs),
        MapConfig::new(desc.map_fanout, desc.id_len),
    )?;
    let codec = Arc::clone(ok.crypto.extents());
    let mut extents = 0u64;
    let mut raw = Vec::new();
    let mut out = Vec::new();
    let walked = map.walk_leaves(&tip_commit.map_root, &mut |ci, e| {
        raw.clear();
        out.clear();
        let res = store.read_extent(&entry_hint(e), &mut raw).and_then(|()| {
            let expect = OpenExpect {
                chunk_size: cs,
                chunk_id: None,
                id_prefix: Some(&e.prefix),
                verify_id: true,
                output: OpenOutput::Plaintext,
            };
            codec.open(ExtentKind::Data, &raw, &expect, &mut out)
        });
        match res {
            Ok(_) if out.len() == cs as usize => extents += 1,
            Ok(_) => problems.push(format!("chunk {}: plaintext is not the chunk size", ci.0)),
            Err(err) => problems.push(format!("chunk {}: {err}", ci.0)),
        }
    });
    if let Err(e) = walked {
        problems.push(format!("map walk: {e}"));
    }
    for l in store.leftovers()? {
        problems.push(format!("leftover {}", l.name));
    }
    r.tip = tip.0;
    r.commits = commits;
    r.segments = segments;
    r.packs = packs;
    r.extents = extents;
    r.problems.extend(problems);
    Ok(r)
}
