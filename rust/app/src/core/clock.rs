//! `Date.now()` behind a trait, and the local-time strings main.js prints (fixed pt-BR / ECMA formats measured
//! from Electron 38).
use std::{cell::Cell, time::SystemTime};

use chrono::{
    DateTime, Datelike, FixedOffset, Local, MappedLocalTime, TimeDelta, TimeZone, Timelike,
};

/// The wall clock (`Date.now()`, ms since the epoch).
pub trait Clock {
    fn now(&self) -> f64;
}

/// Production: the system clock, whole milliseconds like `Date.now()`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> f64 {
        match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => d.as_millis() as f64,
            Err(e) => -(e.duration().as_millis() as f64),
        }
    }
}

/// Tests: `t0` + tokio's (paused) elapsed time + an offset. `set_time` moves the wall clock without firing any
/// timer, like node's `mock.timers.setTime`.
pub struct MockClock {
    t0: f64,
    start: tokio::time::Instant,
    offset: Cell<f64>,
}

impl MockClock {
    /// Must be created inside the (paused) runtime.
    pub fn new(t0: f64) -> Self {
        MockClock {
            t0,
            start: tokio::time::Instant::now(),
            offset: Cell::new(0.0),
        }
    }

    pub fn set_time(&self, t: f64) {
        self.offset.set(t - (self.now() - self.offset.get()));
    }
}

impl Clock for MockClock {
    fn now(&self) -> f64 {
        self.t0 + self.start.elapsed().as_nanos() as f64 / 1e6 + self.offset.get()
    }
}

/// `new Date(ms)` in the local zone (TimeClip truncates); None = Invalid Date.
fn local(ms: f64) -> Option<DateTime<Local>> {
    if !ms.is_finite() || ms.abs() > 8.64e15 {
        return None;
    }
    Local.timestamp_millis_opt(ms.trunc() as i64).earliest()
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `new Date(ms).toDateString()`, e.g. `Sun Nov 22 2026` (ECMA-262 DateString).
pub fn to_date_string(ms: f64) -> String {
    let Some(d) = local(ms) else {
        return "Invalid Date".into();
    };
    let y = d.year();
    let year = if y >= 0 {
        format!("{y:04}")
    } else {
        format!("-{:04}", -y)
    };
    format!(
        "{} {} {:02} {year}",
        DAYS[d.weekday().num_days_from_sunday() as usize],
        MONTHS[d.month0() as usize],
        d.day()
    )
}

/// `toLocaleString('pt-BR', {day, month, hour, minute: '2-digit'})`, e.g. `22/11, 17:50` (log lines).
pub fn log_stamp(ms: f64) -> String {
    local(ms).map_or_else(
        || "Invalid Date".into(),
        |d| {
            format!(
                "{:02}/{:02}, {:02}:{:02}",
                d.day(),
                d.month(),
                d.hour(),
                d.minute()
            )
        },
    )
}

/// `toLocaleString('pt-BR')`, e.g. `22/11/2026, 17:50:03` (crash lines).
pub fn crash_stamp(ms: f64) -> String {
    local(ms).map_or_else(
        || "Invalid Date".into(),
        |d| {
            format!(
                "{:02}/{:02}/{}, {:02}:{:02}:{:02}",
                d.day(),
                d.month(),
                d.year(),
                d.hour(),
                d.minute(),
                d.second()
            )
        },
    )
}

/// `toLocaleTimeString('pt-BR', {hour, minute: '2-digit'})`, e.g. `17:50`.
pub fn time_hm(ms: f64) -> String {
    local(ms).map_or_else(
        || "Invalid Date".into(),
        |d| format!("{:02}:{:02}", d.hour(), d.minute()),
    )
}

/// main.js:107 `new Date(y, m, d + 1, 0, 0, 1)` for the local date of `ms`: a skipped local time uses the offset
/// before the transition, a repeated one the earlier instant (ECMA-262 LocalTime -> UTC).
pub fn next_midnight(ms: f64) -> f64 {
    let fallback = ms + 86_400_000.0;
    let Some(now) = local(ms) else {
        return fallback;
    };
    let Some(ndt) = now
        .date_naive()
        .succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 1))
    else {
        return fallback;
    };
    let next = match Local.from_local_datetime(&ndt) {
        MappedLocalTime::Single(t) | MappedLocalTime::Ambiguous(t, _) => t.fixed_offset(),
        MappedLocalTime::None => {
            let Some(before) = Local
                .from_local_datetime(&(ndt - TimeDelta::hours(24)))
                .earliest()
            else {
                return fallback;
            };
            let off: FixedOffset = *before.offset();
            DateTime::<FixedOffset>::from_naive_utc_and_offset(ndt - off, off)
        }
    };
    next.timestamp_millis() as f64
}

/// A local wall time as epoch ms (tests: `new Date('2026-10-19T12:00:00').getTime()`).
pub fn local_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<f64> {
    Local
        .with_ymd_and_hms(y, mo, d, h, mi, s)
        .earliest()
        .map(|t| t.timestamp_millis() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_electron() {
        let t = local_ms(2026, 11, 22, 17, 50, 3).unwrap_or_default();
        assert_eq!(to_date_string(t), "Sun Nov 22 2026");
        assert_eq!(log_stamp(t), "22/11, 17:50");
        assert_eq!(crash_stamp(t), "22/11/2026, 17:50:03");
        assert_eq!(time_hm(t), "17:50");
        let t = local_ms(2026, 11, 2, 7, 5, 0).unwrap_or_default();
        assert_eq!(
            (
                to_date_string(t).as_str(),
                log_stamp(t).as_str(),
                time_hm(t + 0.9).as_str()
            ),
            ("Mon Nov 02 2026", "02/11, 07:05", "07:05")
        );
        let t = local_ms(2026, 11, 2, 0, 5, 0).unwrap_or_default();
        assert_eq!(time_hm(t), "00:05");
        assert_eq!(to_date_string(f64::NAN), "Invalid Date");
    }

    #[test]
    fn midnight_rolls_months_and_years() {
        for ((y, m, d), (ny, nm, nd)) in [
            ((2026, 11, 30), (2026, 12, 1)),
            ((2026, 12, 31), (2027, 1, 1)),
            ((2028, 2, 28), (2028, 2, 29)),
        ] {
            let now = local_ms(y, m, d, 23, 59, 59).unwrap_or_default();
            assert_eq!(
                next_midnight(now),
                local_ms(ny, nm, nd, 0, 0, 1).unwrap_or_default()
            );
            let now = local_ms(y, m, d, 0, 0, 0).unwrap_or_default();
            assert_eq!(
                next_midnight(now),
                local_ms(ny, nm, nd, 0, 0, 1).unwrap_or_default()
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn mock_clock() {
        let c = MockClock::new(1000.0);
        tokio::time::advance(std::time::Duration::from_millis(500)).await;
        assert_eq!(c.now(), 1500.0);
        c.set_time(10_000.0);
        assert_eq!(c.now(), 10_000.0);
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert_eq!(c.now(), 10_001.0);
    }
}
