use std::env;
use std::ffi::OsString;
use std::io::Write;

mod args;
mod collate;
mod color;
mod dir;
mod entry;
mod format;
mod ids;
mod layout;
mod lister;
mod permissions;
mod quoting;
mod render;
mod sizes;
mod sort;
mod time;
mod walk;

use crate::args::parse_args;
use crate::lister::Lister;
use crate::time::init_locale;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    init_locale();

    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let (opts, operands) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("ls: {}", err);
            eprintln!("Try 'ls --help' for more information.");
            std::process::exit(2);
        }
    };

    let mut lister = Lister::new(opts);
    let mut code = lister.run(operands);

    let output_error = match lister.output.output_error.take() {
        Some(err) => Some(err),
        None => lister.output.out.flush().err(),
    };
    if let Some(err) = output_error {
        eprintln!("ls: write error: {}", err);
        code = 2;
    }

    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use crate::collate::{KeyMode, Level1Table};
    use crate::quoting::quote_name;
    use crate::sizes::{format_blocks, human_size};
    use crate::time::{
        civil_from_days, format_time_at, is_recent, utc_datetime, DateTime, Timestamp,
    };

    #[test]
    fn human_sizes_match_gnu_ls() {
        // Expected values were produced by GNU coreutils 9.12 `ls -lh` on
        // sparse files of each size. GNU rounds up, never to nearest.
        let cases: &[(u64, &str)] = &[
            (0, "0"),
            (1, "1"),
            (512, "512"),
            (1023, "1023"),
            (1024, "1.0K"),
            (1025, "1.1K"),
            (1030, "1.1K"),
            (1536, "1.5K"),
            (2047, "2.0K"),
            (10188, "10K"),
            (10230, "10K"),
            (10239, "10K"),
            (10240, "10K"),
            (10241, "11K"),
            (10300, "11K"),
            (102399, "100K"),
            (1047552, "1023K"),
            (1048064, "1.0M"),
            (1048575, "1.0M"),
            (1048576, "1.0M"),
            (1048577, "1.1M"),
            (10433331, "10M"),
            (10485248, "10M"),
            (1073741824, "1.0G"),
            (3221225472, "3.0G"),
            (1099511627777, "1.1T"),
            (1152921504606846976, "1.0E"),
            (9223372036854775807, "8.0E"),
            (u64::MAX, "16E"),
        ];
        for &(bytes, expected) in cases {
            assert_eq!(human_size(bytes), expected, "human_size({bytes})");
        }
    }

    #[test]
    fn dates_are_converted_from_unix_time() {
        let now = 4_000_000_000;
        assert_eq!(format_time_at(0, now, utc_datetime(0)), "Jan  1  1970");
        assert_eq!(
            format_time_at(86_400, now, utc_datetime(86_400)),
            "Jan  2  1970"
        );
        assert_eq!(
            format_time_at(951_782_400, now, utc_datetime(951_782_400)),
            "Feb 29  2000"
        );
    }

    #[test]
    fn years_are_zero_padded_like_gnu_ls() {
        let now = 4_000_000_000;
        let date = |year| DateTime {
            year,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
        };

        assert_eq!(format_time_at(0, now, date(-1)), "Jan  1  -001");
        assert_eq!(format_time_at(0, now, date(0)), "Jan  1  0000");
        assert_eq!(format_time_at(0, now, date(500)), "Jan  1  0500");
        assert_eq!(format_time_at(0, now, date(999)), "Jan  1  0999");
        assert_eq!(format_time_at(0, now, date(1000)), "Jan  1  1000");
        assert_eq!(format_time_at(0, now, date(9999)), "Jan  1  9999");
        assert_eq!(format_time_at(0, now, date(10000)), "Jan  1  10000");
    }

    #[test]
    fn recent_times_are_one_sided() {
        let now = 1_700_000_100;
        assert_eq!(
            format_time_at(1_700_000_000, now, utc_datetime(1_700_000_000)),
            "Nov 14 22:13"
        );
        assert_eq!(format_time_at(now, now, utc_datetime(now)), "Nov 14  2023");
        assert_eq!(
            format_time_at(now + 60, now, utc_datetime(now + 60)),
            "Nov 14  2023"
        );
    }

    #[test]
    fn recency_compares_nanoseconds_like_gnu_ls() {
        let at = |seconds, nanoseconds| Timestamp {
            seconds,
            nanoseconds,
        };
        let now = at(1_700_000_100, 900_000_000);
        // Modified earlier within the same second: a time of day, not a year.
        assert!(is_recent(at(1_700_000_100, 300_000_000), now));
        assert!(!is_recent(now, now));
        assert!(!is_recent(at(1_700_000_100, 900_000_001), now));
        // Exactly six months old is no longer recent; a nanosecond less is.
        let half_year = 15_778_476;
        assert!(!is_recent(
            at(now.seconds - half_year, now.nanoseconds),
            now
        ));
        assert!(is_recent(
            at(now.seconds - half_year, now.nanoseconds + 1),
            now
        ));
    }

    #[test]
    fn block_counts_match_gnu_units() {
        // GNU ls reports 1024-byte units rounded up (checked against
        // coreutils 9.12 with faked odd st_blocks values).
        assert_eq!(format_blocks(0, false), "0");
        assert_eq!(format_blocks(1, false), "1");
        assert_eq!(format_blocks(3, false), "2");
        assert_eq!(format_blocks(8, false), "4");
        assert_eq!(format_blocks(2047, false), "1024");
        assert_eq!(format_blocks(8, true), "4.0K");
        assert_eq!(format_blocks(1, true), "512");
        assert_eq!(format_blocks(3, true), "1.5K");
        assert_eq!(format_blocks(2047, true), "1.0M");
        assert_eq!(format_blocks(2049, true), "1.1M");
        assert_eq!(format_blocks(20479, true), "10M");
    }

    #[test]
    fn filenames_are_quoted_for_terminals() {
        assert_eq!(quote_name("plain.txt", true), "plain.txt");
        assert_eq!(quote_name("a b", true), "'a b'");
        assert_eq!(quote_name("quote'single", true), "\"quote'single\"");
        assert_eq!(quote_name("line\nbreak", true), "$'line\\nbreak'");
        assert_eq!(quote_name("\x1b[31mred", true), "$'\\033[31mred'");
        assert_eq!(quote_name("both'\"quotes", true), "'both'\\''\"quotes'");
    }

    #[test]
    fn piped_names_protect_record_boundaries() {
        assert_eq!(quote_name("line\nbreak", false), "line?break");
        assert_eq!(quote_name("a b", false), "a b");
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn level1_keys_drop_ignored_characters_and_fold_case() {
        let table: &'static Level1Table = Box::leak(Box::new(Level1Table::expected()));
        let mode = KeyMode::Level1(table);
        let key = |name: &str| {
            let mut out = Vec::new();
            mode.push_key(name.as_bytes(), &mut out).then_some(out)
        };

        assert_eq!(key("IMG_2023.jpg"), key("img2023jpg"));
        assert_eq!(key("a-b c.d(1)"), key("abcd1"));
        assert_eq!(key("_-. "), Some(Vec::new()));
        assert!(key("$") < key("0") && key("0") < key("9") && key("9") < key("a"));
        assert!(key("a") < key("B") && key("Y") < key("z"));
        assert!(key("ab") < key("ab_c"));
        assert_eq!(key("café"), None);
        assert_eq!(key("tab\there"), None);

        // A name without a key leaves earlier keys in the buffer intact.
        let mut out = b"abc".to_vec();
        assert!(!mode.push_key("資料".as_bytes(), &mut out));
        assert_eq!(out, b"abc");
    }
}
