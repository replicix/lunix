//! Journal file and record (freeze item 2, on-disk-formats.md section 5).
//!
//! A lane owns a ring of 64 MiB files. Block 0 of each file is a
//! [`FileHeader`]; records start at offset 4096 and are packed back to back,
//! 8-byte aligned, in batches padded with zeros to a 4 KiB multiple (D12).
//! [`scan_file`] is the normative reader (section 5.3): it resynchronises past
//! a damaged record, and the global `write_seq` contiguity check of recovery
//! (not this crate) decides where recovery stops.
//!
//! The same record framing is the DELTA inline-records region
//! ([`encode_delta_region`]) and the future memfd sink.

use core::ops::Range;

use crate::bytes::{align8, crc32c, crc32c_append, round4k, Put, Reader};
use crate::error::{FormatError, Result};
use crate::ids::{VolumeUuid, WriteSeq};

const WHAT_FILE: &str = "journal file header";
const WHAT_REC: &str = "journal record";

/// File header magic.
pub const FILE_MAGIC: &[u8; 8] = b"LUNIXJNL";
/// Record magic.
pub const RECORD_MAGIC: &[u8; 4] = b"LXJR";
/// Block size of the journal grid.
pub const BLOCK: usize = 4096;
/// Length of a journal file.
pub const FILE_LEN: usize = 64 << 20;
/// Length of the inline record header.
pub const RECORD_HEADER_LEN: usize = 64;
/// Most blocks in one write record (1 MiB).
pub const MAX_WRITE_BLOCKS: u32 = 256;
/// Most lanes.
pub const MAX_LANES: u16 = 255;

/// Block 0 of a journal file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileHeader {
    /// Lane, 0 ..= 255; equals the lane in the file name.
    pub lane: u16,
    /// File ordinal, at least 1; equals the ordinal in the file name.
    pub ordinal: u32,
    /// The volume the file belongs to.
    pub volume_uuid: VolumeUuid,
    /// Random value drawn each time the file is (re)initialised; never zero.
    pub generation: u64,
}

impl FileHeader {
    /// Appends the whole 4096-byte block (header and zero padding).
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        if self.lane > MAX_LANES {
            return Err(FormatError::range(WHAT_FILE, "lane", u64::from(self.lane)));
        }
        if self.ordinal == 0 {
            return Err(FormatError::range(WHAT_FILE, "ordinal", 0));
        }
        if self.generation == 0 {
            return Err(FormatError::range(WHAT_FILE, "generation", 0));
        }
        let start = out.len();
        out.extend_from_slice(FILE_MAGIC);
        out.put_u16(1);
        out.put_u16(self.lane);
        out.put_u32(self.ordinal);
        out.extend_from_slice(&self.volume_uuid.0);
        out.put_u64(self.generation);
        out.put_zeros(20);
        let crc = crc32c(&out[start..]);
        out.put_u32(crc);
        out.put_zeros(BLOCK - 64);
        Ok(())
    }

    /// Decodes block 0; `block` is exactly 4096 bytes.
    pub fn decode(block: &[u8]) -> Result<Self> {
        let mut r = Reader::new(block, WHAT_FILE);
        r.magic(FILE_MAGIC)?;
        r.version_u16(1)?;
        let lane = r.u16()?;
        if lane > MAX_LANES {
            return Err(FormatError::range(WHAT_FILE, "lane", u64::from(lane)));
        }
        let ordinal = r.u32()?;
        if ordinal == 0 {
            return Err(FormatError::range(WHAT_FILE, "ordinal", 0));
        }
        let volume_uuid = VolumeUuid(r.array()?);
        let generation = r.u64()?;
        if generation == 0 {
            return Err(FormatError::range(WHAT_FILE, "generation", 0));
        }
        r.reserved(20)?;
        let covered = r.consumed();
        let crc = r.u32()?;
        if crc != crc32c(covered) {
            return Err(FormatError::Checksum { what: WHAT_FILE });
        }
        r.reserved(BLOCK - 64)?;
        r.finish()?;
        Ok(Self {
            lane,
            ordinal,
            volume_uuid,
            generation,
        })
    }
}

