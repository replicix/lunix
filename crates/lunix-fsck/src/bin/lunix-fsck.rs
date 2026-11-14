//! `lunix-fsck`: offline checker and repairer of a local volume.

use std::path::PathBuf;
use std::process::ExitCode;

use lunix_fsck::{check, Credentials, Options};

const USAGE: &str = "\
usage: lunix-fsck [options] <volume-dir>

  --deep                 open every extent (tag or AEAD, id)
  --json                 machine-readable output
  --key-file <path>      key file of a keyed volume
  --passphrase-env <V>   read the passphrase from environment variable V
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
            "--passphrase-env" => match args.next() {
                Some(v) => match std::env::var_os(&v) {
                    Some(p) => {
                        creds.passphrase = Some(p.to_string_lossy().into_owned().into_bytes())
                    }
                    None => return usage("the passphrase variable is not set"),
                },
                None => return usage("--passphrase-env needs a variable name"),
            },
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
