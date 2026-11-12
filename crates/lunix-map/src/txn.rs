//! [`Txn`]: a batch of updates applied atomically to the dirty overlay.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use lunix_format::node::{DataEntry, Entry, MAX_DATA_CLEN};
use lunix_format::Hint;
use lunix_iface::map::{entry_hint, ChunkNo, MapTxn, TxnReport};
use lunix_iface::{Error, Result};

use crate::map::{check_entry, Dirty, RadixMap};

enum Op {
    Set(DataEntry),
    Hole,
    Rehint(Hint, Hint),
}

/// Ops are buffered and evaluated at commit (a `rehint` compares against the
/// entry as of commit time). The cap is checked as ops arrive, counting every
/// leaf an op may dirty, so `MapFull` surfaces from `set` as the trait says.
pub(crate) struct Txn<'a> {
    map: &'a RadixMap,
    ops: Vec<(u64, Op)>,
    /// Leaves this transaction would add to the overlay.
    new_leaves: HashSet<u64>,
}

impl<'a> Txn<'a> {
    pub(crate) fn new(map: &'a RadixMap) -> Self {
        Self {
            map,
            ops: Vec::new(),
            new_leaves: HashSet::new(),
        }
    }

    fn touch(&mut self, ci: ChunkNo) -> Result<()> {
        let li = ci.0 / self.map.fanout() as u64;
        let g = self.map.inner();
        if ci.0 >= g.chunk_count {
            return Err(Error::invalid("chunk past the end of the map"));
        }
        if !g.overlay.contains_key(&li) && !self.new_leaves.contains(&li) {
            if g.overlay.len() as u64 + self.new_leaves.len() as u64 >= self.map.dirty_cap() {
                return Err(Error::MapFull);
            }
            self.new_leaves.insert(li);
        }
        Ok(())
    }
}

/// Applies `ops` in order to one leaf; true if any entry changed.
fn apply(entries: &mut [Entry], ops: &[(usize, &Op)], report: &mut TxnReport) -> bool {
    let mut changed = false;
    for &(slot, op) in ops {
        let Some(cur) = entries.get_mut(slot) else {
            continue;
        };
        let new = match op {
            Op::Set(e) => {
                report.sets += 1;
                Entry::Data(*e)
            }
            Op::Hole => {
                report.sets += 1;
                Entry::Hole
            }
            Op::Rehint(old, new) => match cur {
                Entry::Data(d) if entry_hint(d) == *old => {
                    report.rehints_applied += 1;
                    let mut d = *d;
                    d.seg = new.seg;
                    d.off = new.off;
                    d.clen = new.clen;
                    Entry::Data(d)
                }
                _ => {
                    report.rehints_skipped += 1;
                    continue;
                }
            },
        };
        if *cur != new {
            *cur = new;
            changed = true;
        }
    }
    changed
}

impl MapTxn for Txn<'_> {
    fn set(&mut self, ci: ChunkNo, entry: DataEntry) -> Result<()> {
        check_entry(&entry, self.map.id_len())?;
        self.touch(ci)?;
        self.ops.push((ci.0, Op::Set(entry)));
        Ok(())
    }

    fn set_hole(&mut self, ci: ChunkNo) -> Result<()> {
        self.touch(ci)?;
        self.ops.push((ci.0, Op::Hole));
        Ok(())
    }

    fn rehint(&mut self, ci: ChunkNo, old: Hint, new: Hint) -> Result<()> {
        if new.clen == 0 || new.clen > MAX_DATA_CLEN {
            return Err(Error::invalid("clen out of range"));
        }
        self.touch(ci)?;
        self.ops.push((ci.0, Op::Rehint(old, new)));
        Ok(())
    }

    fn commit(self: Box<Self>) -> Result<TxnReport> {
        let map = self.map;
        let fan = map.fanout() as u64;
        let _w = map.writer();

        let mut by_leaf: BTreeMap<u64, Vec<(usize, &Op)>> = BTreeMap::new();
        for (ci, op) in &self.ops {
            by_leaf
                .entry(ci / fan)
                .or_default()
                .push(((ci % fan) as usize, op));
        }

        // Read what is missing first, so that a failure changes nothing.
        let mut loaded: HashMap<u64, Vec<Entry>> = HashMap::new();
        for li in by_leaf.keys() {
            if !map.inner().overlay.contains_key(li) {
                loaded.insert(*li, map.leaf_entries(*li)?);
            }
        }

        let mut g = map.inner();
        if g.overlay.len() as u64 + loaded.len() as u64 > map.dirty_cap() {
            return Err(Error::MapFull);
        }
        let mut report = TxnReport::default();
        for (li, ops) in &by_leaf {
            if let Some(d) = g.overlay.get_mut(li) {
                let leaf = Arc::make_mut(d);
                apply(&mut leaf.entries, ops, &mut report);
            } else if let Some(mut entries) = loaded.remove(li) {
                if apply(&mut entries, ops, &mut report) {
                    let leaf: Dirty = Arc::new(lunix_format::node::Leaf { entries });
                    g.overlay.insert(*li, leaf);
                }
            }
        }
        Ok(report)
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        self.map.inner().txn_open = false;
    }
}
