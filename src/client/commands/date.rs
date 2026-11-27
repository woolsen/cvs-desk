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

/// A `log -d` value: `;`-separated dates and ranges. Every item is normalized like
/// [`normalize`], and the values are returned in the order the user wrote them, one per
/// `Argument -d`. A value that is clearly not a date, or a range whose start is after its
/// end, is refused with the offending item as the error text.
pub(crate) fn normalize_dates(text: &str) -> std::result::Result<Vec<String>, String> {
    text.split(';')
        .map(|item| normalize_date_item(item.trim()))
        .collect()
}

fn normalize_date_item(text: &str) -> std::result::Result<String, String> {
    if text.is_empty() {
        return Err(text.to_string());
    }

    let Some(range) = range_parts(text) else {
        return match parse(text) {
            Some(time) => Ok(utc_text(time)),
            // Text this module cannot parse but the server's getdate may understand (month
            // names, relative phrases, other separators) is passed through, as before.
            None if date_like(text) => Ok(text.to_string()),
            None => Err(text.to_string()),
        };
    };

    // Compare only when both sides are dates this module understands; a phrase only the
    // server can read is left for the server to check.
    for side in [range.start, range.end] {
        let side = side.trim();
        if !side.is_empty() && parse(side).is_none() && !date_like(side) {
            return Err(text.to_string());
        }
    }

    let start = endpoint(range.start, default_start());
    let end = endpoint(range.end, default_end());
    if let (Some(start), Some(end)) = (start, end) {
        if start > end {
            return Err(text.to_string());
        }
        return Ok(format!(
            "{}{}{}",
            utc_text(start),
            if range.inclusive { "<=" } else { "<" },
            utc_text(end)
        ));
    }

    // At least one side is a phrase only the server understands; keep the usual shape.
    Ok(normalize(text))
}

fn endpoint(text: &str, default: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if text.is_empty() {
        Some(default)
    } else {
        parse(text)
    }
}

/// The reference's `1/1/1970 UTC` for an open start of a range.
fn default_start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap()
}

/// The reference's `2038-01-01` for an open end of a range.
fn default_end() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2038, 1, 1, 0, 0, 0).unwrap()
}

struct RangeParts<'a> {
    start: &'a str,
    end: &'a str,
    inclusive: bool,
}

/// Split `D1<D2`, `D1<=D2`, `D2>D1` or `D2>=D1` into its two ends. As in the reference, a
/// `>` range is reversed into a `<` range.
fn range_parts(text: &str) -> Option<RangeParts<'_>> {
    if let Some((end, rest)) = text.split_once('>') {
        let (start, inclusive) = match rest.strip_prefix('=') {
            Some(start) => (start, true),
            None => (rest, false),
        };
        return Some(RangeParts {
            start,
            end,
            inclusive,
        });
    }
    if let Some((start, rest)) = text.split_once('<') {
        let (end, inclusive) = match rest.strip_prefix('=') {
            Some(end) => (end, true),
            None => (rest, false),
        };
        return Some(RangeParts {
            start,
            end,
            inclusive,
        });
    }
    None
}

/// Whether text at least looks like something the server's date parser might accept. This is
/// the same tolerance `tag -D` uses: refuse only text with nothing date-like in it.
fn date_like(text: &str) -> bool {
    const WORDS: [&str; 27] = [
        "now",
        "today",
        "tomorrow",
        "yesterday",
        "last",
        "next",
        "ago",
        "this",
        "jan",
        "feb",
        "mar",
        "apr",
        "may",
        "jun",
        "jul",
        "aug",
        "sep",
        "oct",
        "nov",
        "dec",
        "mon",
        "tue",
        "wed",
        "thu",
        "fri",
        "sat",
        "sun",
    ];
    let lower = text.to_ascii_lowercase();
    let has_word = lower
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|part| WORDS.contains(&part));
    has_word
        || (text.chars().any(|c| c.is_ascii_digit()) && text.chars().any(|c| "-/.:,".contains(c)))
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

    #[test]
    fn a_date_list_is_split_on_semicolons() {
        let values = normalize_dates(&format!("2020-01-01;{}", "2020-02-01<2020-03-01")).unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], midnight(2020, 1, 1));
        assert_eq!(
            values[1],
            format!("{}<{}", midnight(2020, 2, 1), midnight(2020, 3, 1))
        );
    }

    #[test]
    fn a_range_that_runs_backwards_is_refused() {
        let error = normalize_dates("2020-02-01<2020-01-01").unwrap_err();
        assert_eq!(error, "2020-02-01<2020-01-01");
    }

    #[test]
    fn a_reversed_greater_than_range_is_flipped_not_refused() {
        let values = normalize_dates("2020-02-01>2020-01-01").unwrap();
        assert_eq!(
            values,
            vec![format!("{}<{}", midnight(2020, 1, 1), midnight(2020, 2, 1))]
        );
    }

    #[test]
    fn an_open_range_gets_the_reference_defaults() {
        let values = normalize_dates("<2020-01-01").unwrap();
        assert_eq!(
            values[0],
            format!("{}<{}", utc_text(default_start()), midnight(2020, 1, 1))
        );
    }

    #[test]
    fn text_with_nothing_date_like_is_refused() {
        assert!(normalize_dates("garbage").is_err());
        assert!(normalize_dates("2020-01-01;not a date at all").is_err());
        // A phrase the server's getdate understands is left for the server.
        assert_eq!(
            normalize_dates("last tuesday").unwrap(),
            vec!["last tuesday"]
        );
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
