//! Opening a volume: parse the descriptor, unlock the keys, run the attach
//! check and take the volume lock (DESIGN section 4.4, step 1).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lunix_crypto::keyring::{KeyFile, Unlock};
use lunix_crypto::{
    check_attach, AttachError, AttachRequest, Credential, UnlockOptions, VolumeCrypto,
};
use lunix_format::keyring::Keyring;
use lunix_format::local::LocalPinsFile;
use lunix_format::paths::{local as lpath, local_keyring_path};
use lunix_format::volume::VolumeDescriptor;
use lunix_format::CryptoMode;
use lunix_iface::crypto::{CommitCodec, ExtentCodec, MetaCodec};
use lunix_iface::{Error, ErrorKind};
use lunix_store::{read_volume_file, LocalVolume, StoreOptions};

use crate::code;
use crate::report::{Finding, Sink};
use crate::Options;

/// An opened, locked volume.
pub(crate) struct Session {
    pub(crate) dir: PathBuf,
    pub(crate) vol: LocalVolume,
    pub(crate) desc: VolumeDescriptor,
    pub(crate) crypto: VolumeCrypto,
    /// BLAKE3-256 of the exact `volume` bytes (the genesis `parent_hash`).
    pub(crate) volume_hash: [u8; 32],
    /// Where scratch directories go: next to the volume, or `Options::scratch_dir`.
    pub(crate) scratch_base: PathBuf,
    /// The `keyring_seq` of the keyring that unlocked the volume (0 for a
    /// plain volume); `--repair-pins` keeps it (P-4).
    pub(crate) keyring_seq: u64,
}

impl Session {
    pub(crate) fn commit_codec(&self) -> &dyn CommitCodec {
        self.crypto.meta().as_ref()
    }

    pub(crate) fn meta_codec(&self) -> &dyn MetaCodec {
        self.crypto.meta().as_ref()
    }

    pub(crate) fn extents(&self) -> &dyn ExtentCodec {
        self.crypto.extents().as_ref()
    }

    pub(crate) fn ovh(&self) -> u32 {
        self.desc.crypto_mode.extent_ovh()
    }
}

pub(crate) fn mode_name(mode: CryptoMode) -> &'static str {
    match mode {
        CryptoMode::Plain => "plain",
        CryptoMode::Integrity => "integrity",
        CryptoMode::Encrypted => "encrypted",
    }
}

fn io_finding(object: &str, e: &Error) -> Finding {
    Finding::new(code::IO, object, format!("{e}"))
}

enum Cand<'a> {
    KeyFile(PathBuf),
    Pass(&'a [u8]),
}

/// What the descriptor says, available even when the rest fails.
pub(crate) struct Identity {
    pub(crate) volume: String,
    pub(crate) mode: &'static str,
}

