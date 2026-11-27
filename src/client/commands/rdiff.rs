//! `cvs rdiff`: a patch between two revisions of a module, with no working copy needed.
//!
//! The reference names every value separately (`-r` then `1.1`), and sends the flags in a
//! fixed order: `-l`, `-f`, `-t`, `-s`, `-u`, then the first revision or date, the second,
//! the keyword option, and the modules. Two revisions or dates at most; `-t` (the last two
//! diffs) cannot be combined with either or with `-s`.
//!
//! `-D` is parsed with [`date::normalize`] before connecting, as the reference's `Make_Date`
//! does (`src/main.c:1029-1037`, `src/patch.c:112-120`), and two dates must run forward
//! (`src/patch.c:190-192`). A diff that found differences still exits 0; only a server
//! `error` makes the command fail (`src/patch.c:234-235`).

use chrono::DateTime;

use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, Response};
use crate::client::root::Root;

const USAGE: &[&str] = &[
    "Usage: cvs rdiff [-flR] [-c|-u] [-s|-t] [-V %d] [-k kopt]",
    "    -r rev|-D date [-r rev2 | -D date2] modules...",
    "\t-f\tForce a head revision match if tag/date not found.",
    "\t-l\tLocal directory only, not recursive",
    "\t-R\tProcess directories recursively.",
    "\t-c\tContext diffs (default)",
    "\t-u\tUnidiff format.",
    "\t-s\tShort patch - one liner per file.",
    "\t-t\tTop two diffs - last change made to the file.",
    "\t-V vers\tUse RCS Version \"vers\" for keyword expansion.",
    "\t-k kopt\tSpecify keyword expansion mode.",
    "\t-D date\tDate.",
    "\t-r rev\tRevision - symbolic or numeric.",
    "(Specify the --help global option for a list of other help options)",
];

const KFLAG_MODES: [&str; 6] = ["kv", "kvl", "k", "v", "o", "b"];

#[derive(Debug, Default)]
struct Options {
    local: bool,
    force_tag_match: bool,
    top_two: bool,
    short: bool,
    unidiff: bool,
    rev1: Option<String>,
    date1: Option<String>,
    rev2: Option<String>,
    date2: Option<String>,
    /// Normalized `-k<mode>`, sent as one argument (`src/patch.c:130-134`).
    kflag: Option<String>,
    modules: Vec<String>,
}

impl Options {
    /// `-r` and `-D` fill the first free slot, then the second; a third is an error. The
    /// count is checked before the value is used, as the reference does.
    fn check_slot(&self) -> Result<()> {
        if self.rev2.is_some() || self.date2.is_some() {
            return Err(Error::aborted(
                "rdiff",
                "no more than two revisions/dates can be specified",
            ));
        }
        Ok(())
    }

    fn revision(&mut self, value: String) -> Result<()> {
        self.check_slot()?;
        if self.rev1.is_some() || self.date1.is_some() {
            self.rev2 = Some(value);
        } else {
            self.rev1 = Some(value);
        }
        Ok(())
    }

    fn dated(&mut self, value: String) -> Result<()> {
        self.check_slot()?;
        let value = normalize_date(&value)?;
        if self.rev1.is_some() || self.date1.is_some() {
            self.date2 = Some(value);
        } else {
            self.date1 = Some(value);
        }
        Ok(())
    }

    /// `RCS_check_kflag` accepts exactly these modes and sends `-k<mode>` as one argument
    /// (`src/rcs.c:3350-3386`).
    fn set_kflag(&mut self, value: String) -> Result<()> {
        if !KFLAG_MODES.contains(&value.as_str()) {
            return Err(Error::plain("rdiff", "invalid RCS keyword expansion mode"));
        }
        self.kflag = Some(format!("-k{}", value));
        Ok(())
    }
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            opts.modules.extend(args[i + 1..].iter().cloned());
            break;
        }
        // getopt runs with a leading `+`: the first module name ends the options.
        if !arg.starts_with('-') || arg == "-" {
            opts.modules.extend(args[i..].iter().cloned());
            break;
        }
        parse_short(&mut opts, arg, args, &mut i)?;
        i += 1;
    }
    Ok(opts)
}

