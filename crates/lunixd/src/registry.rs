//! The host registry `<state_dir>/volumes.json` (D8): name → uuid, volume
//! directory. Replaced atomically; rebuildable by scanning volume
//! directories for `volume.json`.

use std::path::{Path, PathBuf};

use lunix_format::local::{RegistryEntry, VolumeJson, VolumesJson};
use lunix_format::VolumeUuid;

use crate::fsio;

/// The registry file's largest accepted size.
const MAX_REGISTRY: u64 = 16 << 20;

/// Why the registry could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The file system.
    #[error("registry i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The file is not a valid `volumes.json`.
    #[error("registry is invalid: {0}")]
    Format(#[from] lunix_format::FormatError),
}

/// The registry in memory, backed by its file.
#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
    data: VolumesJson,
}

impl Registry {
    /// Loads `path`; a missing file is an empty registry.
    ///
    /// # Errors
    /// I/O, or an invalid file (the caller may [`rebuild`](Self::rebuild)).
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        let data = match fsio::read_capped(path, MAX_REGISTRY)? {
            None => VolumesJson {
                version: 1,
                volumes: Vec::new(),
            },
            Some(bytes) => {
                let text = String::from_utf8(bytes)
                    .map_err(|_| lunix_format::FormatError::invalid("volumes.json", "not UTF-8"))?;
                VolumesJson::from_json(&text)?
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            data,
        })
    }

    /// Writes the registry atomically (tmp, fdatasync, rename, dir fsync).
    ///
    /// # Errors
    /// I/O or validation.
    pub fn save(&self) -> Result<(), RegistryError> {
        let text = self.data.to_json_pretty()?;
        if let Some(dir) = self.path.parent() {
            fsio::create_dir_durable(dir)?;
        }
        fsio::replace_durable(&self.path, text.as_bytes(), 0o600)?;
        Ok(())
    }

    /// Every entry.
    pub fn entries(&self) -> &[RegistryEntry] {
        &self.data.volumes
    }

    /// The entry named `name`.
    pub fn find(&self, name: &str) -> Option<&RegistryEntry> {
        self.data.volumes.iter().find(|e| e.name == name)
    }

    /// Adds an entry (in memory; call [`save`](Self::save)).
    ///
    /// # Errors
    /// A duplicate name (validation).
    pub fn insert(&mut self, e: RegistryEntry) -> Result<(), RegistryError> {
        let mut next = self.data.clone();
        next.volumes.push(e);
        next.validate()?;
        self.data = next;
        Ok(())
    }

    /// Removes the entry named `name`; true if one was removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.data.volumes.len();
        self.data.volumes.retain(|e| e.name != name);
        self.data.volumes.len() != before
    }

    /// Rebuilds a registry for `path` from the volume directories under
    /// `roots` (`<root>/<uuid>/volume.json`). Directories whose name is not a
    /// uuid, or whose `volume.json` does not parse, are skipped and returned
    /// for the log; a second volume with a name already taken is skipped too.
    pub fn rebuild(path: &Path, roots: &[PathBuf]) -> (Self, Vec<String>) {
        let mut data = VolumesJson {
            version: 1,
            volumes: Vec::new(),
        };
        let mut skipped = Vec::new();
        for root in roots {
            let Ok(rd) = std::fs::read_dir(root) else {
                skipped.push(format!("{}: unreadable", root.display()));
                continue;
            };
            let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
            dirs.sort();
            for d in dirs {
                let Some(uuid) = d
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.parse::<VolumeUuid>().ok())
                else {
                    continue;
                };
                let json = fsio::read_capped(&d.join("volume.json"), 1 << 20)
                    .ok()
                    .flatten()
                    .and_then(|b| String::from_utf8(b).ok())
                    .and_then(|t| VolumeJson::from_json(&t).ok());
                let Some(json) = json else {
                    skipped.push(format!("{}: no valid volume.json", d.display()));
                    continue;
                };
                if data.volumes.iter().any(|e| e.name == json.name) {
                    skipped.push(format!(
                        "{}: name {:?} already taken",
                        d.display(),
                        json.name
                    ));
                    continue;
                }
                data.volumes.push(RegistryEntry {
                    name: json.name,
                    uuid: uuid.to_string(),
                    directory: d.to_string_lossy().into_owned(),
                });
            }
        }
        (
            Self {
                path: path.to_path_buf(),
                data,
            },
            skipped,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, uuid: &str) -> RegistryEntry {
        RegistryEntry {
            name: name.into(),
            uuid: uuid.into(),
            directory: format!("/v/{uuid}"),
        }
    }

    const U1: &str = "6f1c2a4e-1b2c-4d5e-8f90-0123456789ab";
    const U2: &str = "7f1c2a4e-1b2c-4d5e-8f90-0123456789ab";

    #[test]
    fn insert_save_load_remove() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("state/volumes.json");
        let mut r = Registry::load(&p).unwrap();
        assert!(r.entries().is_empty());
        r.insert(entry("a", U1)).unwrap();
        assert!(r.insert(entry("a", U2)).is_err(), "duplicate name");
        r.insert(entry("b", U2)).unwrap();
        r.save().unwrap();
        let r2 = Registry::load(&p).unwrap();
        assert_eq!(r2.entries().len(), 2);
        assert_eq!(r2.find("b").unwrap().uuid, U2);
        let mut r3 = r2;
        assert!(r3.remove("a"));
        assert!(!r3.remove("a"));
        r3.save().unwrap();
        assert_eq!(Registry::load(&p).unwrap().entries().len(), 1);
    }

    #[test]
    fn rebuild_scans_volume_directories() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("vols");
        for (u, name) in [(U1, "one"), (U2, "two")] {
            let d = root.join(u);
            std::fs::create_dir_all(&d).unwrap();
            let j = VolumeJson::new(name, &d.to_string_lossy());
            std::fs::write(d.join("volume.json"), j.to_json_pretty().unwrap()).unwrap();
        }
        std::fs::create_dir_all(root.join("families")).unwrap();
        let (r, skipped) = Registry::rebuild(&t.path().join("volumes.json"), &[root]);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(r.find("one").unwrap().uuid, U1);
        assert_eq!(r.find("two").unwrap().uuid, U2);
        r.save().unwrap();
    }
}
