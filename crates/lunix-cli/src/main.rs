//! The `lunix` binary.

use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::Parser;
use lunix_cli::{run, Cli};

/// Exit code of a usage error of `lunix fsck`: 2 means "errors found" there, so
/// a bad command line is 64, as in the `lunix-fsck` binary.
const FSCK_USAGE: u8 = 64;

/// True when the first word that is not a global option (and not its value) is
/// `fsck`.
fn is_fsck(args: &[String]) -> bool {
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socket" | "--timeout" => {
                it.next();
            }
            s if s.starts_with("--socket=") || s.starts_with("--timeout=") => {}
            s if s.starts_with('-') => {}
            s => return s == "fsck",
        }
    }
    false
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            if is_fsck(&args)
                && !matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion)
            {
                let _ = e.print();
                return ExitCode::from(FSCK_USAGE);
            }
            e.exit()
        }
    };
    run(
        cli,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}
