use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::fmt::Write as _;
use std::hash::{BuildHasherDefault, Hasher};
use std::os::raw::{c_char, c_int, c_long, c_ulong};
use std::time::{SystemTime, UNIX_EPOCH};

/// Broken-down calendar date/time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: i64,
    pub minute: i64,
}

pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn utc_datetime(seconds: i64) -> DateTime {
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);

    DateTime {
        year,
        month,
        day,
        hour: second_of_day / 3600,
        minute: (second_of_day % 3600) / 60,
    }
}

pub fn local_datetime(seconds: i64) -> Option<DateTime> {
    local_fields(seconds).map(|(datetime, _, _)| datetime)
}

/// `localtime_r` for `seconds`: the local date/time, the second within the
/// minute, and the UTC offset in seconds.
fn local_fields(seconds: i64) -> Option<(DateTime, i64, i64)> {
    let time: TimeT = seconds as TimeT;
    let mut tm = std::mem::MaybeUninit::<Tm>::uninit();
    let result = unsafe { localtime_r(&time, tm.as_mut_ptr()) };

    if result.is_null() {
        return None;
    }

    let tm = unsafe { tm.assume_init() };

    let datetime = DateTime {
        year: tm.tm_year as i64 + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as i64,
        minute: tm.tm_min as i64,
    };
    // `c_long` is 32 bits on 32-bit targets, so the conversion is not
    // always a no-op.
    #[allow(clippy::useless_conversion)]
    let offset = i64::from(tm.tm_gmtoff);
    Some((datetime, tm.tm_sec as i64, offset))
}

const DAY: i64 = 86_400;

/// Convert a file time to local time, calling `localtime_r` at most about
/// three times per distinct UTC day instead of once per file.
///
/// `localtime_r` takes a process-wide lock in glibc and dominated long
/// listings, worst of all when several threads render at once. Within one
/// UTC day, local time is UTC plus a fixed offset unless the zone changes
/// its offset during that day. So the second time a day is seen, it is
/// probed at its first and last second: if UTC plus the offset reproduces
/// `localtime_r`'s fields exactly at both ends, every later time in the day
/// uses the offset. Otherwise (a transition day, or a zone with leap seconds
/// such as `right/UTC`, whose fields include a correction) times in that day
/// keep going through `localtime_r`. A day seen only once costs one call,
/// as before.
///
/// This relies on no zone changing its offset and back within one day. No
/// zone in the tz database has done so since 1970 (checked over all 1,795
/// zone files), and earlier times always use `localtime_r`. Only a
/// hand-written `TZ` rule with a daylight-saving period under a day could
/// defeat it.
///
/// The cache is per thread, so directory workers never contend on it, and it
/// persists across directories.
pub fn local_time(seconds: i64) -> DateTime {
    let day = seconds.div_euclid(DAY);
    let offset = DAYS.with_borrow_mut(|days| match days.get(&day) {
        Some(&DayOffset::Known(offset)) => offset,
        Some(DayOffset::SeenOnce) => {
            let offset = day_offset(day);
            days.insert(day, DayOffset::Known(offset));
            offset
        }
        None => {
            days.insert(day, DayOffset::SeenOnce);
            None
        }
    });
    match offset {
        Some(offset) => utc_datetime(seconds + offset),
        None => local_datetime(seconds).unwrap_or_else(|| utc_datetime(seconds)),
    }
}

/// Fixed offsets for the days of a known set of times, read-only so that
/// many threads can convert times without touching `localtime_r`.
///
/// Built on one thread from that thread's [`local_time`] cache; days probed
/// for earlier listings are reused.
pub struct DayOffsets {
    offsets: DayMap<Option<i64>>,
}

impl DayOffsets {
    pub fn for_times(times: impl IntoIterator<Item = i64>) -> Self {
        let mut offsets = DayMap::default();
        DAYS.with_borrow_mut(|days| {
            for time in times {
                offsets
                    .entry(time.div_euclid(DAY))
                    .or_insert_with_key(|&day| match days.get(&day) {
                        Some(&DayOffset::Known(offset)) => offset,
                        _ => {
                            let offset = day_offset(day);
                            days.insert(day, DayOffset::Known(offset));
                            offset
                        }
                    });
            }
        });
        Self { offsets }
    }

