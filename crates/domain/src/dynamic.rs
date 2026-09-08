//! Dynamic variables: the `{{$timestamp}}` family, worth something different on
//! every run.
//!
//! This lives in the domain because **what** `{{$randomInt 1 100}}` means is a
//! rule of the format, not a platform detail. What does not live here is where
//! the clock and the randomness come from: those arrive as a [`DynamicSeed`],
//! so the domain still never touches the world and a test can pin both values
//! and check the exact output.
//!
//! # One value per execution, not per occurrence
//!
//! The value is derived from the seed and from the placeholder's own text, so
//! two `{{$uuid}}` in the same request give the same identifier while two
//! executions give different ones. That is deliberate: the case that actually
//! comes up is a correlation id repeated in a header and in the body, and
//! wanting two different values there would be a bug.

use crate::error::DomainError;

/// Where the values that change on every run come from.
///
/// It is handed to the variable context already computed rather than read
/// here: the domain has no clock and no random generator, and that is exactly
/// the property that lets these functions be checked with `assert_eq!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DynamicSeed {
    /// Seconds elapsed since the Unix epoch.
    pub unix_seconds: i64,
    /// Seed for the pseudo-random generator.
    pub random: u64,
}

impl DynamicSeed {
    /// A fixed seed, for tests and for any reproducible use.
    #[must_use]
    pub const fn fixed(unix_seconds: i64, random: u64) -> Self {
        Self {
            unix_seconds,
            random,
        }
    }
}

/// Resolves a dynamic placeholder (`$timestamp`, `$randomInt 1 100`…).
///
/// `spec` arrives exactly as it was written inside the braces, `$` included.
///
/// # Errors
///
/// - [`DomainError::UnknownDynamicVariable`] when the name is not one we know
///   how to generate.
/// - [`DomainError::InvalidDynamicVariable`] when the arguments do not fit.
pub fn resolve_dynamic(spec: &str, seed: DynamicSeed) -> Result<String, DomainError> {
    let mut tokens = spec.split_whitespace();
    let name = tokens.next().unwrap_or_default();
    let args: Vec<&str> = tokens.collect();

    let invalid = |reason: &str| DomainError::InvalidDynamicVariable {
        spec: spec.to_owned(),
        reason: reason.to_owned(),
    };

    match name {
        "$timestamp" => Ok(seed.unix_seconds.to_string()),

        // JetBrains generates 0-1000 with no arguments; VS Code requires a
        // range. Both forms are accepted so a file from either one works.
        "$randomInt" => match args.as_slice() {
            [] => Ok(bounded(random_for(spec, seed), 0, 1000).to_string()),
            [min, max] => {
                let min: i64 = min
                    .parse()
                    .map_err(|_| invalid("the minimum is not an integer"))?;
                let max: i64 = max
                    .parse()
                    .map_err(|_| invalid("the maximum is not an integer"))?;
                if min > max {
                    return Err(invalid("the minimum is greater than the maximum"));
                }
                Ok(bounded(random_for(spec, seed), min, max).to_string())
            }
            _ => Err(invalid("expected `$randomInt` or `$randomInt min max`")),
        },

        // `$uuid` is JetBrains and `$guid` is VS Code; httpyac takes both.
        "$uuid" | "$guid" => Ok(uuid_v4(random_for(spec, seed), random_for("uuid-hi", seed))),

        "$isoTimestamp" => Ok(format_iso8601(seed.unix_seconds)),

        "$datetime" => match args.as_slice() {
            ["iso8601"] => Ok(format_iso8601(seed.unix_seconds)),
            ["rfc1123"] => Ok(format_rfc1123(seed.unix_seconds)),
            // Rejected rather than approximated: a format of our own invention
            // would send a date the file did not ask for.
            _ => Err(invalid("only `iso8601` and `rfc1123` are supported")),
        },

        _ => Err(DomainError::UnknownDynamicVariable {
            name: name.to_owned(),
        }),
    }
}

