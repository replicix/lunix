//! Rules of ADR-11 that a source scan can check: no public function takes a
//! salt or a nonce; the injectable random source exists only behind the
//! `test-rng` feature; the only `unsafe` is in the key arena.

use std::path::{Path, PathBuf};

fn sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        for e in std::fs::read_dir(dir).expect("read_dir") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&p).expect("read");
                out.push((p, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    out
}

/// The parameter list of every `pub fn` in `text`, with the three lines above
/// it (for attributes).
fn pub_fns(text: &str) -> Vec<(String, String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if !(t.starts_with("pub fn ") || t.starts_with("pub(crate) fn ")) {
            continue;
        }
        let from =
            text.lines().take(i).map(|l| l.len() + 1).sum::<usize>() + (line.len() - t.len());
        let rest = &text[from..];
        let Some(open) = rest.find('(') else { continue };
        let mut depth = 0usize;
        let mut end = open;
        for (k, c) in rest[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + k;
                        break;
                    }
                }
                _ => {}
            }
        }
        let name = rest[..open]
            .trim()
            .rsplit(' ')
            .next()
            .unwrap_or("")
            .to_owned();
        let params = rest[open + 1..end].to_owned();
        let above = lines[i.saturating_sub(3)..i].join("\n");
        found.push((name, params, above));
    }
    found
}

#[test]
fn no_function_takes_a_salt_or_a_nonce_outside_the_test_hooks() {
    let mut checked = 0;
    for (path, text) in sources() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "testing.rs" {
            continue;
        }
        for (f, params, above) in pub_fns(&text) {
            checked += 1;
            let lower = params.to_lowercase();
            let gated = above.contains("feature = \"test-rng\"");
            if !gated {
                assert!(
                    !lower.contains("salt") && !lower.contains("nonce"),
                    "{}: `{f}` takes a salt or a nonce: {params}",
                    path.display()
                );
            }
        }
    }
    assert!(checked > 40, "the scan found only {checked} functions");
}

#[test]
fn the_injectable_random_source_is_behind_the_test_feature() {
    for (path, text) in sources() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        for (f, params, above) in pub_fns(&text) {
            if params.contains("Rng") || f == "with_rng" {
                assert!(
                    above.contains("feature = \"test-rng\"") || name == "testing.rs",
                    "{}: `{f}` accepts a random source outside test-rng",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn unsafe_is_confined_to_the_arena() {
    for (path, text) in sources() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let code: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let uses_unsafe =
            code.contains("unsafe {") || code.contains("unsafe fn") || code.contains("unsafe impl");
        if name == "arena.rs" {
            assert!(uses_unsafe, "the arena is expected to hold the unsafe");
        } else {
            assert!(!uses_unsafe, "{}: unsafe outside the arena", path.display());
        }
    }
}

#[test]
fn randomness_comes_from_getrandom_not_a_userspace_drbg() {
    for (path, text) in sources() {
        let code: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("aws_lc_rs::rand") && !code.contains("RAND_bytes"),
            "{}: the AWS-LC DRBG must not seed salts or nonces",
            path.display()
        );
        if path.ends_with("rng.rs") {
            assert!(code.contains("getrandom"), "rng.rs must call getrandom(2)");
        }
    }
}
