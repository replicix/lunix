//! Step 3: the map walk from every retained root (rule R2).
//!
//! Every node is read through the extent codec (`kind = node`), must be codec
//! 0, must carry the id its parent names, and must hash to that id; the
//! shapes are checked against the descriptor and the commit's size. The walk
//! is streaming: only the entries it finds are kept, for the resolution step.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::os::unix::fs::FileExt;

use lunix_format::commit::{Commit, MapRoot};
use lunix_format::node::{
    empty_hashes, levels_for, Entry, Interior, Leaf, LeafParams, NodeLoc, MAX_NODE_EXTENT_LEN,
};
use lunix_format::{Codec, ExtentKind, IdPrefix, ObjId};
use lunix_iface::crypto::{OpenExpect, OpenOutput};
use lunix_iface::ErrorKind;

use crate::code;
use crate::facts::Facts;
use crate::report::{Finding, Sink};
use crate::session::Session;

/// One non-hole map entry.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EntryRec {
    pub(crate) chunk: u64,
    pub(crate) seg: ObjId,
    pub(crate) off: u32,
    pub(crate) clen: u32,
    pub(crate) prefix: IdPrefix,
}

/// What the walks found.
#[derive(Default, Debug)]
pub(crate) struct WalkOut {
    /// The tip root's entries, in chunk order.
    pub(crate) tip_entries: Vec<EntryRec>,
    /// Entries of the other retained roots' subtrees the tip does not share.
    pub(crate) other_entries: Vec<EntryRec>,
    /// Chunks the tip volume has.
    pub(crate) tip_chunks: u64,
    /// No finding was raised by the tip walk: the figures are complete.
    pub(crate) tip_complete: bool,
    /// Packs the walks reached.
    pub(crate) used_packs: HashSet<ObjId>,
}

/// Pack descriptors kept open at once.
const MAX_OPEN_PACKS: usize = 32;

struct Walker<'a> {
    s: &'a Session,
    packs: &'a std::collections::BTreeMap<ObjId, crate::containers::Container>,
    sink: &'a mut Sink,
    nodes: &'a mut u64,
    files: HashMap<ObjId, File>,
    fanout: u16,
    params: LeafParams,
    empty: Vec<[u8; 32]>,
    chunks: u64,
    visited: &'a mut HashSet<([u8; 32], u64, u32)>,
    skip_visited: bool,
    entries: Vec<EntryRec>,
    buf: Vec<u8>,
    body: Vec<u8>,
    failed: bool,
}

fn pack_name(pack: ObjId) -> String {
    format!("map/{:016x}", pack.raw())
}

pub(crate) fn walk(s: &Session, sink: &mut Sink, facts: &mut Facts) {
    let Some(tip) = facts.tip.clone() else {
        return;
    };
    let mut visited: HashSet<([u8; 32], u64, u32)> = HashSet::new();
    let mut out = WalkOut::default();
    let mut nodes = 0u64;
    // The tip first and whole, so its entry list is complete; the other
    // retained roots then skip every subtree already walked.
    let mut roots: Vec<(MapRoot, u64, bool)> = vec![(tip.full.map_root, tip.full.size, true)];
    for r in facts.retained.iter().rev() {
        if let Commit::Full(f) = &r.commit {
            if r.seq != tip.seq {
                roots.push((f.map_root, f.size, false));
            }
        }
    }
    for (root, size, is_tip) in roots {
        let mut w = Walker::new(s, &facts.packs, sink, &mut nodes, &mut visited, !is_tip);
        let Some(()) = w.setup(size) else {
            if is_tip {
                out.tip_complete = false;
            }
            continue;
        };
        w.walk_root(&root);
        let failed = w.failed;
        let chunks = w.chunks;
        let entries = std::mem::take(&mut w.entries);
        if is_tip {
            out.tip_entries = entries;
            out.tip_chunks = chunks;
            out.tip_complete = !failed;
        } else {
            out.other_entries.extend(entries);
        }
    }
    out.used_packs = visited
        .iter()
        .filter_map(|(_, p, _)| ObjId::from_raw(*p).ok())
        .collect();
    facts.stats.map_nodes += nodes;
    facts.walk = Some(out);
}

impl<'a> Walker<'a> {
    fn new(
        s: &'a Session,
        packs: &'a std::collections::BTreeMap<ObjId, crate::containers::Container>,
        sink: &'a mut Sink,
        nodes: &'a mut u64,
        visited: &'a mut HashSet<([u8; 32], u64, u32)>,
        skip_visited: bool,
    ) -> Self {
        let fanout = s.desc.map_fanout;
        Self {
            s,
            packs,
            sink,
            nodes,
            files: HashMap::new(),
            fanout,
            params: LeafParams {
                fanout,
                id_len: s.desc.id_len,
                ovh: s.ovh(),
            },
            empty: Vec::new(),
            chunks: 0,
            visited,
            skip_visited,
            entries: Vec::new(),
            buf: Vec::new(),
            body: Vec::new(),
            failed: false,
        }
    }

