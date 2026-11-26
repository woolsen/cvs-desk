//! The date text `rlog -d` and `rannotate -D` put on the wire.
//!
//! The user types a date in local time, but CVS's protocol wants it in UTC as
//! `31 Dec 2019 16:00:00 -0000`: English month names, no leading zero on the day, and a
//! literal `-0000`. A date range (`D1<D2`) is normalized on both sides. A value this client
//! cannot parse is passed through unchanged, and the server parses it with its own rules.

use chrono::{
    DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc,
};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Normalize one date or a `D1<D2` range, leaving text that cannot be parsed as it is.
pub(crate) fn normalize(text: &str) -> String {
    // The reference turns `D1>D2` into `D2<D1`; `<=` keeps its direction.
    for (separator, reversed, output) in [
        ("<=", false, "<="),
        (">=", true, "<="),
        ("<", false, "<"),
        (">", true, "<"),
    ] {
        if let Some((left, right)) = text.split_once(separator) {
            let (left, right) = if reversed {
                (right, left)
            } else {
                (left, right)
            };
            return format!("{}{}{}", one(left), output, one(right));
        }
    }
    one(text)
}

/// Normalize a single date: local time in, UTC text out.
fn one(text: &str) -> String {
    match parse(text.trim()) {
        Some(time) => utc_text(time),
        None => text.to_string(),
    }
}

fn parse(text: &str) -> Option<DateTime<Utc>> {
    if text.is_empty() {
        return None;
    }

    // The reference's own output, and `asctime` text, already carry a zone or are UTC.
    if let Ok(time) = DateTime::parse_from_str(text, "%d %b %Y %H:%M:%S %z") {
        return Some(time.with_timezone(&Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(text, "%a %b %e %H:%M:%S %Y") {
        return Some(Utc.from_utc_datetime(&naive));
    }

    if let Some(time) = relative(text) {
        return Some(time);
    }

    for format in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
        "%m/%d/%Y %H:%M:%S",
        "%m/%d/%Y %H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return local_to_utc(naive);
        }
    }

    for format in ["%Y-%m-%d", "%m/%d/%Y", "%d-%b-%Y", "%d %b %Y"] {
        if let Ok(date) = NaiveDate::parse_from_str(text, format) {
            let naive = date.and_hms_opt(0, 0, 0)?;
            return local_to_utc(naive);
        }
    }

    None
}

fn relative(text: &str) -> Option<DateTime<Utc>> {
    let now = Local::now();
    let time = match text.to_ascii_lowercase().as_str() {
        "now" | "today" => now,
        "tomorrow" => now + Duration::days(1),
        "yesterday" => now - Duration::days(1),
        _ => return None,
    };
    Some(time.with_timezone(&Utc))
}

/// A local wall-clock time in UTC. An ambiguous or impossible time (a DST fold or gap) is
/// left for the server instead of guessing.
fn local_to_utc(naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|time| time.with_timezone(&Utc))
}

fn utc_text(time: DateTime<Utc>) -> String {
    format!(
        "{} {} {} {:02}:{:02}:{:02} -0000",
        time.day(),
        MONTHS[time.month0() as usize],
        time.year(),
        time.hour(),
        time.minute(),
        time.second()
    )
}

/// A file's own time as the `Checkin-time` request carries it, with the reference's
/// `1 Jan 2020 00:00:00 -0000` shape.
pub(crate) fn internet_from_system(time: std::time::SystemTime) -> String {
    utc_text(time.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The local midnight the reference converts, in UTC text.
    fn midnight(year: i32, month: u32, day: u32) -> String {
        let naive = NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        utc_text(local_to_utc(naive).unwrap())
    }

    #[test]
    fn a_local_day_becomes_the_reference_utc_text() {
        assert_eq!(normalize("2020-01-01"), midnight(2020, 1, 1));
        assert_eq!(normalize("2020-01-01 12:30:00"), {
            let naive = NaiveDate::from_ymd_opt(2020, 1, 1)
                .unwrap()
                .and_hms_opt(12, 30, 0)
                .unwrap();
            utc_text(local_to_utc(naive).unwrap())
        });
        assert_eq!(normalize("01-Jan-2020"), midnight(2020, 1, 1));
        // The day is not zero-padded, as in the reference's own text.
        assert!(!normalize("2020-01-01").starts_with('0'));
        assert!(normalize("2020-01-01").ends_with(" -0000"));
    }

    #[test]
    fn a_normalized_date_round_trips() {
        let text = "31 Dec 2019 16:00:00 -0000";
        assert_eq!(normalize(text), text);
    }

    #[test]
    fn a_range_is_normalized_on_both_sides() {
        let expected = format!("{}<{}", midnight(2020, 1, 1), midnight(2020, 2, 1));
        assert_eq!(normalize("2020-01-01<2020-02-01"), expected);
        assert_eq!(
            normalize("2020-01-01<=2020-02-01"),
            format!("{}<={}", midnight(2020, 1, 1), midnight(2020, 2, 1))
        );
    }

    #[test]
    fn a_reversed_range_is_flipped_like_the_reference() {
        assert_eq!(
            normalize("2020-02-01>2020-01-01"),
            format!("{}<{}", midnight(2020, 1, 1), midnight(2020, 2, 1))
        );
        assert_eq!(
            normalize("2020-02-01>=2020-01-01"),
            format!("{}<={}", midnight(2020, 1, 1), midnight(2020, 2, 1))
        );
    }

    #[test]
    fn text_that_is_not_a_date_is_passed_through() {
        for text in ["garbage", "last tuesday", "", "1 day ago"] {
            assert_eq!(normalize(text), text);
        }
    }
}

#[cfg(test)]
mod internet_tests {
    use super::*;

    #[test]
    fn a_file_time_becomes_the_reference_internet_text() {
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
        assert_eq!(internet_from_system(time), "1 Jan 2020 00:00:00 -0000");
    }
}
