//! Extended ISO instant/duration range and canonical formatting.
//! No local time zones, system clocks, or relative-date inference are involved.
use crate::{ModelError, TimeParts};
use regex::Regex;
use std::{fmt, str::FromStr, sync::LazyLock};

const MIN_INSTANT: i64 = -31_557_014_167_219_200;
const MAX_INSTANT: i64 = 31_556_889_864_403_199;
const BILLION: i128 = 1_000_000_000;
static INSTANT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
    r"(?i)\A(?P<year>[+-]?[0-9]{4,10})-(?P<month>[0-9]{2})-(?P<day>[0-9]{2})T",
    r"(?P<hour>[0-9]{2}):(?P<minute>[0-9]{2}):(?P<second>[0-9]{2})(?:\.(?P<fraction>[0-9]{0,9}))?",
    r"(?P<offset>Z|[+-](?:[0-9]{2}|[0-9]{4}|[0-9]{6}|[0-9]{2}:[0-9]{2}(?::[0-9]{2})?))\z"
)).expect("ISO instant grammar")
});
static DURATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\A(?P<sign>[+-]?)P(?:(?P<days>[+-]?[0-9]+)D)?",
        r"(?P<time>T(?:(?P<hours>[+-]?[0-9]+)H)?(?:(?P<minutes>[+-]?[0-9]+)M)?",
        r"(?:(?P<seconds>[+-]?[0-9]+)(?:[.,](?P<fraction>[0-9]{0,9}))?S)?)?\z"
    ))
    .expect("ISO duration grammar")
});

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(TimeParts);
impl Timestamp {
    pub fn new(seconds: i64, nanos: u32) -> Result<Self, ModelError> {
        if !(MIN_INSTANT..=MAX_INSTANT).contains(&seconds) {
            return Err(ModelError::InvalidTime(
                "instant outside supported range".into(),
            ));
        }
        Ok(Self(TimeParts::new(seconds, nanos)?))
    }
    pub fn parts(self) -> TimeParts {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DurationValue(TimeParts);
impl DurationValue {
    pub fn new(seconds: i64, nanos: u32) -> Result<Self, ModelError> {
        Ok(Self(TimeParts::new(seconds, nanos)?))
    }
    pub fn parts(self) -> TimeParts {
        self.0
    }
}

fn fraction(text: &str) -> u32 {
    if text.is_empty() {
        0
    } else {
        text.parse::<u32>().expect("matched nanosecond digits") * 10u32.pow(9 - text.len() as u32)
    }
}
fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
fn months(year: i64) -> [i64; 12] {
    [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ]
}
fn year_start(year: i64) -> i64 {
    365 * year + (year + 3).div_euclid(4) - (year + 99).div_euclid(100)
        + (year + 399).div_euclid(400)
        - 719_528
}

impl FromStr for Timestamp {
    type Err = ModelError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        fn read(text: &str) -> Option<Timestamp> {
            let parts = INSTANT.captures(text)?;
            let raw = &parts["year"];
            let year = raw.parse::<i64>().ok()?;
            if (raw.starts_with('+') && raw.len() < 6)
                || (raw.starts_with('-') && year == 0)
                || (!raw.starts_with(['+', '-']) && raw.len() != 4)
            {
                return None;
            }
            let number = |key| parts.name(key)?.as_str().parse::<i64>().ok();
            let month = number("month")?;
            let day = number("day")?;
            let hour = number("hour")?;
            let minute = number("minute")?;
            let mut second = number("second")?;
            let nanos = fraction(parts.name("fraction").map_or("", |m| m.as_str()));
            if !(1..=12).contains(&month)
                || !(1..=months(year)[month as usize - 1]).contains(&day)
                || minute > 59
                || hour > 24
            {
                return None;
            }
            if hour == 24 && (minute != 0 || second != 0 || nanos != 0) {
                return None;
            }
            if second == 60 && hour == 23 && minute == 59 {
                second = 59;
            }
            if second > 59 {
                return None;
            }
            let offset = &parts["offset"];
            let shift = if offset.eq_ignore_ascii_case("z") {
                0
            } else {
                let digits = offset[1..].replace(':', "");
                let hours = digits[..2].parse::<i64>().ok()?;
                let minutes = if digits.len() >= 4 {
                    digits[2..4].parse::<i64>().ok()?
                } else {
                    0
                };
                let seconds = if digits.len() == 6 {
                    digits[4..].parse::<i64>().ok()?
                } else {
                    0
                };
                if hours > 18
                    || minutes > 59
                    || seconds > 59
                    || (hours == 18 && (minutes != 0 || seconds != 0))
                {
                    return None;
                }
                (hours * 3600 + minutes * 60 + seconds)
                    * if offset.starts_with('-') { -1 } else { 1 }
            };
            let days =
                year_start(year) + months(year).iter().take(month as usize - 1).sum::<i64>() + day
                    - 1;
            Timestamp::new(
                days * 86_400 + hour * 3600 + minute * 60 + second - shift,
                nanos,
            )
            .ok()
        }
        read(text).ok_or_else(|| ModelError::InvalidTime(if INSTANT.is_match(text) {
            "invalid calendar date, time, UTC offset or supported year range; check month/day (including leap years), time and offset".into()
        } else {
            "expected ISO date and time with T and an explicit Z or UTC offset, for example 2026-01-02T03:04:05Z; at most 9 fractional digits".into()
        }))
    }
}

impl FromStr for DurationValue {
    type Err = ModelError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        fn read(text: &str) -> Option<DurationValue> {
            let parts = DURATION.captures(text)?;
            if parts.name("days").is_none() && parts.name("time").is_none() {
                return None;
            }
            if parts
                .name("time")
                .is_some_and(|time| time.as_str().len() == 1)
            {
                return None;
            }
            let component = |key, multiplier| -> Option<i128> {
                parts.name(key).map_or(Some(0), |m| {
                    m.as_str().parse::<i128>().ok()?.checked_mul(multiplier)
                })
            };
            let seconds = component("days", 86_400)?
                .checked_add(component("hours", 3600)?)?
                .checked_add(component("minutes", 60)?)?
                .checked_add(component("seconds", 1)?)?;
            let nanos = i128::from(fraction(parts.name("fraction").map_or("", |m| m.as_str())))
                * if parts
                    .name("seconds")
                    .is_some_and(|s| s.as_str().starts_with('-'))
                {
                    -1
                } else {
                    1
                };
            let total = seconds.checked_mul(BILLION)?.checked_add(nanos)?;
            let total = if &parts["sign"] == "-" {
                total.checked_neg()?
            } else {
                total
            };
            DurationValue::new(
                i64::try_from(total.div_euclid(BILLION)).ok()?,
                total.rem_euclid(BILLION) as u32,
            )
            .ok()
        }
        read(text).ok_or_else(|| ModelError::InvalidTime(if DURATION.is_match(text) {
            "duration requires a day or nonempty time component within the supported range".into()
        } else {
            "expected ISO duration such as PT5M or -PT1.5S (days/hours/minutes/seconds, at most 9 fractional digits); calendar months, years and shorthand such as 5m are unsupported".into()
        }))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [year, month, day, hour, minute, second, nanos, _weekday] = self.utc_parts();
        if year < 0 {
            write!(f, "-{:04}", -year)?;
        } else if year > 9999 {
            write!(f, "+{year}")?;
        } else {
            write!(f, "{year:04}")?;
        }
        write!(
            f,
            "-{month:02}-{:02}T{:02}:{:02}:{:02}",
            day, hour, minute, second
        )?;
        if nanos != 0 {
            if nanos % 1_000_000 == 0 {
                write!(f, ".{:03}", nanos / 1_000_000)?;
            } else if nanos % 1000 == 0 {
                write!(f, ".{:06}", nanos / 1000)?;
            } else {
                write!(f, ".{nanos:09}")?;
            }
        }
        f.write_str("Z")
    }
}
impl fmt::Display for DurationValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total = self.total_nanos();
        if total < 0 {
            f.write_str("-")?;
        }
        let magnitude = total.abs();
        let seconds = magnitude / BILLION;
        let nanos = magnitude % BILLION;
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        let seconds = seconds % 60;
        f.write_str("PT")?;
        if hours != 0 {
            write!(f, "{hours}H")?;
        }
        if minutes != 0 {
            write!(f, "{minutes}M")?;
        }
        if seconds == 0 && nanos == 0 && (hours != 0 || minutes != 0) {
            return Ok(());
        }
        write!(f, "{seconds}")?;
        if nanos > 0 {
            let digits = format!("{nanos:09}");
            write!(f, ".{}", digits.trim_end_matches('0'))?;
        }
        f.write_str("S")
    }
}

