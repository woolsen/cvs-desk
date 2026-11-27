//! The encoding of CVS control files, and the timestamp text they carry.
//!
//! CVSNT writes `Entries`, `Root` and `Repository` in the console's code page, not UTF-8.
//! A Chinese Windows uses 936 (GBK), and a real working copy proves it: its `Entries` bytes
//! are not valid UTF-8. The client therefore has to *encode* as well as decode.
//!
//! A new working copy gets the system's ANSI code page (936 on a Chinese Windows), which is
//! what CVSNT would write. An existing working copy keeps whatever its `Entries` already
//! uses, so a mixed tree is never created by this client.
//!
//! Timestamps are `asctime`-shaped, cut to 24 characters: `Tue Nov 24 09:52:53 2026`. The
//! desktop program compares that text with the file's own mtime text, so the client has to
//! write exactly the same shape.

use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local, NaiveDateTime, TimeZone, Timelike, Utc};

use crate::client::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Utf8,
    Gbk,
}

impl Codec {
    pub fn decode(self, bytes: &[u8]) -> String {
        match self {
            Codec::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            Codec::Gbk => {
                let (text, _, _) = encoding_rs::GBK.decode(bytes);
                text.into_owned()
            }
        }
    }

    pub fn encode(self, text: &str) -> Result<Vec<u8>> {
        match self {
            Codec::Utf8 => Ok(text.as_bytes().to_vec()),
            Codec::Gbk => {
                let (bytes, _, had_errors) = encoding_rs::GBK.encode(text);
                if had_errors {
                    return Err(Error::new(format!(
                        "the text '{}' cannot be written in GBK",
                        text
                    )));
                }
                Ok(bytes.into_owned())
            }
        }
    }
}

/// The codec a control file's bytes are in: valid UTF-8 stays UTF-8, anything else is GBK.
pub fn detect(bytes: &[u8]) -> Codec {
    if std::str::from_utf8(bytes).is_ok() {
        Codec::Utf8
    } else {
        Codec::Gbk
    }
}

/// The codec to use for a folder: follow an existing `Entries`, else the system ANSI page.
pub fn for_dir(dir: &Path) -> Codec {
    match std::fs::read(dir.join("CVS").join("Entries")) {
        Ok(bytes) => detect(&bytes),
        Err(_) => system_ansi(),
    }
}

/// The Windows ANSI code page as a codec. 936 is GBK; everything else is written as UTF-8
/// until this client grows tables for the other pages.
pub fn system_ansi() -> Codec {
    #[cfg(windows)]
    {
        if unsafe { win::GetACP() } == 936 {
            return Codec::Gbk;
        }
    }
    Codec::Utf8
}

/// The code page the console is using. Used for text printed to a terminal; a pipe is always
/// UTF-8 so a GUI reader never has to guess.
pub fn console() -> Codec {
    #[cfg(windows)]
    {
        let handle = unsafe { win::GetStdHandle(-11i32 as u32) };
        if unsafe { win::GetFileType(handle) } == 2 {
            if unsafe { win::GetConsoleOutputCP() } == 936 {
                return Codec::Gbk;
            }
        }
    }
    Codec::Utf8
}

#[cfg(windows)]
mod win {
    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetACP() -> u32;
        pub fn GetConsoleOutputCP() -> u32;
        pub fn GetStdHandle(which: u32) -> isize;
        pub fn GetFileType(handle: isize) -> u32;
    }
}

/// `asctime`-shaped text for a file time, in **UTC**.
///
/// CVSNT writes the server's revision time into `Entries` without converting it to the
/// client's zone; the real working copy proves it (`01:19:30` in `Entries`, `09:19:30` on
/// disk in China Standard Time). Writing the same shape keeps a checked-out file "up to date"
/// for CVS and for the desktop program once it compares in UTC too.
pub fn asctime(time: SystemTime) -> String {
    let utc: DateTime<Utc> = time.into();
    format_asctime(&utc.naive_utc())
}

/// The same text for a naive local time. `%e` is the space-padded day CVS uses.
pub fn format_asctime(naive: &NaiveDateTime) -> String {
    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {}",
        weekday(naive),
        month(naive),
        naive.day(),
        naive.hour(),
        naive.minute(),
        naive.second(),
        naive.year()
    )
}

fn weekday(naive: &NaiveDateTime) -> &'static str {
    ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        [naive.weekday().num_days_from_monday() as usize]
}

fn month(naive: &NaiveDateTime) -> &'static str {
    [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][naive.month0() as usize]
}

/// Read one of CVS's 24-character timestamps back. The text has no zone; CVS stores the
/// repository's revision time, which this client treats as UTC.
pub fn parse_asctime(text: &str) -> Option<SystemTime> {
    let text = text.trim();
    if text.len() != 24 {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(text, "%a %b %e %H:%M:%S %Y").ok()?;
    Some(Utc.from_utc_datetime(&naive).into())
}

/// The `Mod-time` response's shape: `24 Nov 2026 09:52:53 -0000`.
pub fn parse_mod_time(text: &str) -> Option<SystemTime> {
    let time = DateTime::parse_from_str(text.trim(), "%d %b %Y %H:%M:%S %z").ok()?;
    Some(time.with_timezone(&Local).into())
}

/// The line ending CVS uses in control files on this platform. CVSNT writes CRLF on
/// Windows; a Unix CVS writes LF.
pub fn newline() -> &'static str {
    if cfg!(windows) {
        "\r\n"
    } else {
        "\n"
    }
}