    /// Local time for `seconds`; times outside the prepared days, or on days
    /// without a fixed offset, go through `localtime_r`.
    pub fn local_time(&self, seconds: i64) -> DateTime {
        match self.offsets.get(&seconds.div_euclid(DAY)) {
            Some(&Some(offset)) => utc_datetime(seconds + offset),
            _ => local_datetime(seconds).unwrap_or_else(|| utc_datetime(seconds)),
        }
    }
}

/// Maps keyed by day number. Days are small integers derived from file
/// times, so a fixed multiplicative hash spreads them well and avoids
/// SipHash and its per-thread random seeding, which cost more than the
/// lookups themselves for small listings.
type DayMap<V> = HashMap<i64, V, BuildHasherDefault<DayHasher>>;

#[derive(Default)]
struct DayHasher(u64);

impl Hasher for DayHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
    }

    fn write_i64(&mut self, value: i64) {
        // Fibonacci hashing: the high bits, which hashbrown uses, depend on
        // every bit of the day number.
        self.0 = (value as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

#[derive(Clone, Copy)]
enum DayOffset {
    /// One time in this day was converted with `localtime_r`.
    SeenOnce,
    /// The day was probed: its fixed offset, or `None` to keep using
    /// `localtime_r`.
    Known(Option<i64>),
}

thread_local! {
    static DAYS: RefCell<DayMap<DayOffset>> = RefCell::new(DayMap::default());
}

/// The UTC offset in effect for the whole UTC day `day`, if it can be
/// established safely (see [`LocalTime`]).
fn day_offset(day: i64) -> Option<i64> {
    let first = day.checked_mul(DAY)?;
    if first < 0 {
        return None;
    }
    let last = first + DAY - 1;
    let (first_local, first_second, offset) = local_fields(first)?;
    let (last_local, last_second, last_offset) = local_fields(last)?;
    let reproduces = |time: i64, local: DateTime, second: i64| {
        utc_datetime(time + offset) == local && (time + offset).rem_euclid(60) == second
    };
    (offset == last_offset
        && reproduces(first, first_local, first_second)
        && reproduces(last, last_local, last_second))
    .then_some(offset)
}

/// Format the timestamp for `seconds` relative to `now`.
#[allow(dead_code)] // used by the unit tests in src/main.rs
pub fn format_time_at(seconds: i64, now: i64, datetime: DateTime) -> String {
    let mut out = String::new();
    push_format_time_at(seconds, now, datetime, &mut out);
    out
}

/// Write the formatted timestamp into `out`, reusing `out`'s capacity.
pub fn push_format_time_at(seconds: i64, now: i64, datetime: DateTime, out: &mut String) {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const HALF_YEAR: i64 = 15_778_476;

    out.clear();
    out.push_str(MONTHS[(datetime.month - 1) as usize]);
    out.push(' ');

    if datetime.day < 10 {
        out.push(' ');
        push_digit(out, datetime.day as u8);
    } else {
        push_two_digits(out, datetime.day as u8);
    }

    if seconds < now && now - seconds <= HALF_YEAR {
        out.push(' ');
        push_two_digits(out, datetime.hour as u8);
        out.push(':');
        push_two_digits(out, datetime.minute as u8);
    } else {
        out.push_str("  ");
        // nstrftime's `%Y` zero-pads to four digits. Keep the common year range
        // on the manual fast path; `{:04}` handles negative years and the rare
        // years beyond 9999 with the same padding and sign.
        if (0..=9999).contains(&datetime.year) {
            let year = datetime.year as u16;
            out.push((b'0' + (year / 1000) as u8) as char);
            out.push((b'0' + (year / 100 % 10) as u8) as char);
            out.push((b'0' + (year / 10 % 10) as u8) as char);
            out.push((b'0' + (year % 10) as u8) as char);
        } else {
            let _ = write!(out, "{:04}", datetime.year);
        }
    }
}

fn push_digit(out: &mut String, digit: u8) {
    out.push((b'0' + digit) as char);
}

fn push_two_digits(out: &mut String, value: u8) {
    out.push((b'0' + value / 10) as char);
    out.push((b'0' + value % 10) as char);
}

pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;

    (if month <= 2 { year + 1 } else { year }, month, day)
}

pub fn init_locale() {
    unsafe {
        setlocale(LC_ALL, c"".as_ptr());
    }
}

type TimeT = c_long;

#[repr(C)]
#[allow(dead_code)]
struct Tm {
    tm_sec: c_int,
    tm_min: c_int,
    tm_hour: c_int,
    tm_mday: c_int,
    tm_mon: c_int,
    tm_year: c_int,
    tm_wday: c_int,
    tm_yday: c_int,
    tm_isdst: c_int,
    tm_gmtoff: c_long,
    tm_zone: *const c_char,
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
const LC_ALL: c_int = 0;
#[cfg(not(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
const LC_ALL: c_int = 6;

#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[cfg(target_os = "linux")]
const TIOCGWINSZ: c_ulong = 0x5413;
#[cfg(not(target_os = "linux"))]
const TIOCGWINSZ: c_ulong = 0x4008_7468;

extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn localtime_r(timep: *const TimeT, result: *mut Tm) -> *mut Tm;
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
}

/// Return the terminal width in columns, from `COLUMNS` or `ioctl(TIOCGWINSZ)`,
/// falling back to 80.
pub fn terminal_width() -> usize {
    if let Some(columns) = env::var_os("COLUMNS")
        .and_then(|value| value.to_str().and_then(|value| value.parse::<usize>().ok()))
    {
        if columns > 0 {
            return columns;
        }
    }

    let mut size = WinSize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let result = unsafe { ioctl(1, TIOCGWINSZ, &mut size as *mut WinSize) };

    if result == 0 && size.ws_col > 0 {
        size.ws_col as usize
    } else {
        80
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    extern "C" {
        fn tzset();
    }

    /// `TZ` is process-wide; tests that change it take this lock.
    static TZ_LOCK: Mutex<()> = Mutex::new(());

    /// Compare `local_time` with `localtime_r` for `zone` at every `step`
    /// seconds from 1960 to 2040, twice so the second pass uses probed days.
    /// Returns the fraction of days that got a fixed offset.
    fn check_zone(zone: &str, step: i64) -> f64 {
        env::set_var("TZ", zone);
        unsafe { tzset() };
        DAYS.with_borrow_mut(DayMap::clear);
        let times: Vec<i64> = (-315_619_200..2_208_988_800)
            .step_by(step as usize)
            .collect();
        for _ in 0..2 {
            for &time in &times {
                assert_eq!(
                    Some(local_time(time)),
                    local_datetime(time),
                    "{zone} at {time}"
                );
            }
        }
        let shared = DayOffsets::for_times(times.iter().copied());
        for &time in &times {
            assert_eq!(
                Some(shared.local_time(time)),
                local_datetime(time),
                "{zone} at {time}"
            );
        }
        DAYS.with_borrow(|days| {
            let fixed = days
                .values()
                .filter(|offset| matches!(offset, DayOffset::Known(Some(_))))
                .count();
            fixed as f64 / days.len() as f64
        })
    }

    #[test]
    fn local_time_matches_localtime_r() {
        let _guard = TZ_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let zones = [
            "America/New_York",
            "Europe/London",
            "Australia/Lord_Howe", // 30-minute DST
            "Asia/Kathmandu",      // +05:45
            "Pacific/Apia",        // skipped 2011-12-30 entirely
            "Africa/Casablanca",   // DST suspended during Ramadan
            "right/America/New_York",
            "UTC",
        ];
        for zone in zones {
            if !std::path::Path::new("/usr/share/zoneinfo")
                .join(zone)
                .exists()
            {
                continue;
            }
            let cached = check_zone(zone, 7919);
            if zone.starts_with("right/") {
                // Leap-second zones fall back to localtime_r except before
                // the first leap second (1972), when no correction applies.
                assert!(cached < 0.05, "{zone}: {cached:.2} of days cached");
            } else {
                assert!(cached > 0.6, "{zone}: only {cached:.2} of days cached");
            }
        }
        env::remove_var("TZ");
        unsafe { tzset() };
    }

    /// Every zone on the system; slow, so run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn local_time_matches_localtime_r_in_every_zone() {
        let _guard = TZ_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::path::Path::new("/usr/share/zoneinfo");
        let mut stack = vec![root.to_path_buf()];
        let mut zones = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if std::fs::read(&path).is_ok_and(|data| data.starts_with(b"TZif")) {
                    check_zone(path.strip_prefix(root).unwrap().to_str().unwrap(), 86_413);
                    zones += 1;
                }
            }
        }
        assert!(zones > 300, "found only {zones} zones");
        env::remove_var("TZ");
        unsafe { tzset() };
    }
}
