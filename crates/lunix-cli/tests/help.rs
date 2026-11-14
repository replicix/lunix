//! Snapshots of `--help` for every command, and the documented defaults.

mod common;

use common::{assert_snapshot, run_bare};

const COMMANDS: &[&[&str]] = &[
    &[],
    &["volume"],
    &["volume", "create"],
    &["volume", "list"],
    &["attach"],
    &["detach"],
    &["status"],
    &["resize"],
    &["space"],
    &["gc"],
    &["fsck"],
    &["key"],
    &["key", "ls"],
    &["key", "add-slot"],
    &["key", "rm-slot"],
];

#[test]
fn help_of_every_command_matches_its_snapshot() {
    for cmd in COMMANDS {
        let mut args: Vec<&str> = cmd.to_vec();
        args.push("--help");
        let o = run_bare(&args);
        assert_eq!(o.code, 0, "{cmd:?}: {}", o.stderr);
        let name = if cmd.is_empty() {
            "help.txt".to_owned()
        } else {
            format!("help-{}.txt", cmd.join("-"))
        };
        assert_snapshot(&name, &o.stdout);
    }
}

#[test]
fn create_help_documents_the_default_mode() {
    let o = run_bare(&["volume", "create", "--help"]);
    assert!(o.stdout.contains("--integrity"), "{}", o.stdout);
    assert!(o.stdout.contains("(the default)"), "{}", o.stdout);
    assert!(o.stdout.contains("integrity.key"), "{}", o.stdout);
    assert!(o.stdout.contains("(default: integrity)"), "{}", o.stdout);
}

#[test]
fn help_lists_the_exit_codes() {
    let o = run_bare(&["--help"]);
    for code in [
        "0  success",
        "3  busy",
        "4  not found",
        "5  no space",
        "6  downgrade",
    ] {
        assert!(o.stdout.contains(code), "{code}");
    }
}