/// Convert text-file contents to the platform's line ending, the way CVS's checkout does.
///
/// A byte pair already written as CRLF is left alone, so a repository that stores CRLF does
/// not become CRCRLF. Binary (`-kb`) and Unicode (`-ku`) entries never take this path.
pub fn to_platform_newlines(content: &[u8]) -> Vec<u8> {
    if !cfg!(windows) {
        return content.to_vec();
    }
    let mut out = Vec::with_capacity(content.len() + content.len() / 32);
    let mut previous = 0u8;
    for &byte in content {
        if byte == b'\n' && previous != b'\r' {
            out.push(b'\r');
        }
        out.push(byte);
        previous = byte;
    }
    out
}

/// Convert a working text file back to the line ending the repository stores (LF).
///
/// The reference client does this before sending a modified file: a CRLF working copy must
/// not look changed merely because of its line endings.
pub fn to_repository_newlines(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len());
    for (index, &byte) in content.iter().enumerate() {
        if byte == b'\r' && content.get(index + 1) == Some(&b'\n') {
            continue;
        }
        out.push(byte);
    }
    out
}

/// A file's current modification time.
pub fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Set a file's modification time. The file must be writable; call this before making a
/// checked-out file read-only.
pub fn set_file_mtime(path: &Path, time: SystemTime) -> Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    let times = std::fs::FileTimes::new().set_modified(time);
    file.set_times(times)?;
    Ok(())
}

/// Compare two 24-character `asctime` texts the way CVS does: a `'0'` in the day-of-month
/// column counts the same as a space.
pub fn timestamps_equal(a: &str, b: &str) -> bool {
    let mut a = a.as_bytes().to_vec();
    let mut b = b.as_bytes().to_vec();
    if a.len() < 24 || b.len() < 24 {
        return a == b;
    }
    if a[8] == b'0' && b[8] == b' ' {
        a[8] = b' ';
    }
    if a[8] == b' ' && b[8] == b'0' {
        b[8] = b' ';
    }
    a[..24] == b[..24]
}

/// Is the entry date one CVS wrote to say a merge left conflicts?
pub fn is_conflict_date(date: &str) -> bool {
    date.trim().starts_with("Result of merge")
}

/// Is this entry date one CVS wrote to say an update could not finish?
pub fn is_unfinished_date(date: &str) -> bool {
    date.starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn utf8_and_gbk_both_round_trip() {
        let text = "知识库";
        let utf8 = Codec::Utf8.encode(text).unwrap();
        assert_eq!(Codec::Utf8.decode(&utf8), text);
        let gbk = Codec::Gbk.encode(text).unwrap();
        assert_ne!(gbk, utf8);
        assert_eq!(Codec::Gbk.decode(&gbk), text);
        assert_eq!(detect(&utf8), Codec::Utf8);
        assert_eq!(detect(&gbk), Codec::Gbk);
    }

    #[test]
    fn asctime_is_twenty_four_characters_and_space_pads_the_day() {
        let naive = NaiveDate::from_ymd_opt(2026, 11, 4)
            .unwrap()
            .and_hms_opt(9, 52, 53)
            .unwrap();
        let text = format_asctime(&naive);
        assert_eq!(text, "Wed Nov  4 09:52:53 2026");
        assert_eq!(text.len(), 24);
        let back = parse_asctime(&text).unwrap();
        let utc: DateTime<Utc> = back.into();
        assert_eq!(utc.naive_utc(), naive);
    }

    #[test]
    fn the_two_timestamp_shapes_cvs_uses_are_both_understood() {
        assert!(parse_asctime("Tue Nov 24 09:52:53 2026").is_some());
        assert!(parse_asctime("Result of merge").is_none());
        assert!(parse_asctime("/patched").is_none());
        assert!(parse_mod_time("24 Nov 2026 09:52:53 -0000").is_some());
        assert!(parse_mod_time("not a time").is_none());
    }

    #[test]
    fn the_conflict_and_unfinished_markers_are_recognised() {
        assert!(is_conflict_date("Result of merge+Tue Nov 24 09:52:53 2026"));
        assert!(is_conflict_date("Result of merge"));
        assert!(!is_conflict_date("Tue Nov 24 09:52:53 2026"));
        assert!(is_unfinished_date("/patched"));
        assert!(!is_unfinished_date("Tue Nov 24 09:52:53 2026"));
    }

    #[test]
    fn a_zero_padded_day_matches_a_space_padded_one() {
        assert!(timestamps_equal(
            "Thu Nov 05 09:14:03 2026",
            "Thu Nov  5 09:14:03 2026"
        ));
        assert!(!timestamps_equal(
            "Thu Nov  5 09:14:03 2026",
            "Thu Nov  5 09:14:04 2026"
        ));
    }

    #[test]
    fn text_line_endings_go_out_as_crlf_and_back_as_lf() {
        let repository = b"a\nb\n";
        let platform = to_platform_newlines(repository);
        if cfg!(windows) {
            assert_eq!(platform, b"a\r\nb\r\n");
        } else {
            assert_eq!(platform, repository);
        }
        assert_eq!(to_repository_newlines(&platform), repository);
        // A file that already stores CRLF is not doubled.
        assert_eq!(to_platform_newlines(b"a\r\nb"), b"a\r\nb");
    }
}
