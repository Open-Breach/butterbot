//! Duration settings: a whole number followed by `s`, `m`, `h` or `d`.

use std::time::Duration;

const UNITS: [(char, u64); 4] = [('d', 86_400), ('h', 3_600), ('m', 60), ('s', 1)];

/// Reads a duration setting such as `"30d"` or `"5m"`.
///
/// On failure the message says what is expected; it does not repeat the key,
/// which the caller knows.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let invalid = || {
        format!(
            "invalid duration {text:?}: expected a whole number followed by s, m, h or d, \
             for example \"30d\""
        )
    };
    let mut characters = text.chars();
    let unit = characters.next_back().ok_or_else(invalid)?;
    let digits = characters.as_str();
    let seconds_per_unit = UNITS
        .iter()
        .find(|(symbol, _)| *symbol == unit)
        .map(|(_, seconds)| *seconds)
        .ok_or_else(invalid)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    digits
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(seconds_per_unit))
        .map(Duration::from_secs)
        .ok_or_else(|| format!("invalid duration {text:?}: the value is too large"))
}

/// Writes a duration the way it is written in configuration, using the
/// largest unit that divides it evenly. Fractions of a second are dropped.
pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    UNITS
        .iter()
        .find(|(_, per_unit)| seconds != 0 && seconds.is_multiple_of(*per_unit))
        .map(|(symbol, per_unit)| format!("{}{symbol}", seconds / per_unit))
        .unwrap_or_else(|| format!("{seconds}s"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // configuration, "Durations", scenario "Valid duration".
    #[test]
    fn reads_each_unit() {
        assert_eq!(parse_duration("30d"), Ok(Duration::from_secs(30 * 86_400)));
        assert_eq!(parse_duration("5m"), Ok(Duration::from_secs(300)));
        assert_eq!(parse_duration("12h"), Ok(Duration::from_secs(43_200)));
        assert_eq!(parse_duration("45s"), Ok(Duration::from_secs(45)));
        assert_eq!(parse_duration("0s"), Ok(Duration::ZERO));
    }

    #[test]
    fn rejects_everything_else() {
        for text in [
            "thirty days",
            "",
            "30",
            "d",
            "30 d",
            " 30d",
            "30d ",
            "1.5h",
            "-5m",
            "+5m",
            "5M",
            "5min",
            "1h30m",
            "٣d",
        ] {
            let error = parse_duration(text).expect_err(text);
            assert!(
                error.contains("expected a whole number"),
                "{text:?}: {error}"
            );
        }
    }

    #[test]
    fn rejects_a_value_that_does_not_fit() {
        let error = parse_duration("99999999999999999999d").expect_err("too large");
        assert!(error.contains("too large"), "{error}");
        let error = parse_duration("18446744073709551615d").expect_err("overflows in seconds");
        assert!(error.contains("too large"), "{error}");
    }

    #[test]
    fn formats_with_the_largest_even_unit() {
        for text in ["30d", "36h", "5m", "90s", "0s", "1d"] {
            let duration = parse_duration(text).expect(text);
            assert_eq!(format_duration(duration), text);
        }
        assert_eq!(format_duration(Duration::from_secs(172_800)), "2d");
    }
}
