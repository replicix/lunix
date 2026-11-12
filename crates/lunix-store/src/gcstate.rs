//! Pins (`snapshots/`, `readers/`, `holds/`) and GC state (`state/gc/`,
//! `gc/<pass>`).

use std::fs;
use std::path::Path;

use lunix_format::gcfile::{Condemned, GcPassBody};
use lunix_format::paths::{self, local as lpath};
use lunix_format::pins::{HoldPin, ReaderPin, SnapshotPin};
use lunix_format::{ObjectKind, VolumeUuid};
use lunix_iface::store::{GcStateStore, PinKind, PinRoot, PinSource};
use lunix_iface::{Error, Result};

use crate::fsops::{
    io_err, io_or_missing, list_hex_ids, publish, read_capped, read_dir_names, Publish,
    EVIDENCE_POINTS, STATE_POINTS,
};
use crate::volume::LocalVolume;

/// Pin objects are at most 4 KiB of body; the file is that plus the envelope.
const MAX_PIN_FILE: u64 = 4096 + 128;

impl LocalVolume {
    /// Reads one pin object, authenticates it as `kind` under its
    /// bucket-relative `name`, and returns the body.
    fn open_pin(&self, dir: &Path, file: &str, kind: ObjectKind, name: &str) -> Result<Vec<u8>> {
        let path = dir.join(file);
        let size = fs::metadata(&path)
            .map_err(|e| io_or_missing(e, "pin"))?
            .len();
        if size > MAX_PIN_FILE {
            return Err(Error::Corrupt {
                what: "pin",
                detail: "longer than any pin object",
            });
        }
        let bytes = fs::read(&path).map_err(|e| io_or_missing(e, "pin"))?;
        self.inner.meta.open(kind, name, &bytes)
    }

    fn pin_files(&self, sub: &str) -> Result<Vec<String>> {
        Ok(read_dir_names(&self.inner.path(sub))
            .map_err(io_err)?
            .into_iter()
            .map(|(n, _)| n)
            // A crashed writer's temp file is not a pin.
            .filter(|n| !n.ends_with(".tmp"))
            .collect())
    }
}

fn bad_pin(detail: &'static str) -> Error {
    Error::Corrupt {
        what: "pin",
        detail,
    }
}

impl PinSource for LocalVolume {
    fn roots(&self) -> Result<Vec<PinRoot>> {
        let mut roots = Vec::new();

        let dir = self.inner.path("snapshots");
        for file in self.pin_files("snapshots")? {
            let name = paths::snapshot(&file).map_err(|_| bad_pin("snapshot name"))?;
            let body = self.open_pin(&dir, &file, ObjectKind::Snapshots, &name)?;
            let pin = SnapshotPin::decode(&body)?;
            roots.push(PinRoot {
                kind: PinKind::Snapshot,
                name: file,
                commit_seq: pin.commit_seq,
                deadline: None,
            });
        }

        let dir = self.inner.path("readers");
        for file in self.pin_files("readers")? {
            let (host, id) = file
                .rsplit_once('-')
                .ok_or_else(|| bad_pin("reader name"))?;
            let id = crate::fsops::parse_hex16(id).ok_or_else(|| bad_pin("reader name"))?;
            let name = paths::reader(host, id).map_err(|_| bad_pin("reader name"))?;
            let body = self.open_pin(&dir, &file, ObjectKind::Readers, &name)?;
            let pin = ReaderPin::decode(&body)?;
            roots.push(PinRoot {
                kind: PinKind::Reader,
                name: file,
                commit_seq: pin.commit_seq,
                deadline: Some(pin.deadline),
            });
        }

        let dir = self.inner.path("holds");
        for file in self.pin_files("holds")? {
            let child: VolumeUuid = file.parse().map_err(|_| bad_pin("hold name"))?;
            let name = paths::hold(&child);
            let body = self.open_pin(&dir, &file, ObjectKind::Holds, &name)?;
            let pin = HoldPin::decode(&body)?;
            roots.push(PinRoot {
                kind: PinKind::Hold,
                name: file,
                commit_seq: pin.base_seq,
                deadline: None,
            });
        }
        Ok(roots)
    }
}

impl GcStateStore for LocalVolume {
    fn load_condemned(&self) -> Result<Option<Condemned>> {
        let path = self.inner.path(&lpath::state_gc_condemned());
        // It is a hint: absent, oversized or undecodable all mean "re-mark".
        match read_capped(
            &path,
            u64::from(lunix_format::gcfile::CONDEMNED_MAX_BODY) + 64,
            "condemned list",
        ) {
            Ok(bytes) => Ok(Condemned::decode(&bytes).ok()),
            Err(e)
                if matches!(
                    e.kind(),
                    lunix_iface::ErrorKind::NotFound | lunix_iface::ErrorKind::Corrupt
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn store_condemned(&self, list: &Condemned) -> Result<()> {
        let mut bytes = Vec::new();
        list.encode(&mut bytes)?;
        publish(
            &self.inner,
            &self.inner.path("state/gc"),
            "condemned",
            &bytes,
            Publish::Replace,
            &STATE_POINTS,
        )
    }

    fn put_evidence(&self, pass: u64, lines: &[u8]) -> Result<()> {
        let name =
            paths::gc_pass(pass).map_err(|_| Error::invalid("gc pass must be at least 1"))?;
        let mut body = Vec::new();
        GcPassBody {
            bytes: lines.to_vec(),
        }
        .encode(&mut body)?;
        let sealed = self.inner.meta.seal(ObjectKind::Gc, &name, &body)?;
        publish(
            &self.inner,
            &self.inner.path("gc"),
            name.strip_prefix("gc/").unwrap_or(&name),
            &sealed,
            Publish::CreateOnly("gc evidence"),
            &EVIDENCE_POINTS,
        )
    }

    fn evidence_passes(&self) -> Result<Vec<u64>> {
        Ok(list_hex_ids(&self.inner.path("gc"), "")
            .map_err(io_err)?
            .into_iter()
            .map(|(pass, _)| pass)
            .filter(|p| *p != 0)
            .collect())
    }
}
