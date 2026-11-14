//! Parsers and formatters for sizes, durations, floors and compression.

use lunix_format::local::FreeFloor;
use lunix_iface::ctl::Compress;

/// Parses a size: decimal digits and an optional `K`, `M`, `G` or `T` suffix
/// (powers of 1024, either case). Rejects zero, signs, fractions, spaces and
/// overflow.
///
/// # Errors
/// A message naming the problem (never echoing more than the input).
pub fn parse_size(s: &str) -> Result<u64, String> {
    let n = parse_size_or_zero(s)?;
    if n == 0 {
        return Err("a size must be greater than zero".to_owned());
    }
    Ok(n)
}

/// As [`parse_size`] but zero is allowed (a floor of 0 bytes).
///
/// # Errors
/// As [`parse_size`].
pub fn parse_size_or_zero(s: &str) -> Result<u64, String> {
    let bad = || format!("`{s}` is not a size (digits with an optional K, M, G or T suffix)");
    let (digits, shift) = match s.as_bytes().last() {
        Some(b'K' | b'k') => (&s[..s.len() - 1], 10),
        Some(b'M' | b'm') => (&s[..s.len() - 1], 20),
        Some(b'G' | b'g') => (&s[..s.len() - 1], 30),
        Some(b'T' | b't') => (&s[..s.len() - 1], 40),
        Some(_) => (s, 0),
        None => return Err(bad()),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("`{s}` is too large (more than 2^64 - 1 bytes)"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("`{s}` is too large (more than 2^64 - 1 bytes)"))
}

/// A size that must fit a `u32` (chunk size, `max_io_buf`).
///
/// # Errors
/// As [`parse_size`], and for a value above `u32::MAX`.
pub fn parse_size_u32(s: &str) -> Result<u32, String> {
    let n = parse_size(s)?;
    u32::try_from(n).map_err(|_| format!("`{s}` is too large here (at most 4G - 1)"))
}

/// Parses a duration with a mandatory unit (`ms`, `s`, `m`, `h`) into
/// milliseconds. Zero is rejected.
///
/// # Errors
/// A message naming the problem.
pub fn parse_duration_ms(s: &str) -> Result<u64, String> {
    let bad = || format!("`{s}` is not a duration (digits and a unit: ms, s, m or h)");
    let (digits, mult) = if let Some(d) = s.strip_suffix("ms") {
        (d, 1u64)
    } else if let Some(d) = s.strip_suffix('s') {
        (d, 1_000)
    } else if let Some(d) = s.strip_suffix('m') {
        (d, 60_000)
    } else if let Some(d) = s.strip_suffix('h') {
        (d, 3_600_000)
    } else {
        return Err(bad());
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| format!("`{s}` is too large"))?;
    let ms = n
        .checked_mul(mult)
        .ok_or_else(|| format!("`{s}` is too large"))?;
    if ms == 0 {
        return Err("a timeout must be greater than zero".to_owned());
    }
    Ok(ms)
}

/// Parses a free-space floor: `N%` (0 to 100) or a size in bytes (zero
/// allowed).
///
/// # Errors
/// A message naming the problem.
pub fn parse_floor(s: &str) -> Result<FreeFloor, String> {
    if let Some(p) = s.strip_suffix('%') {
        let ok = !p.is_empty()
            && p.len() <= 3
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| n <= 100);
        if !ok {
            return Err(format!("`{s}` is not a percentage from 0% to 100%"));
        }
        return Ok(FreeFloor::Percent(s.to_owned()));
    }
    parse_size_or_zero(s).map(FreeFloor::Bytes)
}

/// Parses `auto`, `lz4`, `zstd:N` (N from -7 to 22) or `none`.
///
/// # Errors
/// A message naming the problem.
pub fn parse_compress(s: &str) -> Result<Compress, String> {
    match s {
        "auto" => Ok(Compress::Auto),
        "lz4" => Ok(Compress::Lz4),
        "none" => Ok(Compress::None),
        _ => {
            let level = s
                .strip_prefix("zstd:")
                .and_then(|l| l.parse::<i8>().ok())
                .filter(|l| (-7..=22).contains(l));
            level
                .map(|level| Compress::Zstd { level })
                .ok_or_else(|| format!("`{s}` is not auto, lz4, zstd:N (N from -7 to 22) or none"))
        }
    }
}

