//! The size parser against `u128` arithmetic.

use lunix_cli::parse::parse_size;
use proptest::prelude::*;

fn mult(c: char) -> u128 {
    match c.to_ascii_uppercase() {
        'K' => 1 << 10,
        'M' => 1 << 20,
        'G' => 1 << 30,
        'T' => 1 << 40,
        _ => 1,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn matches_wide_arithmetic(digits in "[0-9]{1,25}", suffix in "[KMGTkmgt]?") {
        let m = suffix.chars().next().map_or(1, mult);
        let wide = digits.parse::<u128>().unwrap() * m;
        let s = format!("{digits}{suffix}");
        match parse_size(&s) {
            Ok(v) => {
                prop_assert!(wide >= 1 && wide <= u128::from(u64::MAX));
                prop_assert_eq!(u128::from(v), wide);
            }
            Err(_) => prop_assert!(wide == 0 || wide > u128::from(u64::MAX)),
        }
    }

    #[test]
    fn plain_numbers_round_trip(n in 1u64..) {
        prop_assert_eq!(parse_size(&n.to_string()), Ok(n));
    }

    #[test]
    fn anything_accepted_has_the_documented_shape(s in "\\PC{0,12}") {
        if parse_size(&s).is_ok() {
            let body = s.trim_end_matches(['K', 'M', 'G', 'T', 'k', 'm', 'g', 't']);
            prop_assert!(!body.is_empty());
            prop_assert!(body.bytes().all(|b| b.is_ascii_digit()));
            prop_assert!(s.len() - body.len() <= 1);
        }
    }

    #[test]
    fn fractions_signs_and_spaces_are_rejected(n in 1u32..1000, frac in 1u32..10) {
        let cases = [
            format!("{n}.{frac}G"),
            format!("-{n}G"),
            format!("+{n}"),
            format!(" {n}"),
            format!("{n} "),
            format!("{n}GB"),
        ];
        for c in cases {
            prop_assert!(parse_size(&c).is_err(), "{c:?}");
        }
    }
}
