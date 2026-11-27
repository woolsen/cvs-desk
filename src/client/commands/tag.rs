//! `cvs tag`: put a symbolic tag on the revisions checked out in a working copy.
//!
//! The flags, the `--` that ends them, and the tag name are `Argument`s before the files; the
//! files themselves are sent as the same `Directory`/`Entry`/state blocks `status` and `log`
//! send. The server does the work and answers `M`/`E` lines, which this client prints as they
//! arrive.
//!
//! `-c` asks the server to verify that every file it tags is unmodified. The client sends the
//! contents of the files that look changed, exactly as the reference does, so the server can
//! compare them with the repository revision instead of trusting the recorded timestamp; a
//! file whose only change is its mtime is re-registered (`Checked-in`) and tagged.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::cli;
use crate::client::commands::date;
use crate::client::commands::targets::{self, Target};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, Response};
use crate::client::root::Root;
use crate::client::work::entries::Entry;
use crate::client::work::glob;

#[derive(Debug, Default)]
struct Options {
    branch: bool,
    /// `-B`: let `-F`/`-d` move or delete a branch tag.
    disturb_branch: bool,
    check_modified: bool,
    delete: bool,
    move_tag: bool,
    force: bool,
    recursive: bool,
    revision: Option<String>,
    date: Option<String>,
    tag: String,
    names: Vec<String>,
}

/// `tag` stops taking options at the tag name, as getopt does with `+`: everything after it is
/// a file name, even when it starts with `-`. Clusters are split the way getopt splits them,
/// including a value attached to `-r`/`-D`.
fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options {
        recursive: true,
        ..Default::default()
    };
    let mut i = 0;
    let mut operands: &[String] = &[];
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            operands = &args[i + 1..];
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            operands = &args[i..];
            break;
        }
        let body = &arg[1..];
        let mut offset = 0;
        while offset < body.len() {
            let option = body[offset..]
                .chars()
                .next()
                .expect("offset is a char boundary");
            let next = offset + option.len_utf8();
            match option {
                'b' => opts.branch = true,
                'B' => opts.disturb_branch = true,
                'c' => opts.check_modified = true,
                'd' => opts.delete = true,
                'F' => opts.move_tag = true,
                'f' => opts.force = true,
                'l' => opts.recursive = false,
                'R' => opts.recursive = true,
                'q' | 'Q' => {
                    // The reference sends these from its own newer client, so the command
                    // refuses them when they are not a global option.
                    return Err(Error::aborted(
                        "tag",
                        "-q or -Q must be specified before \"tag\"",
                    ));
                }
                'r' | 'D' => {
                    let attached = &body[next..];
                    let value = if attached.is_empty() {
                        i += 1;
                        args.get(i).cloned().ok_or_else(|| {
                            Error::plain(
                                "tag",
                                format!("option requires an argument -- {}", option),
                            )
                        })?
                    } else {
                        attached.to_string()
                    };
                    if option == 'r' {
                        opts.revision = Some(value);
                    } else {
                        check_date("tag", &value)?;
                        opts.date = Some(value);
                    }
                    offset = body.len();
                    continue;
                }
                other => return Err(Error::plain("tag", format!("invalid option -- {}", other))),
            }
            offset = next;
        }
        i += 1;
    }
    if let Some((tag, names)) = operands.split_first() {
        opts.tag = tag.clone();
        opts.names = names.to_vec();
    }
    Ok(opts)
}

