use std::env;
use std::ffi::OsString;
use std::io::Write;

mod args;
mod color;
mod entry;
mod ids;
mod layout;
mod lister;
mod permissions;
mod quoting;
mod sizes;
mod sort;
mod time;

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

    let output_error = match lister.output_error.take() {
        Some(err) => Some(err),
        None => lister.out.flush().err(),
    };
    if let Some(err) = output_error {
        eprintln!("ls: write error: {}", err);
        code = 2;
    }

    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use crate::quoting::quote_name;
    use crate::sizes::{format_blocks, human_size};
    use crate::time::{civil_from_days, format_time_at, utc_datetime, DateTime};

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(0), "0");
        assert_eq!(human_size(512), "512");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(10 * 1024), "10K");
        assert_eq!(human_size(1024 * 1024), "1.0M");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0G");
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
    fn block_counts_match_gnu_units() {
        assert_eq!(format_blocks(0, false), "0");
        assert_eq!(format_blocks(1, false), "0");
        assert_eq!(format_blocks(8, false), "4");
        assert_eq!(format_blocks(8, true), "4.0K");
        assert_eq!(format_blocks(1, true), "512");
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
}
