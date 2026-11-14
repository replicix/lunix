//! A relative volume path: the keyring is looked up beside the absolute
//! volume directory (`Path::new("vol").parent()` is the empty path).

mod common;

use common::*;
use lunix_format::CryptoMode;

#[test]
fn a_relative_path_of_one_component_finds_the_keyring() {
    let b = build(&Spec::new(CryptoMode::Integrity, 16 * 1024));
    let parent = b.dir.parent().expect("parent").to_path_buf();
    let name = b.dir.file_name().expect("name").to_owned();
    std::env::set_current_dir(&parent).expect("cd");
    let r = lunix_fsck::check(std::path::Path::new(&name), &b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    std::env::set_current_dir(&b.dir).expect("cd");
    let r = lunix_fsck::check(std::path::Path::new("."), &b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
}
