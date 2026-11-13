//! R1 API test: the engine has exactly one store read call site, and it
//! takes an expectation (`Expectation` has no "none" case; a lean volume
//! reads as a scan with `verify_id`).

const SOURCES: &[(&str, &str)] = &[
    ("cache.rs", include_str!("../src/cache.rs")),
    ("config.rs", include_str!("../src/config.rs")),
    ("coord.rs", include_str!("../src/coord.rs")),
    ("deps.rs", include_str!("../src/deps.rs")),
    ("destage.rs", include_str!("../src/destage.rs")),
    ("engine.rs", include_str!("../src/engine.rs")),
    ("error.rs", include_str!("../src/error.rs")),
    ("flush.rs", include_str!("../src/flush.rs")),
    ("lib.rs", include_str!("../src/lib.rs")),
    ("points.rs", include_str!("../src/points.rs")),
    ("queue.rs", include_str!("../src/queue.rs")),
    ("read.rs", include_str!("../src/read.rs")),
    ("recover.rs", include_str!("../src/recover.rs")),
    ("stats.rs", include_str!("../src/stats.rs")),
];

fn code_lines(src: &str) -> impl Iterator<Item = &str> {
    src.lines()
        .map(str::trim_start)
        .filter(|l| !l.starts_with("//"))
}

#[test]
fn the_only_extent_read_is_the_r1_reader() {
    let mut sites = Vec::new();
    for (name, src) in SOURCES {
        for l in code_lines(src) {
            if l.contains(".read_extent(") {
                sites.push((*name, l.to_owned()));
            }
        }
    }
    // The tracking wrapper forwards; the reader is the one real call.
    let reads: Vec<_> = sites
        .iter()
        .filter(|(_, l)| !l.contains("self.inner.read_extent"))
        .collect();
    assert_eq!(reads.len(), 1, "{sites:?}");
    assert_eq!(reads[0].0, "read.rs");
    assert!(reads[0].1.contains("self.segments.read_extent"));
}

#[test]
fn every_open_expectation_names_an_id_or_scans() {
    let read = SOURCES
        .iter()
        .find(|(n, _)| *n == "read.rs")
        .map(|(_, s)| *s)
        .unwrap_or_default();
    // One OpenExpect in the engine, built from an `Expectation`.
    let n = SOURCES
        .iter()
        .map(|(_, s)| code_lines(s).filter(|l| l.contains("OpenExpect {")).count())
        .sum::<usize>();
    assert_eq!(n, 1);
    assert!(read.contains("verify_id: self.verify_reads || scan"));
    assert!(!read.contains("Expectation::None"));
}

#[test]
fn every_crash_point_is_declared_once_and_listed() {
    let mut names = Vec::new();
    for (_, src) in SOURCES {
        for l in code_lines(src) {
            for marker in ["crashpoint!(\"", "point!(self.points, \""] {
                if let Some(i) = l.find(marker) {
                    let rest = &l[i + marker.len()..];
                    if let Some(j) = rest.find('"') {
                        names.push(rest[..j].to_owned());
                    }
                }
            }
        }
    }
    for p in lunix_engine::CRASH_POINTS {
        assert!(names.iter().any(|n| n == p), "{p} is not in the code");
    }
    for n in &names {
        assert!(
            lunix_engine::CRASH_POINTS.contains(&n.as_str()),
            "{n} is not listed"
        );
    }
}