/// Mixes the seed with the placeholder's text.
///
/// Depending on the text is what keeps `{{$randomInt 1 10}}` and
/// `{{$randomInt 1 100}}` from coming out identical within the same request,
/// without keeping mutable state alive through the interpolation.
fn random_for(spec: &str, seed: DynamicSeed) -> u64 {
    // FNV-1a over the text, then splitmix64 to spread the bits. It is not
    // cryptographic and does not pretend to be: this generates test data, not
    // secrets.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in spec.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }

    splitmix64(seed.random ^ hash)
}

/// One `splitmix64` step, which spreads the bits of any seed well.
fn splitmix64(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Maps a random value into `[min, max]`, both ends included.
fn bounded(value: u64, min: i64, max: i64) -> i64 {
    // `max - min` can overflow an `i64` at the extremes of the type, so the
    // span is computed in `u64`, where it always fits.
    let span = max.wrapping_sub(min).cast_unsigned();
    if span == u64::MAX {
        return value.cast_signed();
    }

    min.wrapping_add((value % (span + 1)).cast_signed())
}

/// Formats 128 bits as a version 4 UUID.
fn uuid_v4(low: u64, high: u64) -> String {
    // The version (4) and variant (RFC 4122) bits are set as the format
    // requires, so whoever receives the value accepts it as a UUID and not as
    // 32 hex digits.
    let high = (high & 0xffff_ffff_ffff_0fff) | 0x0000_0000_0000_4000;
    let low = (low & 0x3fff_ffff_ffff_ffff) | 0x8000_0000_0000_0000;

    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        high >> 32,
        (high >> 16) & 0xffff,
        high & 0xffff,
        (low >> 48) & 0xffff,
        low & 0xffff_ffff_ffff
    )
}

/// A UTC date and time, already broken apart.
///
/// The fields are `i64` rather than narrower integers so no conversion is
/// needed at each step of the calculation: they are only used for formatting.
struct Civil {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
}

/// Breaks a Unix instant into a civil date and a UTC time.
///
/// This is Howard Hinnant's algorithm, which avoids month tables and works for
/// any year with no special cases beyond the era shift. It is implemented here
/// rather than pulling in a date dependency: it is thirty pure, tested lines,
/// and the domain has no dependencies today.
fn civil_from_unix(seconds: i64) -> Civil {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };

    Civil {
        year: if month <= 2 { y + 1 } else { y },
        month,
        day,
        hour: secs_of_day / 3_600,
        minute: (secs_of_day % 3_600) / 60,
        second: secs_of_day % 60,
    }
}

/// `2026-09-05T11:15:12Z`.
fn format_iso8601(seconds: i64) -> String {
    let Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
    } = civil_from_unix(seconds);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// `Sat, 05 Sep 2026 11:15:12 GMT`, the format HTTP headers use.
