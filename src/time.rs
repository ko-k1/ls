use std::env;
use std::os::raw::{c_char, c_int, c_long, c_ulong};
use std::time::{SystemTime, UNIX_EPOCH};

/// Broken-down calendar date/time.
#[derive(Clone, Copy)]
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
    let time: TimeT = seconds as TimeT;
    let mut tm = std::mem::MaybeUninit::<Tm>::uninit();
    let result = unsafe { localtime_r(&time, tm.as_mut_ptr()) };

    if result.is_null() {
        return None;
    }

    let tm = unsafe { tm.assume_init() };

    Some(DateTime {
        year: tm.tm_year as i64 + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as i64,
        minute: tm.tm_min as i64,
    })
}

pub fn format_time(seconds: i64, now: i64) -> String {
    let datetime = local_datetime(seconds).unwrap_or_else(|| utc_datetime(seconds));
    format_time_at(seconds, now, datetime)
}

pub fn format_time_at(seconds: i64, now: i64, datetime: DateTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const HALF_YEAR: i64 = 15_778_476;

    let month_name = MONTHS[(datetime.month - 1) as usize];

    if seconds < now && now - seconds <= HALF_YEAR {
        format!(
            "{} {:>2} {:02}:{:02}",
            month_name, datetime.day, datetime.hour, datetime.minute
        )
    } else {
        format!("{} {:>2}  {}", month_name, datetime.day, datetime.year)
    }
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