pub(crate) fn open(
    dir: &Path,
    opts: &Options,
    sink: &mut Sink,
    ident: &mut Option<Identity>,
) -> Option<Session> {
    let bytes = match read_volume_file(dir) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            sink.push(Finding::new(
                code::VOLUME_PARSE,
                "volume",
                "there is no volume descriptor in this directory",
            ));
            return None;
        }
        Err(e) => {
            sink.push(io_finding("volume", &e));
            return None;
        }
    };
    let desc = match VolumeDescriptor::decode(&bytes) {
        Ok(d) => d,
        Err(e) => {
            sink.push(Finding::new(code::VOLUME_PARSE, "volume", format!("{e}")));
            return None;
        }
    };
    *ident = Some(Identity {
        volume: desc.volume_uuid.to_string(),
        mode: mode_name(desc.crypto_mode),
    });

    // state/pins first: its pin and keyring_seq feed the attach check.
    let pins = match std::fs::read(dir.join(lpath::state_pins())) {
        // A rejected file is reported by the state check; here it only
        // means that no pin feeds the attach check.
        Ok(b) => LocalPinsFile::decode(&b).ok(),
        Err(_) => None,
    };

    let keyring = if desc.crypto_mode.is_keyed() {
        let parent = dir.parent().unwrap_or_else(|| Path::new("."));
        let path = local_keyring_path(&parent.to_string_lossy(), &desc.family_id);
        match std::fs::read(&path) {
            Ok(b) => {
                if let Err(e) = Keyring::decode(&b) {
                    sink.push(Finding::new(code::KEYRING_PARSE, "keyring", format!("{e}")));
                    return None;
                }
                Some(b)
            }
            Err(e) => {
                sink.push(Finding::new(
                    code::KEYRING_PARSE,
                    "keyring",
                    format!("cannot read the keyring of the family: {e}"),
                ));
                return None;
            }
        }
    } else {
        None
    };

    let mut cands: Vec<Cand<'_>> = Vec::new();
    if desc.crypto_mode.is_keyed() {
        if let Some(p) = &opts.credentials.key_file {
            cands.push(Cand::KeyFile(p.clone()));
        }
        if let Some(p) = &opts.credentials.passphrase {
            cands.push(Cand::Pass(p.expose()));
        }
        let default_key = dir.join(lpath::integrity_key());
        if default_key.exists()
            && !cands
                .iter()
                .any(|c| matches!(c, Cand::KeyFile(p) if *p == default_key))
        {
            cands.push(Cand::KeyFile(default_key));
        }
        if cands.is_empty() {
            sink.push(Finding::new(
                code::CREDENTIAL,
                "volume",
                "the volume is keyed and no credential was given (--key-file, --passphrase-env)",
            ));
            return None;
        }
    }

    let pin = pins.as_ref().and_then(|p| p.pins.pin);
    let seen = pins.as_ref().map_or(0, |p| p.pins.max_keyring_seq);
    let mut last_err: Option<AttachError> = None;
    let mut crypto = None;
    let mut keyring_seq = 0;
    let attempts = if cands.is_empty() { 1 } else { cands.len() };
    for i in 0..attempts {
        let key_holder;
        let credential = match cands.get(i) {
            None => None,
            Some(Cand::Pass(p)) => Some(unlock_credential(desc.crypto_mode, Unlock::Passphrase(p))),
            Some(Cand::KeyFile(p)) => match KeyFile::read(p) {
                Ok(k) => {
                    key_holder = k;
                    Some(unlock_credential(
                        desc.crypto_mode,
                        Unlock::KeyFile(&key_holder),
                    ))
                }
                Err(_) => {
                    last_err.get_or_insert(AttachError::WrongCredential);
                    continue;
                }
            },
        };
        let req = AttachRequest {
            credential,
            fingerprint: None,
            pin,
            keyring_seq_seen: seen,
            unlock: UnlockOptions::default(),
        };
        match check_attach(&desc, keyring.as_deref(), &req) {
            Ok(ok) => {
                keyring_seq = ok.keyring_seq.unwrap_or(0);
                crypto = Some(ok.crypto);
                break;
            }
            Err(e) => {
                // A wrong credential is the least informative: keep a better
                // reason if an earlier or later candidate gave one.
                if last_err.is_none() || !e.is_wrong_key_class() {
                    last_err = Some(e);
                }
            }
        }
    }
    let Some(crypto) = crypto else {
        let (c, msg) = match last_err {
            Some(AttachError::BadMac) => (
                code::VOLUME_MAC,
                "the descriptor MAC or checksum does not verify".to_owned(),
            ),
            Some(AttachError::KeyringRollback) => (
                code::KEYRING_ROLLBACK,
                "the keyring is older than the highest keyring_seq in state/pins".to_owned(),
            ),
            Some(AttachError::NeedCredential) => {
                (code::CREDENTIAL, "the volume needs a credential".to_owned())
            }
            Some(AttachError::WrongCredential) | None => (
                code::CREDENTIAL,
                "the credential unwraps no slot (wrong credential, or a damaged keyring)"
                    .to_owned(),
            ),
            Some(AttachError::Format(f)) => (code::KEYRING_PARSE, format!("{f}")),
            Some(other) => (code::ATTACH, format!("{other}")),
        };
        sink.push(Finding::new(c, "volume", msg));
        return None;
    };

    let meta: Arc<dyn MetaCodec> = crypto.meta().clone();
    let store_opts = StoreOptions {
        allow_tmpfs: true,
        // The one place that may lower the rollback floor (offline repair),
        // and only for the repairs that do (M14 re-review): a plain check
        // opens the store without it.
        allow_floor_reset: opts.repair
            && (opts.truncate_tip || opts.reset_floor || opts.repair_pins),
        ..StoreOptions::default()
    };
    let vol = match LocalVolume::open(dir, meta, store_opts) {
        Ok(v) => v,
        Err(e) if e.kind() == ErrorKind::Busy => {
            sink.push(Finding::new(
                code::BUSY,
                "LOCK",
                "another process holds the volume lock; fsck is offline-only",
            ));
            return None;
        }
        Err(e) => {
            sink.push(io_finding("volume", &e));
            return None;
        }
    };
    let volume_hash = crypto.meta().object_hash(&bytes);
    Some(Session {
        dir: dir.to_path_buf(),
        vol,
        desc,
        crypto,
        volume_hash,
        keyring_seq,
        scratch_base: opts
            .scratch_dir
            .clone()
            .or_else(|| dir.parent().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir),
    })
}

fn unlock_credential(mode: CryptoMode, u: Unlock<'_>) -> Credential<'_> {
    if mode == CryptoMode::Encrypted {
        Credential::Key(u)
    } else {
        Credential::Integrity(u)
    }
}