fn format_rfc1123(seconds: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    let Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
    } = civil_from_unix(seconds);

    // The Unix epoch fell on a Thursday, hence the order of `DAYS`. The indices
    // are correct by construction: `rem_euclid` never returns a negative, and
    // the month comes from a calculation bounded to 1..=12.
    let weekday = usize::try_from(seconds.div_euclid(86_400).rem_euclid(7)).unwrap_or_default();
    let month_index = usize::try_from(month - 1).unwrap_or_default();

    format!(
        "{}, {day:02} {} {year:04} {hour:02}:{minute:02}:{second:02} GMT",
        DAYS[weekday % DAYS.len()],
        MONTHS[month_index % MONTHS.len()]
    )
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap`/`expect` document the expectation and their panic IS the failure.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// 2026-09-05T11:15:12Z, a Saturday.
    const SEED: DynamicSeed = DynamicSeed::fixed(1_788_606_912, 0x1234_5678_9abc_def0);

    fn value(spec: &str) -> String {
        resolve_dynamic(spec, SEED).unwrap()
    }

    #[test]
    fn the_timestamp_is_the_seeds_seconds() {
        assert_eq!(value("$timestamp"), "1788606912");
    }

    #[test]
    fn random_int_without_arguments_runs_from_0_to_1000() {
        let n: i64 = value("$randomInt").parse().unwrap();
        assert!((0..=1000).contains(&n), "out of range: {n}");
    }

    #[test]
    fn random_int_honours_the_requested_range() {
        for _ in 0..1 {
            let n: i64 = value("$randomInt 5 7").parse().unwrap();
            assert!((5..=7).contains(&n), "out of range: {n}");
        }
    }

    #[test]
    fn random_int_accepts_a_single_value_range() {
        assert_eq!(value("$randomInt 42 42"), "42");
    }

    #[test]
    fn random_int_accepts_negative_bounds() {
        let n: i64 = value("$randomInt -10 -5").parse().unwrap();
        assert!((-10..=-5).contains(&n), "out of range: {n}");
    }

    #[test]
    fn a_reversed_range_is_an_error() {
        assert!(matches!(
            resolve_dynamic("$randomInt 10 1", SEED),
            Err(DomainError::InvalidDynamicVariable { .. })
        ));
    }

    #[test]
    fn a_range_that_is_not_numeric_is_an_error() {
        assert!(resolve_dynamic("$randomInt a b", SEED).is_err());
    }

    #[test]
    fn the_same_placeholder_gives_the_same_value_within_one_execution() {
        // This is what lets a correlation id be repeated in a header and in
        // the body without becoming two different values.
        assert_eq!(value("$uuid"), value("$uuid"));
    }

    #[test]
    fn different_placeholders_give_different_values() {
        assert_ne!(value("$randomInt 1 1000000"), value("$randomInt 0 999999"));
    }

    #[test]
    fn another_seed_gives_another_uuid() {
        let another = DynamicSeed::fixed(SEED.unix_seconds, 99);
        assert_ne!(value("$uuid"), resolve_dynamic("$uuid", another).unwrap());
    }

    #[test]
    fn the_uuid_has_the_shape_of_a_v4_uuid() {
        let uuid = value("$uuid");
        let parts: Vec<&str> = uuid.split('-').collect();

        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(
            uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "{uuid}"
        );
        // Version 4 and the RFC 4122 variant.
        assert!(parts[2].starts_with('4'), "version: {uuid}");
        assert!(
            ['8', '9', 'a', 'b'].contains(&parts[3].chars().next().unwrap()),
            "variant: {uuid}"
        );
    }

    #[test]
    fn guid_is_vs_codes_name_for_the_same_thing() {
        assert!(resolve_dynamic("$guid", SEED).is_ok());
    }

    #[test]
    fn formats_iso8601() {
        assert_eq!(value("$isoTimestamp"), "2026-09-05T11:15:12Z");
        assert_eq!(value("$datetime iso8601"), "2026-09-05T11:15:12Z");
    }

    #[test]
    fn formats_rfc1123() {
        assert_eq!(value("$datetime rfc1123"), "Sat, 05 Sep 2026 11:15:12 GMT");
    }

    #[test]
    fn the_epoch_is_a_thursday() {
        // Anchors the weekday calculation, which is the one most likely to
        // drift by one without any other test noticing.
        assert_eq!(
            resolve_dynamic("$datetime rfc1123", DynamicSeed::fixed(0, 0)).unwrap(),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
    }

    #[test]
    fn handles_dates_before_the_epoch() {
        assert_eq!(
            resolve_dynamic("$isoTimestamp", DynamicSeed::fixed(-1, 0)).unwrap(),
            "1969-12-31T23:59:59Z"
        );
    }

    #[test]
    fn handles_a_leap_year() {
        // 2024-02-29T12:00:00Z.
        assert_eq!(
            resolve_dynamic("$isoTimestamp", DynamicSeed::fixed(1_709_208_000, 0)).unwrap(),
            "2024-02-29T12:00:00Z"
        );
    }

    #[test]
    fn a_format_we_do_not_know_is_an_error() {
        assert!(matches!(
            resolve_dynamic("$datetime YYYY-MM-DD", SEED),
            Err(DomainError::InvalidDynamicVariable { .. })
        ));
    }

    #[test]
    fn an_unknown_name_says_so() {
        assert_eq!(
            resolve_dynamic("$aadToken", SEED),
            Err(DomainError::UnknownDynamicVariable {
                name: "$aadToken".to_owned()
            })
        );
    }
}