    fn fail(&mut self, f: Finding) {
        self.failed = true;
        self.sink.push(f);
    }

    /// Depth and empty hashes for a volume of `size` bytes.
    fn setup(&mut self, size: u64) -> Option<()> {
        let cs = u64::from(self.s.desc.chunk_size);
        let levels = match levels_for(size, self.s.desc.chunk_size, self.fanout) {
            Ok(l) => l,
            Err(e) => {
                self.fail(Finding::new(
                    code::MAP_ROOT,
                    "state/commits",
                    format!("the size {size} gives no valid map depth: {e}"),
                ));
                return None;
            }
        };
        let codec = self.s.extents();
        let empty = empty_hashes(self.fanout, levels, |b| {
            codec.chunk_id(ExtentKind::Node, b).0
        });
        match empty {
            Ok(e) => self.empty = e,
            Err(e) => {
                self.fail(Finding::new(
                    code::MAP_ROOT,
                    "state/commits",
                    format!("{e}"),
                ));
                return None;
            }
        }
        self.chunks = size / cs;
        Some(())
    }

    fn levels(&self) -> u8 {
        // `empty` holds E_0 ..= E_levels.
        (self.empty.len() - 1) as u8
    }

    fn walk_root(&mut self, root: &MapRoot) {
        let levels = self.levels();
        let Some(pack) = root.pack else {
            // An empty map: the hash is the empty hash of some level.
            if !(1..levels).any(|k| self.empty[usize::from(k)] == root.hash) {
                self.fail(Finding::new(
                    code::MAP_ROOT,
                    "state/commits",
                    "an empty root whose hash is not the empty hash of any level of this size",
                ));
            }
            return;
        };
        let loc = NodeLoc {
            pack,
            off: root.off,
            len: root.len,
        };
        if loc.len == 0 || loc.len > MAX_NODE_EXTENT_LEN {
            self.fail(Finding::new(
                code::MAP_ROOT,
                "state/commits",
                "the root's location length is out of range",
            ));
            return;
        }
        self.node(root.hash, loc, levels - 1, 0);
    }

    fn pread(&mut self, loc: NodeLoc) -> std::io::Result<()> {
        if !self.files.contains_key(&loc.pack) {
            // A volume has one live pack per commit that left a node behind:
            // keep the descriptors bounded, the walk revisits a pack's nodes
            // close together.
            if self.files.len() >= MAX_OPEN_PACKS {
                self.files.clear();
            }
            let f = File::open(self.s.dir.join(pack_name(loc.pack)))?;
            self.files.insert(loc.pack, f);
        }
        self.buf.resize(loc.len as usize, 0);
        match self.files.get(&loc.pack) {
            Some(f) => f.read_exact_at(&mut self.buf, u64::from(loc.off)),
            None => Err(std::io::Error::other("no file")),
        }
    }

    /// Reads, checks and descends into one node. `first_leaf` is the index of
    /// the first leaf below it.
    fn node(&mut self, hash: [u8; 32], loc: NodeLoc, level: u8, first_leaf: u128) {
        let key = (hash, loc.pack.raw(), loc.off);
        if self.skip_visited && self.visited.contains(&key) {
            return;
        }
        let obj = pack_name(loc.pack);
        let at =
            |f: Finding| f.with_range(u64::from(loc.off), u64::from(loc.off) + u64::from(loc.len));
        let ovh = self.s.ovh();
        match self.packs.get(&loc.pack) {
            None => {
                self.fail(Finding::new(
                    code::MAP_NODE_MISSING,
                    &obj,
                    "the pack does not exist",
                ));
                return;
            }
            Some(p) if !p.good => {
                self.fail(Finding::new(
                    code::MAP_NODE_MISSING,
                    &obj,
                    "the pack failed its own checks and counts as absent (R3)",
                ));
                return;
            }
            Some(p) => {
                let ok = p
                    .find(loc.off)
                    .is_some_and(|e| u64::from(ovh) + u64::from(e.clen) == u64::from(loc.len));
                if !ok {
                    self.fail(at(Finding::new(
                        code::MAP_NODE_LOC,
                        &obj,
                        "the child's location is not an extent of the pack",
                    )));
                    return;
                }
            }
        }
        if let Err(e) = self.pread(loc) {
            self.fail(Finding::new(code::IO, &obj, format!("cannot read: {e}")));
            return;
        }
        *self.nodes += 1;
        self.body.clear();
        let expect = OpenExpect {
            chunk_size: lunix_format::segment::MAX_NODE_ULEN,
            chunk_id: None,
            id_prefix: None,
            verify_id: false,
            output: OpenOutput::Plaintext,
        };
        let meta = match self
            .s
            .extents()
            .open(ExtentKind::Node, &self.buf, &expect, &mut self.body)
        {
            Ok(m) => m,
            Err(e) => {
                let (c, why) = if e.kind() == ErrorKind::Auth {
                    (code::MAP_NODE_AUTH, "the node extent does not authenticate")
                } else {
                    (code::MAP_NODE_CORRUPT, "the node extent does not decode")
                };
                self.fail(at(Finding::new(c, &obj, why)));
                return;
            }
        };
        if meta.desc.codec != Codec::Raw {
            self.fail(at(Finding::new(
                code::MAP_NODE_CODEC,
                &obj,
                format!(
                    "the node is stored with codec {}, not 0",
                    meta.desc.codec.id()
                ),
            )));
            return;
        }
        if meta.chunk_id.0 != hash {
            self.fail(at(Finding::new(
                code::MAP_NODE_ID,
                &obj,
                "the extent carries another id than the hash its parent names",
            )));
            return;
        }
        self.visited.insert(key);
        let body = std::mem::take(&mut self.body);
        if level == 0 {
            self.leaf(&body, hash, &obj, loc, first_leaf);
        } else {
            self.interior(&body, hash, &obj, loc, level, first_leaf);
        }
        self.body = body;
    }

