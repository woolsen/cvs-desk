//! `cvs history`: the repository's record of checkouts, commits and tags.
//!
//! No working copy is needed: the client sends the report options and prints the lines the
//! server returns. Options are read the way the reference's
//! `getopt ("+Tacelow?D:b:f:m:n:p:r:t:u:x:X:z:")` reads them: short flags may be clustered
//! (`-cl`, `-cw`, `-al`), an attached value stays with its letter (`-D2020-01-01`,
//! `-z-0700`), `-?` and getopt errors print the usage to stderr, and the first name ends
//! option parsing (everything after it is a file). The options go on the wire in the
//! reference's fixed order — the report flags first, then the date, the file and path filters
//! (`-f`/`-p` in the order typed), the modules, the revision, the tag, the users, the record
//! types, and always the time zone (`+0000` unless a valid `-z` says otherwise). A report
//! type may be given only once, and a later date, revision or tag cancels the earlier
//! filters with a warning, the way the reference does.

use crate::client::cli;
use crate::client::commands::{date, tag, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

/// Every record type `-e` and `-x` know about, in the reference's order.
const RECORD_TYPES: &str = "TOEFWUPCGMAR";

/// The zone names `getdate` recognizes after the time, so `-z` can be checked locally.
/// The reference's `TimezoneTable` (`lib/getdate.y`); military one-letter zones are handled
/// separately because its `MilitaryTable` has no `j`.
const TIMEZONES: [&str; 49] = [
    "gmt", "ut", "utc", "wet", "bst", "wat", "at", "ast", "adt", "est", "edt", "cst", "cdt", "mst",
    "mdt", "pst", "pdt", "yst", "ydt", "hst", "hdt", "cat", "ahst", "nt", "idlw", "cet", "met",
    "mewt", "mest", "swt", "sst", "fwt", "fst", "eet", "bt", "zp4", "zp5", "zp6", "wast", "wadt",
    "cct", "jst", "east", "eadt", "gst", "nzt", "nzst", "nzdt", "idle",
];

/// One `-f`/`-p` entry or a name typed on the command line, kept in the order it arrived.
#[derive(Debug, PartialEq, Eq)]
enum FileSpec {
    File(String),
    Path(String),
}

#[derive(Debug, Default)]
struct Options {
    reports: u32,
    tag_report: bool,
    all_users: bool,
    committed: bool,
    everything: bool,
    last_entry: bool,
    module_report: bool,
    checked_out: bool,
    working: bool,
    since_date: Option<String>,
    backto: Option<String>,
    files: Vec<FileSpec>,
    modules: Vec<String>,
    since_rev: Option<String>,
    since_tag: Option<String>,
    users: Vec<String>,
    extract: Option<String>,
    timezone: Option<String>,
    warnings: Vec<String>,
}

/// How parsing stopped before any request was sent.
#[derive(Debug)]
enum Stop {
    /// A `getopt` complaint (if any) followed by the usage text; exit 1.
    Usage(Option<String>),
    /// A fatal parse error, worded as the reference words it; exit 1.
    Error(Error),
}

/// Apply one option that carries a value. `letter` is the option letter without its dash.
fn apply(opts: &mut Options, letter: char, value: String) -> Result<()> {
    match letter {
        'D' => {
            if opts.since_rev.is_some() || opts.since_tag.is_some() || opts.backto.is_some() {
                opts.warnings
                    .push("date overriding rev/tag/backto".to_string());
                opts.since_rev = None;
                opts.since_tag = None;
                opts.backto = None;
            }
            // `Make_Date` runs during getopt, before any connection; the value is stored in
            // the reference's internet form and sent as it is later (`client.c:5930-5938`).
            tag::check_date("history", &value)?;
            opts.since_date = Some(date::normalize(&value));
        }
        'b' => {
            if opts.since_date.is_some() || opts.since_rev.is_some() || opts.since_tag.is_some() {
                opts.warnings
                    .push("backto overriding date/rev/tag".to_string());
                opts.since_date = None;
                opts.since_rev = None;
                opts.since_tag = None;
            }
            // An empty `-b` clears the other filters but sends no `-b` pair (`history.c:582`).
            opts.backto = Some(value);
        }
        'r' => {
            if opts.since_date.is_some() || opts.since_tag.is_some() || opts.backto.is_some() {
                opts.warnings
                    .push("rev overriding date/tag/backto".to_string());
                opts.since_date = None;
                opts.since_tag = None;
                opts.backto = None;
            }
            opts.since_rev = Some(value);
        }
        't' => {
            if opts.since_date.is_some() || opts.since_rev.is_some() || opts.backto.is_some() {
                opts.warnings
                    .push("tag overriding date/marker/file/repos".to_string());
                opts.since_date = None;
                opts.since_rev = None;
                opts.backto = None;
            }
            // A tag name, not a date: the reference sends it verbatim.
            opts.since_tag = Some(value);
        }
        'f' => save_file(opts, FileSpec::File(value)),
        'p' => save_file(opts, FileSpec::Path(value)),
        'm' => {
            // The first `-m` is also the report type; the rest only add modules.
            if !opts.module_report {
                opts.reports += 1;
            }
            opts.module_report = true;
            opts.modules.push(value);
        }
        'n' => opts.modules.push(value),
        'u' => opts.users.push(value),
        'x' => {
            opts.reports += 1;
            if let Some(bad) = value.chars().find(|c| !RECORD_TYPES.contains(*c)) {
                return Err(Error::plain(
                    "history",
                    format!("{} is not a valid report type", bad),
                ));
            }
            opts.extract = Some(value);
        }
        'z' => {
            // The reference checks the zone before connecting; an unknown one is a warning
            // and the previous value (normally `+0000`) is kept (`history.c:505-536`).
            if timezone_is_known(&value) {
                opts.timezone = Some(value);
            } else {
                opts.warnings
                    .push(format!("{} is not a known time zone", value));
            }
        }
        // Undocumented `-X <file>`: accepted by getopt, used only in DEBUG builds.
        'X' => {}
        _ => unreachable!(),
    }
    Ok(())
}

/// `save_file` refuses an empty name with a warning instead of adding a filter.
fn save_file(opts: &mut Options, file: FileSpec) {
    let name = match &file {
        FileSpec::File(name) | FileSpec::Path(name) => name,
    };
    if name.is_empty() {
        opts.warnings
            .push("save_file: null dir and file name".to_string());
    } else {
        opts.files.push(file);
    }
}

/// Whether `getdate` would read the value as a time zone. The reference builds
/// `1/1/1970 23:00 <value>` and accepts the result; `lt` means the local zone. A bad zone is
/// reported locally and does not replace the previous one.
fn timezone_is_known(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    // No zone text at all parses, and the reference then keeps the empty name.
    if lower.is_empty() || lower == "lt" {
        return true;
    }
    let bytes = lower.as_bytes();
    // Signed offsets (`+0700`), bare numbers and lone signs all parse as (dataless) items,
    // which the reference accepts: the date and time still come from the fixed text.
    if lower
        .chars()
        .all(|c| c.is_ascii_digit() || c == '+' || c == '-' || c.is_whitespace())
    {
        return true;
    }
    if lower.len() == 1 {
        return lower != "j" && bytes[0].is_ascii_alphabetic();
    }
    if TIMEZONES.contains(&lower.as_str()) {
        return true;
    }
    // `<zone> dst` is the grammar's `tZONE tDST`.
    let mut words = lower.split_whitespace();
    matches!(
        (words.next(), words.next(), words.next()),
        (Some(zone), Some("dst"), None) if TIMEZONES.contains(&zone)
    )
}

/// Read the command line the way the reference's getopt does and return the options even
/// when parsing stops, so warnings printed before the stop are kept.
fn parse_args(args: &[String]) -> (Options, Option<Stop>) {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            opts.files
                .extend(args[i + 1..].iter().cloned().map(FileSpec::File));
            break;
        }
        // As with getopt's leading `+`, the first name ends option parsing: `history file -a`
        // sends `-f -a`, it does not mean all users.
        if !arg.starts_with('-') || arg == "-" {
            opts.files
                .extend(args[i..].iter().cloned().map(FileSpec::File));
            break;
        }

        let body: Vec<char> = arg[1..].chars().collect();
        let mut position = 0;
        while position < body.len() {
            let letter = body[position];
            if "DbfmnprtuxXz".contains(letter) {
                let attached: String = body[position + 1..].iter().collect();
                let value = if attached.is_empty() {
                    i += 1;
                    match args.get(i) {
                        Some(value) => value.clone(),
                        None => {
                            return (
                                opts,
                                Some(Stop::Usage(Some(format!(
                                    "cvs: option requires an argument -- {}",
                                    letter
                                )))),
                            )
                        }
                    }
                } else {
                    attached
                };
                if let Err(error) = apply(&mut opts, letter, value) {
                    return (opts, Some(Stop::Error(error)));
                }
                // A value consumes the rest of its cluster (`-xTOE`, `-D2020-01-01`).
                break;
            }
            match letter {
                'T' => {
                    opts.reports += 1;
                    opts.tag_report = true;
                }
                'a' => opts.all_users = true,
                'c' => {
                    opts.reports += 1;
                    opts.committed = true;
                }
                'e' => {
                    opts.reports += 1;
                    opts.everything = true;
                }
                'l' => opts.last_entry = true,
                'o' => {
                    opts.reports += 1;
                    opts.checked_out = true;
                }
                'w' => opts.working = true,
                '?' => return (opts, Some(Stop::Usage(None))),
                other => {
                    return (
                        opts,
                        Some(Stop::Usage(Some(format!(
                            "cvs: invalid option -- {}",
                            other
                        )))),
                    )
                }
            }
            position += 1;
        }
        i += 1;
    }
    // With no report type, the reference asks for checked-out modules.
    if opts.reports == 0 {
        opts.checked_out = true;
    }
    (opts, None)
}