/// Split a short-option cluster the way getopt does. A letter that takes a value consumes
/// the rest of the word, or the next argument when the rest is empty (`-r1.1`, `-r 1.1`).
fn parse_short(opts: &mut Options, arg: &str, args: &[String], i: &mut usize) -> Result<()> {
    let text = &arg[1..];
    for (offset, letter) in text.char_indices() {
        match letter {
            'f' => opts.force_tag_match = true,
            'l' => opts.local = true,
            'R' => opts.local = false,
            't' => opts.top_two = true,
            's' => opts.short = true,
            'u' => opts.unidiff = true,
            'c' => opts.unidiff = false,
            'q' | 'Q' => {
                return Err(Error::aborted(
                    "rdiff",
                    "-q or -Q must be specified before \"rdiff\"",
                ))
            }
            // The reference accepts the value and then refuses the option outright
            // (`src/patch.c:135-152`).
            'V' => {
                return Err(Error::aborted(
                    "rdiff",
                    "the -V option is obsolete and should not be used",
                ))
            }
            'r' | 'D' | 'k' => {
                let attached = &text[offset + letter.len_utf8()..];
                let value = if attached.is_empty() {
                    take_value(args, i, letter)?
                } else {
                    attached.to_string()
                };
                match letter {
                    'r' => opts.revision(value)?,
                    'D' => opts.dated(value)?,
                    _ => opts.set_kflag(value)?,
                }
                return Ok(());
            }
            other => {
                return Err(Error::plain(
                    "rdiff",
                    format!("invalid option -- '{}'", other),
                ))
            }
        }
    }
    Ok(())
}

fn take_value(args: &[String], i: &mut usize, letter: char) -> Result<String> {
    *i += 1;
    args.get(*i).cloned().ok_or_else(|| {
        Error::plain(
            "rdiff",
            format!("option requires an argument -- '{}'", letter),
        )
    })
}

/// `Make_Date` reads one date and refuses text `get_date` cannot parse. Text this client
/// understands becomes the reference's UTC form; a phrase only the server's `getdate` reads
/// is left for it, while text with nothing date-like in it is refused here.
fn normalize_date(text: &str) -> Result<String> {
    let trimmed = text.trim();
    // A range (`D1<D2`) belongs to `log -d`; `Make_Date` takes a single date.
    if trimmed.is_empty() || trimmed.contains(['<', '>']) {
        return Err(unparseable_date(text));
    }
    let normalized = date::normalize(trimmed);
    if date_stamp(&normalized).is_some() || date_like(trimmed) {
        Ok(normalized)
    } else {
        Err(unparseable_date(text))
    }
}

fn unparseable_date(text: &str) -> Error {
    Error::aborted("rdiff", format!("Can't parse date/time: {}", text))
}

fn date_stamp(text: &str) -> Option<i64> {
    DateTime::parse_from_str(text, "%d %b %Y %H:%M:%S %z")
        .ok()
        .map(|time| time.timestamp())
}

/// Whether text at least looks like something the server's date parser might accept, the
/// same tolerance `tag -D` uses.
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

/// Two dates must run forward, as `RCS_datecmp` requires (`src/patch.c:190-192`). A phrase
/// only the server can read has no local stamp and is left for the server to judge.
fn check_date_order(opts: &Options) -> Result<()> {
    if let (Some(first), Some(second)) = (&opts.date1, &opts.date2) {
        if let (Some(first), Some(second)) = (date_stamp(first), date_stamp(second)) {
            if first >= second {
                return Err(Error::aborted(
                    "rdiff",
                    "second date must come after first date!",
                ));
            }
        }
    }
    Ok(())
}

fn usage(sink: &mut dyn Sink) {
    for line in USAGE {
        sink.err((*line).to_string());
    }
}

/// Run `cvs rdiff`.
pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    let opts = parse_args(args)?;
    if opts.modules.is_empty() {
        usage(sink);
        return Ok(1);
    }
    if opts.top_two && opts.short {
        return Err(Error::aborted(
            "rdiff",
            "-t and -s options are mutually exclusive",
        ));
    }
    let any_revision =
        opts.rev1.is_some() || opts.date1.is_some() || opts.rev2.is_some() || opts.date2.is_some();
    if opts.top_two && any_revision {
        return Err(Error::aborted(
            "rdiff",
            "must not specify revisions/dates with -t option!",
        ));
    }
    if !opts.top_two && !any_revision {
        return Err(Error::aborted(
            "rdiff",
            "must specify at least one revision/date!",
        ));
    }
    check_date_order(&opts)?;

    let mut session = protocol::open(root, "rdiff", cancel, sink)?;
    send_options(&mut session, &opts)?;
    // `Argument --` and `Max-dotdot` come after the options and before the module names,
    // so a module starting with `-` is not read as an option by the server.
    targets::begin_files(&mut session, &opts.modules)?;
    targets::finish_arguments(&mut session, &opts.modules, "rdiff")?;
    let code = read_patch(&mut session)?;
    session.close();
    Ok(code)
}

