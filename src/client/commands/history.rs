//! `cvs history`: the repository's record of checkouts, commits and tags.
//!
//! No working copy is needed: the client sends the report options and prints the lines the
//! server returns. The options go in the reference's fixed order — the report flags first,
//! then the date, the file and path filters (`-f`/`-p` in the order typed), the modules, the
//! revision, the tag, the users, the record types, and always the time zone (`+0000` unless
//! `-z` says otherwise). A report type may be given only once, and a later date, revision or
//! tag cancels the earlier filters with a warning, the way the reference does.

use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

/// Every record type `-e` and `-x` know about, in the reference's order.
const RECORD_TYPES: &str = "TOEFWUPCGMAR";

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
    warnings: Vec<&'static str>,
}

fn take(args: &[String], i: &mut usize, option: &str) -> Result<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| Error::plain("history", format!("option {} requires an argument", option)))
}

/// Apply one option that carries a value. `letter` is the option letter without its dash.
fn apply(opts: &mut Options, letter: char, value: String) -> Result<()> {
    match letter {
        'D' => {
            if opts.since_rev.is_some() || opts.since_tag.is_some() || opts.backto.is_some() {
                opts.warnings.push("date overriding rev/tag/backto");
                opts.since_rev = None;
                opts.since_tag = None;
                opts.backto = None;
            }
            opts.since_date = Some(value);
        }
        'b' => {
            if opts.since_date.is_some() || opts.since_rev.is_some() || opts.since_tag.is_some() {
                opts.warnings.push("backto overriding date/rev/tag");
                opts.since_date = None;
                opts.since_rev = None;
                opts.since_tag = None;
            }
            opts.backto = Some(value);
        }
        'r' => {
            if opts.since_date.is_some() || opts.since_tag.is_some() || opts.backto.is_some() {
                opts.warnings.push("rev overriding date/tag/backto");
                opts.since_date = None;
                opts.since_tag = None;
                opts.backto = None;
            }
            opts.since_rev = Some(value);
        }
        't' => {
            if opts.since_date.is_some() || opts.since_rev.is_some() || opts.backto.is_some() {
                opts.warnings.push("tag overriding date/marker/file/repos");
                opts.since_date = None;
                opts.since_rev = None;
                opts.backto = None;
            }
            opts.since_tag = Some(value);
        }
        'f' => opts.files.push(FileSpec::File(value)),
        'p' => opts.files.push(FileSpec::Path(value)),
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
        'z' => opts.timezone = Some(value),
        other => {
            return Err(Error::plain(
                "history",
                format!("invalid option -- '{}'", other),
            ))
        }
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // An attached value (`-D2020-01-01`) for any option that takes one.
        if arg.len() > 2 {
            if let Some(letter) = arg.strip_prefix('-').and_then(|rest| {
                rest.starts_with(|c: char| "Dbfmnprtuxz".contains(c))
                    .then(|| rest.chars().next().unwrap())
            }) {
                apply(&mut opts, letter, arg[2..].to_string())?;
                i += 1;
                continue;
            }
        }
        match arg.as_str() {
            "--" => {
                for name in &args[i + 1..] {
                    opts.files.push(FileSpec::File(name.clone()));
                }
                break;
            }
            "-T" => {
                opts.reports += 1;
                opts.tag_report = true;
            }
            "-a" => opts.all_users = true,
            "-c" => {
                opts.reports += 1;
                opts.committed = true;
            }
            "-e" => {
                opts.reports += 1;
                opts.everything = true;
            }
            "-l" => opts.last_entry = true,
            "-o" => {
                opts.reports += 1;
                opts.checked_out = true;
            }
            "-w" => opts.working = true,
            "-D" | "-b" | "-f" | "-m" | "-n" | "-p" | "-r" | "-t" | "-u" | "-x" | "-z" => {
                let value = take(args, &mut i, arg)?;
                apply(&mut opts, arg.chars().nth(1).unwrap(), value)?;
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "history",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.files.push(FileSpec::File(other.to_string())),
        }
        i += 1;
    }
    // With no report type, the reference asks for checked-out modules.
    if opts.reports == 0 {
        opts.checked_out = true;
    }
    Ok(opts)
}

/// Run `cvs history`.
pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    let opts = parse_args(args)?;
    if opts.reports > 1 {
        return Err(Error::plain(
            "history",
            "Only one report type allowed from: \"-Tcomxe\".",
        ));
    }
    for warning in &opts.warnings {
        sink.err(format!("cvs history: {}", warning));
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
        session.argument(&date::normalize(since))?;
    }
    if let Some(backto) = &opts.backto {
        session.argument("-b")?;
        session.argument(backto)?;
    }
    for file in &opts.files {
        match file {
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
    targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}
