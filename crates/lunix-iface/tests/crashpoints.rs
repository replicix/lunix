//! `crashpoint!`: off compiles to nothing; on registers names and kills the
//! process at the n-th hit.

use lunix_iface::crashpoint;

/// Both builds: the macro is a statement and an expression-free no-op shape.
#[test]
fn macro_is_usable_as_a_statement() {
    crashpoint!("test.statement_form");
    let v = {
        crashpoint!("test.in_block");
        5
    };
    assert_eq!(v, 5);
}

#[cfg(not(feature = "crashpoints"))]
#[test]
fn feature_off_registers_nothing_and_ignores_the_environment() {
    assert!(lunix_iface::crashpoint::registered().is_empty());
    // Even a matching LUNIX_CRASH_AT could not fire: there is no code to read it.
    for _ in 0..3 {
        crashpoint!("test.never_fires");
    }
}

#[cfg(feature = "crashpoints")]
mod on {
    use std::process::Command;

    use lunix_iface::crashpoint;
    use lunix_iface::crashpoint::{check_registry, counts, registered};

    fn touch_points() {
        crashpoint!("test.alpha");
        crashpoint!("test.beta");
    }

    #[test]
    fn names_are_registered_listed_and_unique() {
        touch_points();
        let names = registered();
        assert!(names.contains(&"test.alpha") && names.contains(&"test.beta"));
        assert!(names.windows(2).all(|w| w[0] <= w[1]), "sorted");
        assert_eq!(check_registry(), Ok(()));
        assert!(counts().contains_key("test.alpha"));
    }

    /// Child body: hits `test.child` five times, printing a line before each.
    #[test]
    fn child_body() {
        if std::env::var_os("LUNIX_CRASH_CHILD").is_none() {
            return;
        }
        for i in 1..=5 {
            println!("before hit {i}");
            crashpoint!("test.child");
            println!("after hit {i}");
        }
        println!("survived");
    }

    fn run_child(crash_at: &str) -> (Option<i32>, String) {
        use std::os::unix::process::ExitStatusExt;
        let exe = std::env::current_exe().expect("exe");
        let out = Command::new(exe)
            .args([
                "--exact",
                "on::child_body",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("LUNIX_CRASH_CHILD", "1")
            .env("LUNIX_CRASH_AT", crash_at)
            .output()
            .expect("spawn child");
        (
            out.status.signal(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    #[test]
    fn kills_at_the_nth_hit() {
        let (sig, out) = run_child("test.child:3");
        assert_eq!(sig, Some(9), "SIGKILL expected, output: {out}");
        assert!(out.contains("after hit 2") && out.contains("before hit 3"));
        assert!(!out.contains("after hit 3") && !out.contains("survived"));
    }

    #[test]
    fn default_hit_is_the_first() {
        let (sig, out) = run_child("test.child");
        assert_eq!(sig, Some(9));
        assert!(out.contains("before hit 1") && !out.contains("after hit 1"));
    }

    #[test]
    fn other_names_and_bad_specs_do_not_fire() {
        for spec in [
            "test.other:1",
            "test.child:0",
            "test.child:x",
            "",
            "test.child:99",
        ] {
            let (sig, out) = run_child(spec);
            assert_eq!(sig, None, "spec {spec:?}");
            assert!(out.contains("survived"), "spec {spec:?}");
        }
    }
}