/// Run `cvs tag`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = match parse_args(args) {
        Ok(opts) => opts,
        Err(error) => {
            sink.err(error.text().to_string());
            // getopt reports the bad option and then the command's usage; a `-D` the client
            // cannot parse is a plain fatal error with no usage.
            let text = error.text();
            if text.contains("invalid option --") || text.contains("option requires an argument") {
                print_usage(sink);
            }
            return Ok(1);
        }
    };
    if opts.tag.is_empty() {
        // The reference prints the command's usage and exits 1, without a prefix.
        print_usage(sink);
        return Ok(1);
    }
    if opts.revision.is_some() && opts.date.is_some() {
        return Err(Error::aborted(
            "tag",
            "-r and -D options are mutually exclusive",
        ));
    }
    if opts.branch && opts.delete {
        sink.err("cvs tag: warning: -b ignored with -d options".to_string());
    }
    check_tag_name("tag", &opts.tag)?;
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "tag",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    // The names on the wire are the ones the shell would have expanded, as `SEND_EXPAND_WILD`
    // does. Progress reporting names the invocation folder as `.`, not `./`.
    let names = glob::expand_args(cwd, &opts.names);
    let collect_names: Vec<String> = names.iter().map(|name| collect_name(name)).collect();
    let mut targets = targets::collect(cwd, opts.recursive, &collect_names)?;

    let mut session = protocol::open(root, "tag", cancel, sink)?;
    send_arguments(&mut session, &opts, &names)?;
    let last = targets::send_blocks_quiet(&mut session, &targets)?;
    targets::finish_from_last(&mut session, cwd, &names, "tag", last.as_deref())?;

    // `tag -c` can end a file's answer with `Checked-in`: the server found the contents equal
    // to the repository revision and re-registered the new time. Apply that to `Entries`, as
    // the reference client does, unless `-n` was given.
    let mut checked_in = false;
    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::CheckedIn {
                update_dir, entry, ..
            }
            | Response::NewEntry {
                update_dir, entry, ..
            } => {
                checked_in |= record_checked_in(&mut targets, &update_dir, &entry)?;
            }
            Response::Error { message, .. } => {
                session.close();
                if message.trim().is_empty() {
                    // The server ends a failed command this way; the reference client simply
                    // exits 1 (client.c:3493-3494).
                    return Ok(1);
                }
                return Err(Error::aborted("tag", message));
            }
            _ => {}
        }
    }
    if checked_in && !session.no_execute() {
        for target in targets.values() {
            target.dir.write_entries(&target.entries)?;
        }
    }
    session.close();
    Ok(0)
}

fn print_usage(sink: &mut dyn Sink) {
    if let Some(text) = cli::command_help("tag") {
        sink.err(text.to_string());
    }
}

/// The flags and the tag name, in the order the reference writes them, then the `Max-dotdot`
/// that has to precede any folder block a `..` name reaches. `-R` is the default and is never
/// sent; `-l` is sent when recursion was turned off.
fn send_arguments(session: &mut protocol::Session, opts: &Options, names: &[String]) -> Result<()> {
    if opts.branch {
        session.argument("-b")?;
    }
    if opts.disturb_branch {
        session.argument("-B")?;
    }
    if opts.check_modified {
        session.argument("-c")?;
    }
    if opts.delete {
        session.argument("-d")?;
    }
    if opts.move_tag {
        session.argument("-F")?;
    }
    if opts.force {
        session.argument("-f")?;
    }
    if !opts.recursive {
        session.argument("-l")?;
    }
    if let Some(revision) = &opts.revision {
        session.argument("-r")?;
        session.argument(revision)?;
    }
    if let Some(date) = &opts.date {
        session.argument("-D")?;
        session.argument(&date::normalize(date))?;
    }
    // The server must not read the tag name as one of its own options.
    session.argument("--")?;
    session.argument(&opts.tag)?;

    let levels = max_dotdot(names);
    if levels > 0 {
        if session.supports("Max-dotdot") {
            session.send(&format!("Max-dotdot {}", levels))?;
        } else {
            return Err(Error::aborted(
                "tag",
                "backreference in path (`..') not supported by old (pre-Max-dotdot) servers",
            ));
        }
    }
    Ok(())
}

/// The rule RCS applies: a letter first, visible characters only, and none of `$,.:;@`
/// (rcs.c:3393-3418). Both `tag` and `rtag` refuse an invalid name before connecting.
pub(crate) fn check_tag_name(command: &str, tag: &str) -> Result<()> {
    let mut chars = tag.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => {
            return Err(Error::aborted(
                command,
                format!("tag `{}' must start with a letter", tag),
            ))
        }
    }
    for c in chars {
        if !c.is_ascii_graphic() {
            return Err(Error::aborted(
                command,
                format!("tag `{}' has non-visible graphic characters", tag),
            ));
        }
        if "$,.:;@".contains(c) {
            return Err(Error::aborted(
                command,
                format!("tag `{}' must not contain the characters `$,.:;@'", tag),
            ));
        }
    }
    Ok(())
}

/// `-D` is a single date, and the reference parses it with `Make_Date` before connecting
/// (`main.c:1029-1037`). Text this client cannot parse is refused with the reference's words;
/// relative phrases the server's getdate understands are left for it to parse.
pub(crate) fn check_date(command: &str, text: &str) -> Result<()> {
    if date_is_parseable(text) {
        Ok(())
    } else {
        Err(Error::aborted(
            command,
            format!("Can't parse date/time: {}", text),
        ))
    }
}