/// What a record does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecordKind {
    /// Data for `len` blocks starting at `lba`.
    Write = 1,
    /// DISCARD or WRITE_ZEROES of `len` blocks; no data.
    Hole = 2,
}

/// The fixed 64-byte record header, without its CRC.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RecordHeader {
    /// Write or hole.
    pub kind: RecordKind,
    /// Strictly rising within a lane.
    pub write_seq: WriteSeq,
    /// Owner epoch as stored (0 in phase 1; not checked here).
    pub epoch: u64,
    /// First block, in 4096-byte blocks.
    pub lba: u64,
    /// Length in blocks.
    pub len: u32,
    /// Container generation (file header `generation`; DELTA: 0).
    pub gen: u64,
    /// The record's own offset in its container.
    pub off: u64,
}

/// A record: header and (for a write) borrowed data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record<'a> {
    /// The header.
    pub header: RecordHeader,
    /// `len * 4096` bytes for a write, empty for a hole.
    pub data: &'a [u8],
}

impl Record<'_> {
    /// Encoded size, `align8(64 + data length)`.
    pub fn encoded_len(&self) -> usize {
        // data is bounded by 1 MiB for any record this crate accepts
        align8(RECORD_HEADER_LEN.saturating_add(self.data.len())).unwrap_or(usize::MAX)
    }

    fn validate(&self) -> Result<()> {
        let h = &self.header;
        if h.write_seq.0 == 0 {
            return Err(FormatError::range(WHAT_REC, "write_seq", 0));
        }
        match h.kind {
            RecordKind::Write => {
                if h.len == 0 || h.len > MAX_WRITE_BLOCKS {
                    return Err(FormatError::range(WHAT_REC, "len", u64::from(h.len)));
                }
                if self.data.len() != h.len as usize * BLOCK {
                    return Err(FormatError::invalid(
                        WHAT_REC,
                        "data length differs from len",
                    ));
                }
            }
            RecordKind::Hole => {
                if h.len == 0 {
                    return Err(FormatError::range(WHAT_REC, "len", 0));
                }
                if !self.data.is_empty() {
                    return Err(FormatError::invalid(WHAT_REC, "a hole carries no data"));
                }
            }
        }
        Ok(())
    }

    /// Appends the record (header with CRC, then data).
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        let h = &self.header;
        let start = out.len();
        out.extend_from_slice(RECORD_MAGIC);
        out.put_u8(1);
        out.put_u8(h.kind as u8);
        out.put_zeros(2);
        out.put_u64(h.write_seq.0);
        out.put_u64(h.epoch);
        out.put_u64(h.lba);
        out.put_u32(h.len);
        out.put_u32(0); // crc placeholder
        out.put_u64(h.gen);
        out.put_u64(h.off);
        out.put_zeros(8);
        let crc = crc32c_append(crc32c(&out[start..]), self.data);
        out[start + 36..start + 40].copy_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(self.data);
        Ok(())
    }

    /// Parses the record at the start of `buf` and returns it with the number
    /// of bytes it occupies. Checks everything except `gen` and `off`, which
    /// belong to the container.
    pub fn parse(buf: &[u8]) -> Result<(Record<'_>, usize)> {
        let mut r = Reader::new(buf, WHAT_REC);
        r.magic(RECORD_MAGIC)?;
        r.version_u8(1)?;
        let kind = match r.u8()? {
            1 => RecordKind::Write,
            2 => RecordKind::Hole,
            v => return Err(FormatError::range(WHAT_REC, "kind", u64::from(v))),
        };
        r.reserved(2)?;
        let write_seq = WriteSeq(r.u64()?);
        let epoch = r.u64()?;
        let lba = r.u64()?;
        let len = r.u32()?;
        let crc = r.u32()?;
        let gen = r.u64()?;
        let off = r.u64()?;
        r.reserved(8)?;
        let data_len = match kind {
            RecordKind::Write => {
                if len == 0 || len > MAX_WRITE_BLOCKS {
                    return Err(FormatError::range(WHAT_REC, "len", u64::from(len)));
                }
                len as usize * BLOCK
            }
            RecordKind::Hole => {
                if len == 0 {
                    return Err(FormatError::range(WHAT_REC, "len", 0));
                }
                0
            }
        };
        if write_seq.0 == 0 {
            return Err(FormatError::range(WHAT_REC, "write_seq", 0));
        }
        let data = r.take(data_len)?;
        let mut head = [0u8; RECORD_HEADER_LEN];
        head.copy_from_slice(&buf[..RECORD_HEADER_LEN]);
        head[36..40].fill(0);
        if crc != crc32c_append(crc32c(&head), data) {
            return Err(FormatError::Checksum { what: WHAT_REC });
        }
        let used = r.pos();
        Ok((
            Record {
                header: RecordHeader {
                    kind,
                    write_seq,
                    epoch,
                    lba,
                    len,
                    gen,
                    off,
                },
                data,
            },
            used,
        ))
    }

    /// Decodes exactly one record.
    pub fn decode(buf: &[u8]) -> Result<Record<'_>> {
        let (rec, used) = Self::parse(buf)?;
        if used != buf.len() {
            return Err(FormatError::Trailing {
                what: WHAT_REC,
                extra: buf.len() - used,
            });
        }
        Ok(rec)
    }
}

