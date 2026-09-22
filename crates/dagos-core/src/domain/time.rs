//! Timestamps and the injectable clock.
//!
//! A [`Timestamp`] is a UTC instant with millisecond precision in one canonical text form,
//! `YYYY-MM-DDTHH:MM:SS.mmmZ`. The form is fixed-width, so sorting timestamps as text sorts them
//! chronologically; the store relies on this for stable ordering.

use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MILLIS_PER_DAY: i64 = 86_400_000;
/// 9999-12-31T23:59:59.999Z, the last instant with a four-digit year.
const MAX_MILLIS: i64 = 253_402_300_799_999;

/// A string or instant that is not a canonical DAGOS timestamp.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid timestamp `{0}`: expected UTC `YYYY-MM-DDTHH:MM:SS.mmmZ` between 1970 and 9999")]
pub struct TimestampError(String);

/// A UTC instant with millisecond precision, serialized as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Timestamp(i64);

impl Timestamp {
    /// The instant `millis` milliseconds after the Unix epoch.
    pub fn from_unix_millis(millis: i64) -> Result<Self, TimestampError> {
        if (0..=MAX_MILLIS).contains(&millis) {
            Ok(Self(millis))
        } else {
            Err(TimestampError(millis.to_string()))
        }
    }

    pub fn unix_millis(self) -> i64 {
        self.0
    }

    /// Parses the canonical form. Anything else — other offsets, missing milliseconds,
    /// impossible dates — is rejected.
    pub fn parse(text: &str) -> Result<Self, TimestampError> {
        let invalid = || TimestampError(text.to_owned());
        let bytes = text.as_bytes();
        if bytes.len() != 24 {
            return Err(invalid());
        }
        for (index, expected) in
            [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':'), (19, b'.'), (23, b'Z')]
        {
            if bytes[index] != expected {
                return Err(invalid());
            }
        }
        let number = |range: std::ops::Range<usize>| -> Result<i64, TimestampError> {
            let digits = &text[range];
            if digits.bytes().all(|b| b.is_ascii_digit()) {
                Ok(digits.parse().expect("ascii digits parse"))
            } else {
                Err(invalid())
            }
        };
        let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
        let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
        let millis = number(20..23)?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
            return Err(invalid());
        }
        if second > 59 {
            return Err(invalid());
        }
        let days = days_from_civil(year, month, day);
        let total = days * MILLIS_PER_DAY + ((hour * 60 + minute) * 60 + second) * 1000 + millis;
        let timestamp = Self::from_unix_millis(total).map_err(|_| invalid())?;
        // Day overflow (e.g. February 30th) survives the range checks above but cannot round-trip.
        if timestamp.to_string() == text { Ok(timestamp) } else { Err(invalid()) }
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let days = self.0.div_euclid(MILLIS_PER_DAY);
        let in_day = self.0.rem_euclid(MILLIS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        let (hour, minute) = (in_day / 3_600_000, in_day / 60_000 % 60);
        let (second, millis) = (in_day / 1000 % 60, in_day % 1000);
        write!(f, "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
    }
}

impl TryFrom<String> for Timestamp {
    type Error = TimestampError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Timestamp> for String {
    fn from(timestamp: Timestamp) -> Self {
        timestamp.to_string()
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Source of the current time. Injected so tests and replays are deterministic.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// The system wall clock, the production clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after 1970")
            .as_millis();
        Timestamp::from_unix_millis(i64::try_from(millis).expect("millis fit in i64"))
            .expect("system clock is before year 10000")
    }
}

/// A deterministic clock: returns `start`, then advances by `step_millis` on every call.
#[derive(Debug)]
pub struct SteppingClock {
    next: AtomicI64,
    step_millis: i64,
}

impl SteppingClock {
    pub fn new(start: Timestamp, step_millis: i64) -> Self {
        Self { next: AtomicI64::new(start.unix_millis()), step_millis }
    }
}

impl Clock for SteppingClock {
    fn now(&self) -> Timestamp {
        let millis = self.next.fetch_add(self.step_millis, Ordering::SeqCst);
        Timestamp::from_unix_millis(millis).expect("stepping clock stays within range")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    #[test]
    fn formats_known_instants() {
        assert_eq!(Timestamp::from_unix_millis(0).unwrap().to_string(), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            Timestamp::from_unix_millis(1_790_104_774_123).unwrap().to_string(),
            "2026-09-22T19:19:34.123Z"
        );
        assert_eq!(
            Timestamp::from_unix_millis(MAX_MILLIS).unwrap().to_string(),
            "9999-12-31T23:59:59.999Z"
        );
    }

    #[test]
    fn parse_is_the_inverse_of_display() {
        for millis in [
            0,
            1,
            951_782_400_000,   // 2000-02-29 (leap day)
            4_107_542_399_999, // 2100-02-28T23:59:59.999 (2100 is not a leap year)
            1_790_104_774_123,
            MAX_MILLIS,
        ] {
            let timestamp = Timestamp::from_unix_millis(millis).unwrap();
            assert_eq!(ts(&timestamp.to_string()), timestamp);
        }
    }

    #[test]
    fn rejects_non_canonical_or_impossible_text() {
        for text in [
            "2026-09-22T19:19:34Z",
            "2026-09-22T19:19:34.1234Z",
            "2026-09-22T19:19:34.123+00:00",
            "2026-09-22 19:19:34.123Z",
            "2026-02-30T00:00:00.000Z",
            "2100-02-29T00:00:00.000Z",
            "2026-13-01T00:00:00.000Z",
            "2026-09-22T24:00:00.000Z",
            "2026-09-22T23:60:00.000Z",
            "2026-09-22T23:59:60.000Z",
            "1969-12-31T23:59:59.999Z",
            "+026-09-22T19:19:34.123Z",
            "",
        ] {
            assert!(Timestamp::parse(text).is_err(), "accepted {text}");
        }
        assert!(Timestamp::parse("2000-02-29T00:00:00.000Z").is_ok());
        assert!(Timestamp::from_unix_millis(-1).is_err());
        assert!(Timestamp::from_unix_millis(MAX_MILLIS + 1).is_err());
    }

    #[test]
    fn text_order_matches_chronological_order() {
        let earlier = ts("2026-09-22T19:19:34.999Z");
        let later = ts("2026-09-22T19:19:35.000Z");
        assert!(earlier < later);
        assert!(earlier.to_string() < later.to_string());
    }

    #[test]
    fn serde_uses_the_canonical_form() {
        let timestamp = ts("2026-09-22T19:19:34.123Z");
        let json = serde_json::to_string(&timestamp).unwrap();
        assert_eq!(json, "\"2026-09-22T19:19:34.123Z\"");
        assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), timestamp);
        assert!(serde_json::from_str::<Timestamp>("\"2026-09-22\"").is_err());
    }

    #[test]
    fn stepping_clock_is_deterministic() {
        let clock = SteppingClock::new(ts("2026-01-01T00:00:00.000Z"), 1000);
        assert_eq!(clock.now().to_string(), "2026-01-01T00:00:00.000Z");
        assert_eq!(clock.now().to_string(), "2026-01-01T00:00:01.000Z");
    }
}