/// The command's usage text, on stderr as the reference prints it. The trailing hint belongs
/// to `--help`, not to the usage a failed command shows.
fn usage(sink: &mut dyn Sink) {
    let Some(text) = cli::command_help("history") else {
        return;
    };
    for line in text.lines() {
        if line.starts_with("(Specify the --help global option") {
            continue;
        }
        sink.err(line.to_string());
    }
}

/// Run `cvs history`.
pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    let (opts, stop) = parse_args(args);
    // The reference prints these while getopt runs, before it counts reports or exits.
    for warning in &opts.warnings {
        sink.err(format!("cvs history: {}", warning));
    }
    match stop {
        Some(Stop::Usage(complaint)) => {
            if let Some(complaint) = complaint {
                sink.err(complaint);
            }
            usage(sink);
            return Ok(1);
        }
        Some(Stop::Error(error)) => return Err(error),
        None => {}
    }
    if opts.reports > 1 {
        return Err(Error::plain(
            "history",
            "Only one report type allowed from: \"-Tcomxe\".",
        ));
    }

    let mut session = protocol::open(root, "history", cancel, sink)?;
    if opts.tag_report {
        session.argument("-T")?;
    }
    if opts.all_users {
        session.argument("-a")?;
    }
    if opts.committed {
        session.argument("-c")?;
    }
    if opts.last_entry {
        session.argument("-l")?;
    }
    if opts.checked_out {
        session.argument("-o")?;
    }
    if opts.working {
        session.argument("-w")?;
    }
    if let Some(since) = &opts.since_date {
        session.argument("-D")?;
        session.argument(since)?;
    }
    if let Some(backto) = &opts.backto {
        // An empty `-b` is dropped instead of sending an empty Argument (`history.c:582`).
        if !backto.is_empty() {
            session.argument("-b")?;
            session.argument(backto)?;
        }
    }
    for file in &opts.files {
        match file {
            FileSpec::File(name) if name.starts_with('*') => {
                // A file spec whose name starts with `*` is a repository prefix: `-p` with
                // the star removed (`history.c:582-590`).
                session.argument("-p")?;
                session.argument(&name[1..])?;
            }
            FileSpec::File(name) => {
                session.argument("-f")?;
                session.argument(name)?;
            }
            FileSpec::Path(name) => {
                session.argument("-p")?;
                session.argument(name)?;
            }
        }
    }
    if opts.module_report {
        session.argument("-m")?;
    }
    for module in &opts.modules {
        session.argument("-n")?;
        session.argument(module)?;
    }
    if let Some(rev) = &opts.since_rev {
        session.argument("-r")?;
        session.argument(rev)?;
    }
    if let Some(tag) = &opts.since_tag {
        session.argument("-t")?;
        session.argument(tag)?;
    }
    for user in &opts.users {
        session.argument("-u")?;
        session.argument(user)?;
    }
    if opts.everything {
        session.argument("-e")?;
    }
    if let Some(types) = &opts.extract {
        session.argument("-x")?;
        session.argument(types)?;
    }
    session.argument("-z")?;
    session.argument(opts.timezone.as_deref().unwrap_or("+0000"))?;
    session.send("history")?;
    session.flush()?;
    // An `error` answer — including the empty one CVS 1.11.23 sends when its subcommand
    // failed — is a failure, so the exit code is 1 as in the reference.
    targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> (Options, Option<Stop>) {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        parse_args(&args)
    }

    #[test]
    fn short_flags_may_be_clustered() {
        let (opts, stop) = parse(&["-cl"]);
        assert!(stop.is_none());
        assert!(opts.committed && opts.last_entry);
        assert_eq!(opts.reports, 1);

        let (opts, _) = parse(&["-cw"]);
        assert!(opts.committed && opts.working);

        let (opts, _) = parse(&["-al"]);
        assert!(opts.all_users && opts.last_entry);

        let (opts, _) = parse(&["-Tcw"]);
        assert!(opts.tag_report && opts.committed && opts.working);
        assert_eq!(opts.reports, 2);
    }

    #[test]
    fn an_attached_value_ends_its_cluster() {
        let (opts, stop) = parse(&["-D2020-01-01"]);
        assert!(stop.is_none());
        assert_eq!(opts.since_date, Some(date::normalize("2020-01-01")));

        let (opts, _) = parse(&["-z-0700"]);
        assert_eq!(opts.timezone.as_deref(), Some("-0700"));

        let (opts, _) = parse(&["-xTOE"]);
        assert_eq!(opts.extract.as_deref(), Some("TOE"));
        assert_eq!(opts.reports, 1);
    }

    #[test]
    fn the_first_name_ends_option_parsing() {
        let (opts, stop) = parse(&["file", "-a"]);
        assert!(stop.is_none());
        assert!(!opts.all_users);
        assert_eq!(
            opts.files,
            vec![FileSpec::File("file".into()), FileSpec::File("-a".into())]
        );
    }

    #[test]
    fn getopt_errors_print_the_usage() {
        match parse(&["-Z"]).1 {
            Some(Stop::Usage(Some(text))) => assert!(text.contains("invalid option -- Z")),
            other => panic!("expected usage, got {:?}", other),
        }
        assert!(matches!(parse(&["-?"]).1, Some(Stop::Usage(None))));
        match parse(&["-D"]).1 {
            Some(Stop::Usage(Some(text))) => {
                assert!(text.contains("option requires an argument -- D"))
            }
            other => panic!("expected usage, got {:?}", other),
        }
    }

    #[test]
    fn an_unknown_date_is_refused_before_connecting() {
        match parse(&["-D", "garbage"]).1 {
            Some(Stop::Error(error)) => {
                assert!(error.text().contains("Can't parse date/time: garbage"))
            }
            other => panic!("expected an error, got {:?}", other),
        }
    }

    #[test]
    fn an_unknown_time_zone_warns_and_keeps_the_default() {
        let (opts, stop) = parse(&["-z", "XYZ"]);
        assert!(stop.is_none());
        assert_eq!(opts.timezone, None);
        assert!(opts
            .warnings
            .iter()
            .any(|warning| warning == "XYZ is not a known time zone"));
        // The offsets and `lt` the reference knows are accepted as typed.
        for zone in ["+0000", "-0700", "lt", "GMT", "EST"] {
            let (opts, _) = parse(&["-z", zone]);
            assert_eq!(opts.timezone.as_deref(), Some(zone), "{}", zone);
            assert!(opts.warnings.is_empty(), "{}", zone);
        }
    }

    #[test]
    fn an_empty_backto_is_kept_for_the_clearing_but_not_sent() {
        let (opts, _) = parse(&["-D", "2020-01-01", "-b", ""]);
        assert_eq!(opts.backto.as_deref(), Some(""));
        assert_eq!(opts.since_date, None);
        assert_eq!(opts.warnings, vec!["backto overriding date/rev/tag"]);
    }

    #[test]
    fn a_tag_stays_verbatim_and_the_debug_option_takes_a_value() {
        let (opts, _) = parse(&["-t", "2020-01-01"]);
        assert_eq!(opts.since_tag.as_deref(), Some("2020-01-01"));
        let (opts, stop) = parse(&["-Xdebug"]);
        assert!(stop.is_none());
        assert_eq!(opts.reports, 0);
    }
}
