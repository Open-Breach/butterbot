//! Time: where the current time comes from, and how a time is written.
//!
//! The clock is injected wherever the current time matters, so that tests can
//! fix it.

use std::sync::Arc;

use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

/// A source of the current time.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

/// The clock of the machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// A clock that always shows the same time.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

/// A clock that can be shared between tasks.
pub type SharedClock = Arc<dyn Clock>;

const SECONDS: &[BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");

const MILLISECONDS: &[BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

/// Writes a time in ISO 8601 format, in UTC, to the second:
/// `2026-11-23T02:00:00Z`.
pub fn format_utc(time: OffsetDateTime) -> String {
    time.to_offset(time::UtcOffset::UTC)
        .format(SECONDS)
        .unwrap_or_else(|_| String::from("0000-00-00T00:00:00Z"))
}

/// Writes a time like [`format_utc`], with milliseconds:
/// `2026-11-23T02:00:00.250Z`.
pub fn format_utc_millis(time: OffsetDateTime) -> String {
    time.to_offset(time::UtcOffset::UTC)
        .format(MILLISECONDS)
        .unwrap_or_else(|_| String::from("0000-00-00T00:00:00.000Z"))
}

/// Serialises a time with [`format_utc`]. For use with `#[serde(serialize_with)]`.
pub fn serialize_utc<S: serde::Serializer>(
    time: &OffsetDateTime,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&format_utc(*time))
}

/// Serialises an optional time with [`format_utc`], or as `null`.
pub fn serialize_optional_utc<S: serde::Serializer>(
    time: &Option<OffsetDateTime>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match time {
        Some(time) => serializer.serialize_some(&format_utc(*time)),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn time_is_written_in_utc_to_the_second() {
        assert_eq!(
            format_utc(datetime!(2026-11-23 02:00:00.75 UTC)),
            "2026-11-23T02:00:00Z"
        );
        assert_eq!(
            format_utc(datetime!(2027-02-03 04:05:06 UTC)),
            "2027-02-03T04:05:06Z"
        );
    }

    #[test]
    fn time_in_another_zone_is_converted_to_utc() {
        assert_eq!(
            format_utc(datetime!(2026-11-23 03:30:00 +01:30)),
            "2026-11-23T02:00:00Z"
        );
    }

    #[test]
    fn milliseconds_are_always_three_digits() {
        assert_eq!(
            format_utc_millis(datetime!(2026-11-23 02:00:00.25 UTC)),
            "2026-11-23T02:00:00.250Z"
        );
        assert_eq!(
            format_utc_millis(datetime!(2026-11-23 02:00:00 UTC)),
            "2026-11-23T02:00:00.000Z"
        );
    }

    #[test]
    fn fixed_clock_does_not_move() {
        let clock = FixedClock(datetime!(2026-11-23 02:00:00 UTC));
        assert_eq!(clock.now(), clock.now());
    }
}
