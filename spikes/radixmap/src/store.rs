//! Node locations, the `NodeSource` trait, pack files, the byte-bounded LRU.
use crate::leaf::{wrap_enc, LeafFmt};
use crate::model::Model;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where a node lives: `pack` id, byte offset, stored length (16 bytes on disk).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeLoc {
    pub pack: u32,
    pub len: u32,
    pub off: u64,
}
/// Pack id 0: leaf is regenerated from the synthetic model; `off` = leaf index.
pub const PACK_VIRT: u32 = 0;
/// Written to a discarded sink (cannot be read back).
pub const PACK_GONE: u32 = u32::MAX - 1;
/// No location (empty subtree).
pub const PACK_NONE: u32 = u32::MAX;
pub const LOC_NONE: NodeLoc = NodeLoc {
    pack: PACK_NONE,
    len: 0,
    off: 0,
};

pub trait NodeSource: Send + Sync {
    /// Fetch the stored (possibly compressed) bytes of one node: one "GET".
    fn read(&self, loc: NodeLoc) -> std::io::Result<Vec<u8>>;
}

pub struct PackStore {
    pub dir: PathBuf,
    files: Mutex<HashMap<u32, Arc<File>>>,
    virt: Option<(Model, LeafFmt)>,
    next_pack: AtomicU64,
    /// simulated per-GET latency in microseconds (slept for real)
    pub latency_us: AtomicU64,
    pub gets: AtomicU64,
    pub get_bytes: AtomicU64,
    /// time spent regenerating virtual leaves (not part of a real system's cost)
    pub virt_ns: AtomicU64,
}

impl PackStore {
    pub fn new(dir: PathBuf, virt: Option<(Model, LeafFmt)>) -> Arc<Self> {
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(PackStore {
            dir,
            files: Mutex::new(HashMap::new()),
            virt,
            next_pack: AtomicU64::new(1),
            latency_us: AtomicU64::new(0),
            gets: AtomicU64::new(0),
            get_bytes: AtomicU64::new(0),
            virt_ns: AtomicU64::new(0),
        })
    }
    pub fn pack_path(&self, id: u32) -> PathBuf {
        self.dir.join(format!("pack-{id:06}.bin"))
    }
    /// Ids are never reused within a directory: scan for the highest existing.
    pub fn resume_ids(&self) {
        let mut mx = 0u64;
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if let Some(s) = n.strip_prefix("pack-").and_then(|s| s.strip_suffix(".bin")) {
                    mx = mx.max(s.parse().unwrap_or(0));
                }
            }
        }
        self.next_pack.store(mx + 1, Ordering::Relaxed);
    }
    pub fn new_writer(&self, discard: bool) -> PackWriter {
        let id = self.next_pack.fetch_add(1, Ordering::Relaxed) as u32;
        if discard {
            return PackWriter {
                id: PACK_GONE,
                sink: Sink::Discard,
                off: 0,
                t_write: Duration::ZERO,
                path: None,
            };
        }
        let p = self.pack_path(id);
        let f = File::create(&p).unwrap();
        PackWriter {
            id,
            sink: Sink::File(BufWriter::with_capacity(8 << 20, f)),
            off: 0,
            t_write: Duration::ZERO,
            path: Some(p),
        }
    }
    pub fn reset_counters(&self) {
        self.gets.store(0, Ordering::Relaxed);
        self.get_bytes.store(0, Ordering::Relaxed);
        self.virt_ns.store(0, Ordering::Relaxed);
    }
}

impl NodeSource for PackStore {
    fn read(&self, loc: NodeLoc) -> std::io::Result<Vec<u8>> {
        let lat = self.latency_us.load(Ordering::Relaxed);
        if lat > 0 {
            std::thread::sleep(Duration::from_micros(lat));
        }
        self.gets.fetch_add(1, Ordering::Relaxed);
        if loc.pack == PACK_VIRT {
            let (m, f) = self.virt.as_ref().expect("no virtual leaf model");
            let t = Instant::now();
            let mut ents = Vec::with_capacity(m.fan);
            m.leaf(loc.off, &mut ents);
            let mut raw = Vec::new();
            f.enc_raw(&ents, &mut raw);
            let b = wrap_enc(f.wrap, &raw);
            self.virt_ns
                .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
            self.get_bytes.fetch_add(b.len() as u64, Ordering::Relaxed);
            return Ok(b);
        }
        if loc.pack >= PACK_GONE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "node not retained",
            ));
        }
        let f = {
            let mut g = self.files.lock().unwrap();
            if let Some(f) = g.get(&loc.pack) {
                f.clone()
            } else {
                let f = Arc::new(File::open(self.pack_path(loc.pack))?);
                g.insert(loc.pack, f.clone());
                f
            }
        };
        let mut buf = vec![0u8; loc.len as usize];
        f.read_exact_at(&mut buf, loc.off)?;
        self.get_bytes
            .fetch_add(buf.len() as u64, Ordering::Relaxed);
        Ok(buf)
    }
}