/// The reference's fixed order (`src/patch.c:206-226`). Dates were normalized when parsed.
fn send_options(session: &mut protocol::Session, opts: &Options) -> Result<()> {
    if opts.local {
        session.argument("-l")?;
    }
    if opts.force_tag_match {
        session.argument("-f")?;
    }
    if opts.top_two {
        session.argument("-t")?;
    }
    if opts.short {
        session.argument("-s")?;
    }
    if opts.unidiff {
        session.argument("-u")?;
    }
    if let Some(rev) = &opts.rev1 {
        session.argument("-r")?;
        session.argument(rev)?;
    }
    if let Some(date) = &opts.date1 {
        session.argument("-D")?;
        session.argument(date)?;
    }
    if let Some(rev) = &opts.rev2 {
        session.argument("-r")?;
        session.argument(rev)?;
    }
    if let Some(date) = &opts.date2 {
        session.argument("-D")?;
        session.argument(date)?;
    }
    if let Some(kflag) = &opts.kflag {
        session.argument(kflag)?;
    }
    Ok(())
}

/// Read the diff text. `ok` means no differences (0). CVSNT ends a diff that found
/// differences with an empty `error` instead of `ok`; that ending is success only when it
/// follows printed patch text, as an empty `error` with no output is a real server failure.
/// Any other `error` fails the command (`src/client.c:3493-3494`).
fn read_patch(session: &mut protocol::Session) -> Result<i32> {
    let mut printed = false;
    loop {
        match session.read_response()? {
            Response::Ok => return Ok(0),
            Response::E(text) => session.err(text),
            Response::Notified | Response::Trace(_) | Response::Checksum(_) => {}
            Response::M(text) => {
                printed = true;
                session.out(text);
            }
            Response::Error { message, .. } if message.trim().is_empty() => {
                return Ok(if printed { 0 } else { 1 });
            }
            Response::Error { message, .. } => {
                return Err(Error::aborted(&session.command, message))
            }
            other => {
                return Err(Error::aborted(
                    &session.command,
                    format!("unexpected answer: {:?}", other),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Options {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        parse_args(&args).unwrap()
    }

    fn parse_err(args: &[&str]) -> Error {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        parse_args(&args).unwrap_err()
    }

    #[test]
    fn revisions_and_dates_share_two_slots() {
        let opts = parse(&["-r", "1.1", "-D", "2020-01-01", "mod"]);
        assert_eq!(opts.rev1.as_deref(), Some("1.1"));
        assert_eq!(opts.date1, None);
        assert!(opts.date2.is_some());

        let opts = parse(&["-D", "2020-01-01", "-r", "1.2", "mod"]);
        assert!(opts.date1.is_some());
        assert_eq!(opts.rev2.as_deref(), Some("1.2"));

        let error = parse_err(&["-r1.1", "-r1.2", "-D2020-01-01", "mod"]);
        assert!(
            error
                .text()
                .contains("no more than two revisions/dates can be specified"),
            "{}",
            error
        );
    }

    #[test]
    fn short_options_cluster_like_getopt() {
        let opts = parse(&["-fl", "-us", "-r1.1", "mod"]);
        assert!(opts.force_tag_match);
        assert!(opts.local);
        assert!(opts.unidiff);
        assert!(opts.short);
        assert!(!opts.top_two);

        let opts = parse(&["-Rt"]);
        assert!(!opts.local);
        assert!(opts.top_two);

        // The last of `-u`/`-c` and `-l`/`-R` wins, as getopt applies them in order.
        assert!(!parse(&["-u", "-c"]).unidiff);
        assert!(parse(&["-R", "-l"]).local);
    }

    #[test]
    fn an_option_value_may_be_attached_or_separate() {
        let opts = parse(&["-r1.1", "-D2020-01-01", "-kkv", "mod"]);
        assert_eq!(opts.rev1.as_deref(), Some("1.1"));
        assert!(opts.date2.is_some());
        assert_eq!(opts.kflag.as_deref(), Some("-kkv"));

        let opts = parse(&["-r", "1.1", "-D", "2020-01-01", "-k", "kvl", "mod"]);
        assert_eq!(opts.rev1.as_deref(), Some("1.1"));
        assert!(opts.date2.is_some());
        assert_eq!(opts.kflag.as_deref(), Some("-kkvl"));
    }

    #[test]
    fn options_stop_at_the_first_module() {
        let opts = parse(&["-r1.1", "mod", "-s", "--", "other"]);
        assert!(!opts.short);
        assert_eq!(opts.modules, ["mod", "-s", "--", "other"]);
    }

    #[test]
    fn a_double_dash_ends_the_options_and_a_lone_dash_is_a_module() {
        let opts = parse(&["-r1.1", "--", "-mod", "-"]);
        assert!(opts.rev1.is_some());
        assert_eq!(opts.modules, ["-mod", "-"]);
    }

    #[test]
    fn q_and_q_after_the_command_are_refused() {
        let error = parse_err(&["-r1.1", "-q", "mod"]);
        assert!(
            error
                .text()
                .contains("-q or -Q must be specified before \"rdiff\""),
            "{}",
            error
        );
        let error = parse_err(&["-Qr1.1", "mod"]);
        assert!(
            error.text().contains("must be specified before"),
            "{}",
            error
        );
    }

    #[test]
    fn v_is_obsolete_even_when_attached() {
        let attached: &[&str] = &["-V5", "-r1.1", "mod"];
        let separate: &[&str] = &["-V", "5", "-r1.1", "mod"];
        for args in [attached, separate] {
            let error = parse_err(args);
            assert!(error.text().contains("obsolete"), "{}", error);
        }
    }

    #[test]
    fn kflag_is_validated_and_normalized() {
        assert_eq!(parse(&["-kb", "mod"]).kflag.as_deref(), Some("-kb"));
        let separate: &[&str] = &["-k", "bogus", "mod"];
        let attached: &[&str] = &["-kkx", "mod"];
        let ambiguous: &[&str] = &["-k", "-kb", "mod"];
        for args in [separate, attached, ambiguous] {
            let error = parse_err(args);
            assert!(
                error.text().contains("invalid RCS keyword expansion mode"),
                "{}",
                error
            );
        }
    }

    #[test]
    fn a_date_is_parsed_before_connecting() {
        for date in ["garbage", "", "2020-01-01<2020-02-01"] {
            let error = parse_err(&["-D", date, "mod"]);
            assert!(error.text().contains("Can't parse date/time:"), "{}", error);
        }

        let opts = parse(&["-D", "2020-01-01", "mod"]);
        let date = opts.date1.unwrap();
        assert!(date.ends_with(" -0000"), "{}", date);
        assert!(!date.contains('<'), "{}", date);
    }

    #[test]
    fn the_second_date_must_come_after_the_first() {
        let opts = parse(&["-D", "2020-01-02", "-D", "2020-01-01", "mod"]);
        let error = check_date_order(&opts).unwrap_err();
        assert!(
            error
                .text()
                .contains("second date must come after first date!"),
            "{}",
            error
        );

        let opts = parse(&["-D", "2020-01-01", "-D", "2020-01-01", "mod"]);
        assert!(check_date_order(&opts).is_err());

        let opts = parse(&["-D", "2020-01-01", "-D", "2020-01-02", "mod"]);
        assert!(check_date_order(&opts).is_ok());
    }

    #[test]
    fn a_missing_value_is_reported_as_getopt_does() {
        for option in ['r', 'D', 'k'] {
            let error = parse_err(&[&format!("-{}", option)]);
            assert!(
                error
                    .text()
                    .contains(&format!("option requires an argument -- '{}'", option)),
                "{}",
                error
            );
        }
    }

    #[test]
    fn an_unknown_option_names_the_letter() {
        let error = parse_err(&["-fZ", "mod"]);
        assert!(error.text().contains("invalid option -- 'Z'"), "{}", error);
    }
}