    fn leaf(&mut self, body: &[u8], hash: [u8; 32], obj: &str, loc: NodeLoc, first_leaf: u128) {
        let at =
            |f: Finding| f.with_range(u64::from(loc.off), u64::from(loc.off) + u64::from(loc.len));
        if self.s.extents().chunk_id(ExtentKind::Node, body).0 != hash {
            self.fail(at(Finding::new(
                code::MAP_NODE_HASH,
                obj,
                "the leaf hashes to another value than its parent's child_hash",
            )));
            return;
        }
        let leaf = match Leaf::decode(body, &self.params) {
            Ok(l) => l,
            Err(e) => {
                self.fail(at(Finding::new(code::MAP_NODE_SHAPE, obj, format!("{e}"))));
                return;
            }
        };
        let mut beyond = 0u64;
        for (slot, e) in leaf.entries.iter().enumerate() {
            let Entry::Data(d) = e else { continue };
            let chunk = first_leaf * u128::from(self.fanout) + slot as u128;
            if chunk >= u128::from(self.chunks) {
                beyond += 1;
                continue;
            }
            self.entries.push(EntryRec {
                chunk: chunk as u64,
                seg: d.seg,
                off: d.off,
                clen: d.clen,
                prefix: d.prefix,
            });
        }
        if beyond > 0 {
            self.fail(at(Finding::new(
                code::MAP_ENTRY_RANGE,
                obj,
                format!("{beyond} entries lie past the end of the volume"),
            )));
        }
    }

    fn interior(
        &mut self,
        body: &[u8],
        hash: [u8; 32],
        obj: &str,
        loc: NodeLoc,
        level: u8,
        first_leaf: u128,
    ) {
        let at =
            |f: Finding| f.with_range(u64::from(loc.off), u64::from(loc.off) + u64::from(loc.len));
        let node = match Interior::decode(body, self.fanout) {
            Ok(n) => n,
            Err(e) => {
                self.fail(at(Finding::new(code::MAP_NODE_SHAPE, obj, format!("{e}"))));
                return;
            }
        };
        if node.level != level {
            self.fail(at(Finding::new(
                code::MAP_NODE_SHAPE,
                obj,
                format!(
                    "the node has level {} where its position needs level {level}",
                    node.level
                ),
            )));
            return;
        }
        let hashes_ok = node
            .hash_input()
            .is_ok_and(|h| self.s.extents().chunk_id(ExtentKind::Node, &h).0 == hash);
        if !hashes_ok {
            self.fail(at(Finding::new(
                code::MAP_NODE_HASH,
                obj,
                "the node hashes to another value than its parent's child_hash",
            )));
            return;
        }
        let below = usize::from(level) - 1;
        let span = u128::from(self.fanout)
            .checked_pow(u32::from(level) - 1)
            .unwrap_or(u128::MAX);
        for (i, c) in node.children.iter().enumerate() {
            if c.hash == self.empty[below] {
                continue;
            }
            let Some(cloc) = c.loc else {
                self.fail(at(Finding::new(
                    code::MAP_NODE_SHAPE,
                    obj,
                    "a non-empty child has no location",
                )));
                continue;
            };
            let first = first_leaf.saturating_add(span.saturating_mul(i as u128));
            self.node(c.hash, cloc, level - 1, first);
        }
    }
}