pub enum Sink {
    File(BufWriter<File>),
    Discard,
}
pub struct PackWriter {
    pub id: u32,
    sink: Sink,
    pub off: u64,
    pub t_write: Duration,
    path: Option<PathBuf>,
}
impl PackWriter {
    pub fn append(&mut self, b: &[u8]) -> NodeLoc {
        let t = Instant::now();
        if let Sink::File(w) = &mut self.sink {
            w.write_all(b).unwrap();
        }
        self.t_write += t.elapsed();
        let loc = NodeLoc {
            pack: self.id,
            len: b.len() as u32,
            off: self.off,
        };
        self.off += b.len() as u64;
        loc
    }
    pub fn finish(&mut self) {
        let t = Instant::now();
        if let Sink::File(w) = &mut self.sink {
            w.flush().unwrap();
        }
        self.t_write += t.elapsed();
    }
    /// Delete the pack file (benchmarks free disk after each cell).
    pub fn remove(self) {
        if let Some(p) = self.path {
            drop(self.sink);
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Byte-bounded LRU of decoded (canonical) leaf bytes keyed by node hash.
pub struct Lru {
    cap: usize,
    pub bytes: usize,
    map: HashMap<[u8; 32], usize>,
    slab: Vec<LNode>,
    free: Vec<usize>,
    head: usize,
    tail: usize,
    pub evictions: u64,
}
struct LNode {
    key: [u8; 32],
    val: Arc<Vec<u8>>,
    prev: usize,
    next: usize,
}
const NIL: usize = usize::MAX;
impl Lru {
    pub fn new(cap: usize) -> Self {
        Lru {
            cap,
            bytes: 0,
            map: HashMap::new(),
            slab: Vec::new(),
            free: Vec::new(),
            head: NIL,
            tail: NIL,
            evictions: 0,
        }
    }
    fn unlink(&mut self, i: usize) {
        let (p, n) = (self.slab[i].prev, self.slab[i].next);
        if p != NIL {
            self.slab[p].next = n
        } else {
            self.head = n
        }
        if n != NIL {
            self.slab[n].prev = p
        } else {
            self.tail = p
        }
    }
    fn push_front(&mut self, i: usize) {
        self.slab[i].prev = NIL;
        self.slab[i].next = self.head;
        if self.head != NIL {
            let h = self.head;
            self.slab[h].prev = i;
        }
        self.head = i;
        if self.tail == NIL {
            self.tail = i;
        }
    }
    pub fn get(&mut self, k: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        let i = *self.map.get(k)?;
        self.unlink(i);
        self.push_front(i);
        Some(self.slab[i].val.clone())
    }
    pub fn put(&mut self, k: [u8; 32], v: Arc<Vec<u8>>) {
        if self.map.contains_key(&k) {
            return;
        }
        self.bytes += v.len() + 64;
        let node = LNode {
            key: k,
            val: v,
            prev: NIL,
            next: NIL,
        };
        let i = if let Some(i) = self.free.pop() {
            self.slab[i] = node;
            i
        } else {
            self.slab.push(node);
            self.slab.len() - 1
        };
        self.map.insert(k, i);
        self.push_front(i);
        while self.bytes > self.cap && self.tail != NIL && self.tail != i {
            let t = self.tail;
            self.unlink(t);
            let key = self.slab[t].key;
            self.bytes -= self.slab[t].val.len() + 64;
            self.slab[t].val = Arc::new(Vec::new());
            self.map.remove(&key);
            self.free.push(t);
            self.evictions += 1;
        }
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
}