/// Parses the map id-prefix length: 16 or 32.
///
/// # Errors
/// A message for anything else.
pub fn parse_id_len(s: &str) -> Result<u8, String> {
    match s {
        "16" => Ok(16),
        "32" => Ok(32),
        _ => Err(format!("`{s}` is not 16 or 32")),
    }
}

const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/// Formats bytes for people: `200 GiB`, `1.5 MiB` (one truncated decimal).
pub fn format_bytes(n: u64) -> String {
    let mut unit = 0usize;
    while unit + 1 < UNITS.len() && n >= 1u64 << (10 * (unit + 1)) {
        unit += 1;
    }
    let div = 1u128 << (10 * unit);
    let whole = u128::from(n) / div;
    let tenth = (u128::from(n) % div) * 10 / div;
    if tenth == 0 {
        format!("{whole} {}", UNITS[unit])
    } else {
        format!("{whole}.{tenth} {}", UNITS[unit])
    }
}

/// Formats a duration in milliseconds: `60s`, `1500ms`, `2h`.
pub fn format_duration_ms(ms: u64) -> String {
    if ms.is_multiple_of(3_600_000) {
        format!("{}h", ms / 3_600_000)
    } else if ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}

/// Formats milliseconds since the epoch as `YYYY-MM-DD HH:MM:SS UTC`.
pub fn format_utc_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2);
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_accept_the_plan_examples() {
        assert_eq!(parse_size("64K"), Ok(64 * 1024));
        assert_eq!(parse_size("200G"), Ok(200 << 30));
        assert_eq!(parse_size("1T"), Ok(1 << 40));
        assert_eq!(parse_size("1M"), Ok(1 << 20));
        assert_eq!(parse_size("4096"), Ok(4096));
        assert_eq!(parse_size("2g"), Ok(2 << 30));
    }

    #[test]
    fn sizes_reject_the_plan_counterexamples() {
        for bad in [
            "0", "0G", "1.5G", "-1", "-1G", "+1", "", "K", " 1", "1 ", "1GB", "1_0", "1e3", "0x10",
        ] {
            assert!(parse_size(bad).is_err(), "{bad:?} must be rejected");
        }
        // 2^64 bytes and 2^24 TiB overflow.
        assert!(parse_size("18446744073709551616").is_err());
        assert!(parse_size("16777216T").is_err());
        assert!(parse_size("17179869184G").is_err());
        assert_eq!(parse_size("16777215T"), Ok(16_777_215u64 << 40));
    }

    #[test]
    fn zero_floor_is_allowed_only_where_asked() {
        assert_eq!(parse_floor("0"), Ok(FreeFloor::Bytes(0)));
        assert_eq!(parse_floor("10%"), Ok(FreeFloor::Percent("10%".into())));
        assert_eq!(parse_floor("5G"), Ok(FreeFloor::Bytes(5 << 30)));
        for bad in ["101%", "%", "-1%", "1.5%", "1000%", "x%"] {
            assert!(parse_floor(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn durations_need_a_unit_and_a_value() {
        assert_eq!(parse_duration_ms("60s"), Ok(60_000));
        assert_eq!(parse_duration_ms("250ms"), Ok(250));
        assert_eq!(parse_duration_ms("2m"), Ok(120_000));
        assert_eq!(parse_duration_ms("1h"), Ok(3_600_000));
        for bad in ["60", "0s", "s", "-1s", "1.5s", "", "99999999999999999999h"] {
            assert!(parse_duration_ms(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn compress_policies() {
        assert_eq!(parse_compress("auto"), Ok(Compress::Auto));
        assert_eq!(parse_compress("zstd:-7"), Ok(Compress::Zstd { level: -7 }));
        assert_eq!(parse_compress("zstd:22"), Ok(Compress::Zstd { level: 22 }));
        for bad in ["zstd", "zstd:23", "zstd:-8", "zstd:x", "gzip", ""] {
            assert!(parse_compress(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(200 << 30), "200 GiB");
        assert_eq!(format_bytes(u64::MAX), "15.9 EiB");
    }

    #[test]
    fn utc_formatting() {
        assert_eq!(format_utc_ms(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc_ms(951_782_400_000), "2000-02-29 00:00:00 UTC");
        assert_eq!(format_utc_ms(1_794_652_830_000), "2026-11-14 10:40:30 UTC");
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration_ms(60_000), "1m");
        assert_eq!(format_duration_ms(1_500), "1500ms");
        assert_eq!(format_duration_ms(7_200_000), "2h");
        assert_eq!(format_duration_ms(5_000), "5s");
    }
}
