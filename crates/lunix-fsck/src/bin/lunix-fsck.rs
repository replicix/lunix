//! `lunix-fsck`: offline checker and repairer of a local volume.

use std::path::PathBuf;
use std::process::ExitCode;

use lunix_fsck::{check, Credentials, Options};
use lunix_iface::ctl::Secret;

const USAGE: &str = "\
usage: lunix-fsck [options] <volume-dir>

  --deep                 open every extent (tag or AEAD, id)
  --json                 machine-readable output
  --key-file <path>      key file of a keyed volume
  --passphrase-fd <N>    read the passphrase (bytes, one trailing newline removed)
                         from file descriptor N; never from argv or the environment
  --scratch-dir <dir>    where the private scratch copy of the index goes
                         (default: next to the volume directory)
  --rebuild-index        rewrite the index from the footers
  --repair               safe repairs: remove *.tmp and *.open, rebuild a
                         damaged index, rewrite a damaged checkpoint
  --delete-orphans       with --repair: delete unreferenced segments and packs
  --gc                   with --repair: finish a pending GC pass
  --truncate-tip         with --repair: move the damaged tip aside, make the
                         previous commit the tip, zero the checkpoint and
                         lower the rollback floor (loses journal records C9
                         already reclaimed)
  --reset-floor          with --repair: set the rollback floor to the verified tip
  --repair-pins          with --repair: replace a rejected state/pins from the
                         verified tip (gives up max_keyring_seq, max_epoch, the pin)

exit status: 0 clean, 1 warnings only, 2 errors, 64 usage
";

fn main() -> ExitCode {
    let mut opts = Options::default();
    let mut json = false;
    let mut dir: Option<PathBuf> = None;
    let mut creds = Credentials::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--deep" => opts.deep = true,
            "--json" => json = true,
            "--rebuild-index" => opts.rebuild_index = true,
            "--repair" => opts.repair = true,
            "--delete-orphans" => opts.delete_orphans = true,
            "--gc" => opts.gc = true,
            "--truncate-tip" => opts.truncate_tip = true,
            "--reset-floor" => opts.reset_floor = true,
            "--key-file" => match args.next() {
                Some(p) => creds.key_file = Some(PathBuf::from(p)),
                None => return usage("--key-file needs a path"),
            },
            "--passphrase-fd" => match args.next().and_then(|v| v.parse::<u32>().ok()) {
                Some(fd) => match read_passphrase_fd(fd) {
                    Ok(p) => creds.passphrase = Some(p),
                    Err(why) => return usage(&why),
                },
                None => return usage("--passphrase-fd needs a descriptor number"),
            },
            "--scratch-dir" => match args.next() {
                Some(p) => opts.scratch_dir = Some(PathBuf::from(p)),
                None => return usage("--scratch-dir needs a path"),
            },
            "--repair-pins" => opts.repair_pins = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            s if s.starts_with("--") => return usage("unknown option"),
            _ => {
                if dir.replace(PathBuf::from(a)).is_some() {
                    return usage("one volume directory only");
                }
            }
        }
    }
    let Some(dir) = dir else {
        return usage("missing the volume directory");
    };
    opts.credentials = creds;
    let report = check(&dir, &opts);
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.render_text());
    }
    ExitCode::from(u8::try_from(report.exit_code()).unwrap_or(2))
}

fn usage(why: &str) -> ExitCode {
    eprintln!("lunix-fsck: {why}\n\n{USAGE}");
    ExitCode::from(64)
}

/// The longest passphrase accepted.
const MAX_PASSPHRASE: usize = 4096;

/// Reads descriptor `fd` through `/proc/self/fd` (no unsafe code: the crate
/// forbids it) into one preallocated buffer, so nothing is copied by a
/// reallocation. Any bytes are accepted; one trailing `\n` (or `\r\n`) goes.
fn read_passphrase_fd(fd: u32) -> Result<Secret, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(format!("/proc/self/fd/{fd}"))
        .map_err(|e| format!("cannot read descriptor {fd}: {e}"))?;
    let mut buf: Vec<u8> = Vec::with_capacity(MAX_PASSPHRASE + 2);
    f.by_ref()
        .take(MAX_PASSPHRASE as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read descriptor {fd}: {e}"))?;
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    if buf.is_empty() || buf.len() > MAX_PASSPHRASE {
        return Err("the passphrase is empty or longer than 4096 bytes".to_owned());
    }
    Ok(Secret::new(buf))
}