fn date_is_parseable(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    // A range (`D1<D2`) is for `log -d`; `Make_Date` does not read one.
    if trimmed.contains(['<', '>']) {
        return false;
    }
    // `date::normalize` turns anything it understands into the canonical UTC text; a value it
    // leaves alone is only accepted when it already is that canonical date.
    let normalized = date::normalize(trimmed);
    if chrono::DateTime::parse_from_str(&normalized, "%d %b %Y %H:%M:%S %z").is_ok() {
        return true;
    }
    // getdate understands wider shapes than this client's date module (month names, relative
    // phrases, other separators). Leave those to the server instead of refusing a date it
    // would accept; only text with nothing date-like in it is rejected here.
    let lower = trimmed.to_ascii_lowercase();
    let words = [
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
    let has_word = lower
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|part| words.contains(&part));
    has_word
        || (trimmed.chars().any(|c| c.is_ascii_digit())
            && trimmed.chars().any(|c| "-/.:,".contains(c)))
}

/// Apply the entry a `Checked-in`/`New-entry` response carries. An empty date becomes
/// `dummy timestamp`, as in `remove`, and only a folder the request named is touched.
fn record_checked_in(
    targets: &mut BTreeMap<String, Target>,
    update_dir: &str,
    entry_line: &str,
) -> Result<bool> {
    let mut entry = Entry::parse(entry_line).ok_or_else(|| {
        Error::aborted(
            "tag",
            format!(
                "the server sent an entry this client cannot read: {}",
                entry_line
            ),
        )
    })?;
    if entry.date.is_empty() {
        entry.date = "dummy timestamp".to_string();
    }
    let Some(target) = targets.get_mut(&targets::target_key(update_dir)) else {
        return Ok(false);
    };
    target.entries.set_file(entry);
    Ok(true)
}

/// The name `targets::collect` should look up: `.` and `./` mean the whole folder, and a
/// trailing slash or `./` prefix does not change where a name points. The name as typed is
/// what goes on the wire.
fn collect_name(name: &str) -> String {
    let name = name.replace('\\', "/");
    let name = name.trim_end_matches('/');
    if name.is_empty() || name == "." {
        return String::new();
    }
    name.strip_prefix("./").unwrap_or(name).to_string()
}