/// A finite half-open interval. Equal endpoints form an empty interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval {
    start: Timestamp,
    end: Timestamp,
}
impl Interval {
    pub fn new(start: Timestamp, end: Timestamp) -> Result<Self, ModelError> {
        if start > end {
            return Err(ModelError::InvalidTime(
                "interval start must not exceed end".into(),
            ));
        }
        Ok(Self { start, end })
    }
    pub fn start(self) -> Timestamp {
        self.start
    }
    pub fn end(self) -> Timestamp {
        self.end
    }
    pub fn contains(self, instant: Timestamp) -> bool {
        self.start <= instant && instant < self.end
    }
    pub fn duration(self) -> Result<DurationValue, ModelError> {
        self.end.since(self.start)
    }
    pub fn around(center: Timestamp, radius: DurationValue) -> Result<Self, ModelError> {
        if radius.total_nanos() <= 0 {
            return Err(ModelError::InvalidTime(
                "interval radius must be positive".into(),
            ));
        }
        Self::new(center.subtract(radius)?, center.add(radius)?)
    }
}
impl FromStr for Interval {
    type Err = ModelError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (start, end) = text
            .split_once('/')
            .ok_or_else(|| ModelError::InvalidTime("interval requires ISO start/end".into()))?;
        Self::new(start.parse()?, end.parse()?)
    }
}
impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.start, self.end)
    }
}
fn split_nanos(total: i128) -> Result<(i64, u32), ModelError> {
    Ok((
        i64::try_from(total.div_euclid(BILLION))
            .map_err(|_| ModelError::InvalidTime("temporal arithmetic overflow".into()))?,
        total.rem_euclid(BILLION) as u32,
    ))
}
impl Timestamp {
    /// UTC year, month, day, hour, minute, second, nanosecond and ISO weekday (Monday=1).
    pub fn utc_parts(self) -> [i64; 8] {
        let days = self.0.seconds().div_euclid(86_400);
        let time = self.0.seconds().rem_euclid(86_400);
        let (mut low, mut high) = (-1_000_000_000i64, 1_000_000_001i64);
        while low + 1 < high {
            let middle = (low + high).div_euclid(2);
            if year_start(middle) <= days {
                low = middle;
            } else {
                high = middle;
            }
        }
        let year = low;
        let mut day = days - year_start(year);
        let mut month = 1;
        for length in months(year) {
            if day < length {
                break;
            }
            day -= length;
            month += 1;
        }
        [
            year,
            month,
            day + 1,
            time / 3600,
            (time % 3600) / 60,
            time % 60,
            i64::from(self.0.nanos()),
            (days + 3).rem_euclid(7) + 1,
        ]
    }
    pub fn total_nanos(self) -> i128 {
        i128::from(self.0.seconds()) * BILLION + i128::from(self.0.nanos())
    }
    pub fn from_nanos(total: i128) -> Result<Self, ModelError> {
        let (s, n) = split_nanos(total)?;
        Self::new(s, n)
    }
    pub fn add(self, duration: DurationValue) -> Result<Self, ModelError> {
        Self::from_nanos(self.total_nanos() + duration.total_nanos())
    }
    pub fn subtract(self, duration: DurationValue) -> Result<Self, ModelError> {
        Self::from_nanos(self.total_nanos() - duration.total_nanos())
    }
    pub fn since(self, other: Self) -> Result<DurationValue, ModelError> {
        DurationValue::from_nanos(self.total_nanos() - other.total_nanos())
    }
}
impl DurationValue {
    pub fn total_nanos(self) -> i128 {
        i128::from(self.0.seconds()) * BILLION + i128::from(self.0.nanos())
    }
    pub fn from_nanos(total: i128) -> Result<Self, ModelError> {
        let (s, n) = split_nanos(total)?;
        Self::new(s, n)
    }
    pub fn add(self, other: Self) -> Result<Self, ModelError> {
        Self::from_nanos(self.total_nanos() + other.total_nanos())
    }
    pub fn subtract(self, other: Self) -> Result<Self, ModelError> {
        Self::from_nanos(self.total_nanos() - other.total_nanos())
    }
}