/// Packs records into one batch (D12): back to back, 8-byte aligned, padded
/// with zeros to a 4 KiB multiple by [`BatchBuilder::finish`].
#[derive(Debug)]
pub struct BatchBuilder {
    gen: u64,
    base: u64,
    buf: Vec<u8>,
}

impl BatchBuilder {
    /// A batch whose first byte lands at file offset `base` (a multiple of
    /// 4096) in a file of generation `gen`.
    pub fn new(gen: u64, base: u64) -> Result<Self> {
        if gen == 0 {
            return Err(FormatError::range(WHAT_REC, "gen", 0));
        }
        if !base.is_multiple_of(BLOCK as u64) || base < BLOCK as u64 {
            return Err(FormatError::invalid(
                WHAT_REC,
                "batch must start on a block after block 0",
            ));
        }
        Ok(Self {
            gen,
            base,
            buf: Vec::new(),
        })
    }

    /// Bytes packed so far (before padding).
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True before the first record.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn push(&mut self, header: RecordHeader, data: &[u8]) -> Result<()> {
        let off = self
            .base
            .checked_add(self.buf.len() as u64)
            .ok_or(FormatError::Overflow {
                what: WHAT_REC,
                why: "file offset",
            })?;
        let rec = Record {
            header: RecordHeader {
                gen: self.gen,
                off,
                ..header
            },
            data,
        };
        rec.encode(&mut self.buf)
    }

    /// Appends a write record.
    pub fn push_write(
        &mut self,
        write_seq: WriteSeq,
        epoch: u64,
        lba: u64,
        data: &[u8],
    ) -> Result<()> {
        let len = u32::try_from(data.len() / BLOCK)
            .map_err(|_| FormatError::range(WHAT_REC, "len", data.len() as u64))?;
        self.push(
            RecordHeader {
                kind: RecordKind::Write,
                write_seq,
                epoch,
                lba,
                len,
                gen: 0,
                off: 0,
            },
            data,
        )
    }

    /// Appends a hole record of `len` blocks.
    pub fn push_hole(&mut self, write_seq: WriteSeq, epoch: u64, lba: u64, len: u32) -> Result<()> {
        self.push(
            RecordHeader {
                kind: RecordKind::Hole,
                write_seq,
                epoch,
                lba,
                len,
                gen: 0,
                off: 0,
            },
            &[],
        )
    }

    /// The batch, zero-padded to a multiple of 4096 bytes.
    pub fn finish(mut self) -> Vec<u8> {
        if let Some(n) = round4k(self.buf.len()) {
            self.buf.resize(n, 0);
        }
        self.buf
    }
}

/// The outcome of reading one journal file.
#[derive(Debug, PartialEq, Eq)]
pub struct ScanResult<'a> {
    /// The file header.
    pub header: FileHeader,
    /// Accepted records in file order.
    pub records: Vec<Record<'a>>,
    /// Byte ranges the reader had to skip because no valid record started
    /// there.
    pub damaged: Vec<Range<usize>>,
}