/// How many `..` components a path has, the way `pathname_levels` counts them.
fn max_dotdot(names: &[String]) -> usize {
    let mut max = 0;
    for name in names {
        let mut level: isize = 0;
        let mut deepest: isize = 0;
        for part in name.split(['/', '\\']) {
            match part {
                ".." => {
                    level += 1;
                    deepest = deepest.max(level);
                }
                "." | "" => {}
                _ => level -= 1,
            }
        }
        max = max.max(deepest.max(0) as usize);
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn flags_are_read_until_the_tag_name() {
        let opts = parse(&[
            "-b", "-B", "-c", "-d", "-F", "-f", "-l", "-R", "T1", "a.txt",
        ])
        .unwrap();
        assert!(opts.branch);
        assert!(opts.disturb_branch);
        assert!(opts.check_modified);
        assert!(opts.delete);
        assert!(opts.move_tag);
        assert!(opts.force);
        assert!(opts.recursive, "-R after -l turns recursion back on");
        assert_eq!(opts.tag, "T1");
        assert_eq!(opts.names, vec!["a.txt"]);
    }

    #[test]
    fn clustered_flags_are_split_like_getopt() {
        let opts = parse(&["-bd", "T1"]).unwrap();
        assert!(opts.branch);
        assert!(opts.delete);

        let opts = parse(&["-Bf", "T1"]).unwrap();
        assert!(opts.disturb_branch);
        assert!(opts.force);

        let opts = parse(&["-lR", "T1"]).unwrap();
        assert!(opts.recursive);

        let opts = parse(&["-br1.1", "T1"]).unwrap();
        assert!(opts.branch);
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
    }

    #[test]
    fn a_dash_word_after_the_tag_is_a_file() {
        let opts = parse(&["T1", "-b"]).unwrap();
        assert!(!opts.branch);
        assert_eq!(opts.tag, "T1");
        assert_eq!(opts.names, vec!["-b"]);
    }

    #[test]
    fn a_revision_and_a_date_take_a_separate_or_attached_value() {
        let opts = parse(&["-r", "1.1", "T1"]).unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
        assert_eq!(opts.tag, "T1");

        let opts = parse(&["-r1.1", "-D2020-01-01", "T1"]).unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
        assert_eq!(opts.date.as_deref(), Some("2020-01-01"));
    }

    #[test]
    fn a_missing_value_is_refused() {
        for option in ["-r", "-D", "-br"] {
            let error = parse(&[option]).unwrap_err();
            assert!(
                error.text().contains("option requires an argument -- r")
                    || error.text().contains("option requires an argument -- D"),
                "{}",
                error
            );
        }
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let error = parse(&["-Z", "T1"]).unwrap_err();
        assert!(error.text().contains("invalid option -- Z"), "{}", error);
    }

    #[test]
    fn q_and_q_after_the_command_are_refused() {
        for option in ["-q", "-Q"] {
            let error = parse(&[option, "T1"]).unwrap_err();
            assert!(
                error
                    .text()
                    .contains("-q or -Q must be specified before \"tag\""),
                "{}",
                error
            );
        }
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = parse(&["-b", "--", "-T1"]).unwrap();
        assert!(opts.branch);
        assert_eq!(opts.tag, "-T1");
        assert!(opts.names.is_empty());
    }

    #[test]
    fn no_tag_name_leaves_the_tag_empty() {
        let opts = parse(&["-b"]).unwrap();
        assert!(opts.tag.is_empty());
    }

    #[test]
    fn a_tag_name_must_follow_the_rcs_rule() {
        for good in ["T1", "release_1", "A", "v1_0"] {
            check_tag_name("tag", good).unwrap();
        }
        for (bad, wanted) in [
            ("1.2", "must start with a letter"),
            ("", "must start with a letter"),
            ("-bad", "must start with a letter"),
            ("TAG.1", "must not contain"),
            ("A,B", "must not contain"),
            ("A B", "non-visible"),
        ] {
            let error = check_tag_name("tag", bad).unwrap_err();
            assert!(error.text().contains(wanted), "{}: {}", bad, error);
        }
    }

    #[test]
    fn a_date_is_checked_before_connecting() {
        for good in [
            "2020-01-01",
            "2020/01/01",
            "31 Dec 2019 16:00:00 -0000",
            "today",
            "last tuesday",
        ] {
            check_date("tag", good).unwrap();
        }
        for bad in ["garbage", "", "2020-01-01<2020-02-01"] {
            let error = check_date("tag", bad).unwrap_err();
            assert!(
                error
                    .text()
                    .contains(&format!("Can't parse date/time: {}", bad)),
                "{}",
                error
            );
        }
    }

    #[test]
    fn a_folder_argument_is_looked_up_as_the_folder_itself() {
        assert_eq!(collect_name("."), "");
        assert_eq!(collect_name("./"), "");
        assert_eq!(collect_name("./sub/"), "sub");
        assert_eq!(collect_name("sub\\file.txt"), "sub/file.txt");
        assert_eq!(collect_name("../file.txt"), "../file.txt");
    }

    #[test]
    fn dotdot_levels_are_counted_for_max_dotdot() {
        assert_eq!(max_dotdot(&["a.txt".to_string()]), 0);
        assert_eq!(max_dotdot(&["sub/a.txt".to_string()]), 0);
        assert_eq!(max_dotdot(&["../a.txt".to_string()]), 1);
        assert_eq!(max_dotdot(&["../../a.txt".to_string()]), 2);
        assert_eq!(max_dotdot(&["sub/../a.txt".to_string()]), 0);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-tag-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn target(dir: &std::path::Path) -> Target {
        Target {
            dir: crate::client::work::dir::WorkingDir::new(
                dir.to_path_buf(),
                String::new(),
                "mod".to_string(),
                crate::client::work::codec::for_dir(dir),
            ),
            entries: crate::client::work::entries::Entries::default(),
            filter: Vec::new(),
            selected: true,
            loose: Vec::new(),
        }
    }

    #[test]
    fn a_checked_in_entry_replaces_the_recorded_time() {
        let dir = scratch("checked-in");
        let mut target = target(&dir);
        target.entries.set_file(Entry::new(
            "hello.txt",
            "1.1",
            "Sat Jan  1 00:00:00 2000",
            "",
            "",
        ));
        let mut targets = BTreeMap::new();
        targets.insert(String::new(), target);

        let changed = record_checked_in(
            &mut targets,
            "./",
            "/hello.txt/1.1/Tue Nov 24 09:52:53 2026//",
        )
        .unwrap();
        assert!(changed);
        assert_eq!(
            targets[""].entries.file("hello.txt").unwrap().date,
            "Tue Nov 24 09:52:53 2026"
        );

        // A folder the request did not name is left alone.
        assert!(!record_checked_in(
            &mut targets,
            "other/",
            "/hello.txt/1.1/Tue Nov 24 09:52:53 2026//"
        )
        .unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }
}
