//! Step 1: `volume.json`, `state/*`, the pin objects and the GC list.

use std::fs;

use lunix_format::gcfile::Condemned;
use lunix_format::local::{CheckpointFile, LocalPinsFile, CKP_SLOT_LEN};
use lunix_format::paths::local as lpath;
use lunix_iface::store::{PinSource, VolumeDir};
use lunix_iface::ErrorKind;

use crate::code;
use crate::facts::Facts;
use crate::report::{Finding, Sink};
use crate::session::Session;

pub(crate) fn check(s: &Session, sink: &mut Sink, facts: &mut Facts) {
    match s.vol.volume_json() {
        Ok(j) => facts.json = Some(j),
        Err(e) => sink.push(Finding::new(
            code::VOLUME_JSON,
            "volume.json",
            format!("{e}"),
        )),
    }
    // Read fresh: a repair rewrites it between two runs.
    let pins = match fs::read(s.dir.join(lpath::state_pins())) {
        Ok(b) => match LocalPinsFile::decode(&b) {
            Ok(p) => Some(p),
            Err(e) => {
                sink.push(Finding::new(code::STATE_PINS, "state/pins", format!("{e}")));
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            sink.push(Finding::new(
                code::IO,
                "state/pins",
                format!("cannot read: {e}"),
            ));
            None
        }
    };
    facts.floor = pins.as_ref().and_then(|p| p.floor);

    match s.vol.device_record() {
        Ok(Some(rec)) => {
            if rec.volume_uuid != s.desc.volume_uuid {
                sink.push(Finding::new(
                    code::STATE_DEVICE,
                    "state/device",
                    "the device record names another volume",
                ));
            }
        }
        Ok(None) => {}
        Err(e) => sink.push(Finding::new(
            code::STATE_DEVICE,
            "state/device",
            format!("{e}"),
        )),
    }
    if let Some(p) = &pins {
        if p.pins.volume_uuid != s.desc.volume_uuid || p.pins.family_id != s.desc.family_id {
            sink.push(Finding::new(
                code::STATE_PINS,
                "state/pins",
                "state/pins names another volume or family",
            ));
        }
    }

    if let Err(e) = s.vol.roots() {
        sink.push(Finding::new(
            code::PIN_OBJECT,
            "snapshots/ readers/ holds/",
            format!("{e}"),
        ));
    }

    match fs::read(s.dir.join(lpath::state_gc_condemned())) {
        Ok(b) => match Condemned::decode(&b) {
            Ok(c) => facts.condemned = Some(c),
            Err(e) => sink.push(Finding::new(
                code::CONDEMNED_INVALID,
                "state/gc/condemned",
                format!("{e}"),
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => sink.push(Finding::new(
            code::IO,
            "state/gc/condemned",
            format!("cannot read: {e}"),
        )),
    }

    match s.vol.leftovers() {
        Ok(l) => {
            for lf in &l {
                sink.push(Finding::new(
                    code::LEFTOVER,
                    lf.name.clone(),
                    format!("{} bytes", lf.size_bytes),
                ));
            }
            let device_present = matches!(s.vol.device_record(), Ok(Some(_)) | Err(_));
            facts.closed_cleanly = l.is_empty() && !device_present;
            facts.leftovers = l;
        }
        Err(e) if e.kind() == ErrorKind::Io => {
            sink.push(Finding::new(code::IO, "volume", format!("{e}")));
        }
        Err(e) => sink.push(Finding::new(code::IO, "volume", format!("{e}"))),
    }
}

/// The checkpoint, once the tip is known (`tip` is its `commit_seq`).
pub(crate) fn check_checkpoint(s: &Session, tip: Option<u64>, sink: &mut Sink) {
    let path = s.dir.join(lpath::state_checkpoint());
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            sink.push(Finding::new(
                code::IO,
                "state/checkpoint",
                format!("cannot read: {e}"),
            ));
            return;
        }
    };
    if bytes.iter().all(|b| *b == 0) {
        return;
    }
    let file = match CheckpointFile::decode(&bytes) {
        Ok(f) => f,
        Err(e) => {
            sink.push(Finding::new(
                code::CHECKPOINT,
                "state/checkpoint",
                format!("ignored by mount: {e}"),
            ));
            return;
        }
    };
    for (i, slot) in file.slots.iter().enumerate() {
        if let Err(e) = slot {
            let part = bytes.get(i * CKP_SLOT_LEN..(i + 1) * CKP_SLOT_LEN);
            if part.is_none_or(|p| p.iter().any(|b| *b != 0)) {
                sink.push(
                    Finding::new(
                        code::CHECKPOINT,
                        "state/checkpoint",
                        format!("slot {i} is damaged ({e}); the other slot is used"),
                    )
                    .with_range((i * CKP_SLOT_LEN) as u64, ((i + 1) * CKP_SLOT_LEN) as u64),
                );
            }
        }
    }
    if let Some((_, slot)) = file.newest_valid() {
        if slot.volume_uuid != s.desc.volume_uuid {
            sink.push(Finding::new(
                code::CHECKPOINT,
                "state/checkpoint",
                "the newest slot was written for another volume (ignored)",
            ));
        } else if tip.is_some_and(|t| slot.last_commit_seq > t) {
            sink.push(Finding::new(
                code::CHECKPOINT,
                "state/checkpoint",
                format!(
                    "the newest slot describes commit {} above the tip: mount ignores its lane tails (D17); repair rewrites it",
                    slot.last_commit_seq
                ),
            ));
        }
    }
}
