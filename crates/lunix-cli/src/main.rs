//! The `lunix` binary.

use std::process::ExitCode;

use clap::Parser;
use lunix_cli::{run, Cli};

fn main() -> ExitCode {
    let cli = Cli::parse();
    run(
        cli,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}