fn valid_at<'a>(file: &'a [u8], q: usize, gen: u64, prev: u64) -> Option<(Record<'a>, usize)> {
    let tail = file.get(q..)?;
    // Compare gen, off and write_seq before the CRC, which covers up to
    // 1 MiB: a false candidate then costs a constant.
    let u64_at = |o: usize| -> Option<u64> {
        Some(u64::from_le_bytes(tail.get(o..o + 8)?.try_into().ok()?))
    };
    if u64_at(40)? != gen || u64_at(48)? != q as u64 || u64_at(8)? <= prev {
        return None;
    }
    let (rec, used) = Record::parse(tail).ok()?;
    let ok = rec.header.gen == gen && rec.header.off == q as u64 && rec.header.write_seq.0 > prev;
    ok.then_some((rec, used))
}

/// The normative journal reader (section 5.3) over the bytes of one file.
///
/// `prev_write_seq` is the last `write_seq` accepted in this lane so far (0
/// for the first file). Errors only when the file header is invalid, in which
/// case the file holds no records.
pub fn scan_file(file: &[u8], prev_write_seq: WriteSeq) -> Result<ScanResult<'_>> {
    let head = file.get(..BLOCK).ok_or(FormatError::Truncated {
        what: WHAT_FILE,
        need: BLOCK,
        have: file.len(),
    })?;
    let header = FileHeader::decode(head)?;
    let len = file.len();
    let mut records = Vec::new();
    let mut damaged = Vec::new();
    let mut prev = prev_write_seq.0;
    let mut p = BLOCK;
    while p + 4 <= len {
        let m = u32::from_le_bytes([file[p], file[p + 1], file[p + 2], file[p + 3]]);
        if m == 0 {
            if p.is_multiple_of(BLOCK) {
                break;
            }
            match round4k(p) {
                Some(n) => p = n,
                None => break,
            }
            continue;
        }
        if let Some((rec, used)) = valid_at(file, p, header.generation, prev) {
            prev = rec.header.write_seq.0;
            records.push(rec);
            p += used;
            continue;
        }
        let mut q = p + 8;
        let mut found = None;
        while q + RECORD_HEADER_LEN <= len {
            if file[q..q + 4] == *RECORD_MAGIC
                && valid_at(file, q, header.generation, prev).is_some()
            {
                found = Some(q);
                break;
            }
            q += 8;
        }
        match found {
            Some(q) => {
                damaged.push(p..q);
                p = q;
            }
            None => {
                damaged.push(p..len);
                break;
            }
        }
    }
    Ok(ScanResult {
        header,
        records,
        damaged,
    })
}

/// Encodes a DELTA inline-records region: records back to back, `gen` 0,
/// `off` = offset in the region, no padding and no terminator. The record
/// headers' `gen` and `off` are overwritten.
pub fn encode_delta_region(records: &[Record<'_>], out: &mut Vec<u8>) -> Result<()> {
    let start = out.len();
    let mut prev = 0u64;
    for rec in records {
        if rec.header.write_seq.0 <= prev {
            return Err(FormatError::invalid(WHAT_REC, "write_seq must rise"));
        }
        prev = rec.header.write_seq.0;
        let r = Record {
            header: RecordHeader {
                gen: 0,
                off: (out.len() - start) as u64,
                ..rec.header
            },
            data: rec.data,
        };
        r.encode(out)?;
    }
    Ok(())
}

/// Decodes a DELTA inline-records region strictly.
pub fn decode_delta_region(region: &[u8]) -> Result<Vec<Record<'_>>> {
    let mut recs = Vec::new();
    let mut p = 0usize;
    let mut prev = 0u64;
    while p < region.len() {
        let (rec, used) = Record::parse(&region[p..])?;
        if rec.header.gen != 0 || rec.header.off != p as u64 {
            return Err(FormatError::invalid(
                WHAT_REC,
                "gen or off wrong for a DELTA record",
            ));
        }
        if rec.header.write_seq.0 <= prev {
            return Err(FormatError::invalid(WHAT_REC, "write_seq does not rise"));
        }
        prev = rec.header.write_seq.0;
        recs.push(rec);
        p += used;
    }
    Ok(recs)
}
