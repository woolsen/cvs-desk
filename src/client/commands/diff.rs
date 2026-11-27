//! `cvs diff`: compare the working files with the repository.
//!
//! The server runs the comparison and sends the diff text; this client supplies the folder
//! blocks, the file states, and the options the user asked for. The reference client accepts
//! the whole GNU diff option set (`src/diff.c:336-369`) and forwards each option as an
//! `Argument`, so this parser does the same: `-l` and `-N` first, then the format options in
//! the user's order, the `-k` mode, the revisions/dates, and `--`.
//!
//! The exit code follows the reference: `ok` means no differences (0), an `error` response
//! means the server's diff exited non-zero (1) and a non-empty error message is a failure.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, Response};
use crate::client::root::Root;
use crate::client::work::dir;

#[derive(Debug, Default)]
struct Options {
    /// `-l`: the server must not recurse below the named folders. `-R` clears it again,
    /// exactly as the reference's single `local` variable does (`src/diff.c:370-375`).
    local: bool,
    /// `-N`: treat absent files as empty.
    empty_files: bool,
    names: Vec<String>,
    /// GNU diff arguments, one per `Argument` line, in the user's order.
    format: Vec<String>,
    /// Normalized `-k<mode>`, sent after the format options as the reference does.
    kflag: Option<String>,
    rev1: Option<String>,
    date1: Option<String>,
    rev2: Option<String>,
    date2: Option<String>,
    /// Long options that only the server can judge are not checked here, but a third `-L`
    /// is dropped with the reference's warning.
    labels: usize,
    warnings: Vec<String>,
}

impl Options {
    /// `-r` and `-D` share two slots; a third is refused before any connection.
    fn add_revision(&mut self, value: String) -> Result<()> {
        if self.rev2.is_some() || self.date2.is_some() {
            return Err(Error::plain(
                "diff",
                "no more than two revisions/dates can be specified",
            ));
        }
        if self.rev1.is_some() || self.date1.is_some() {
            self.rev2 = Some(value);
        } else {
            self.rev1 = Some(value);
        }
        Ok(())
    }

    fn add_date(&mut self, value: String) -> Result<()> {
        if self.rev2.is_some() || self.date2.is_some() {
            return Err(Error::plain(
                "diff",
                "no more than two revisions/dates can be specified",
            ));
        }
        if self.rev1.is_some() || self.date1.is_some() {
            self.date2 = Some(value);
        } else {
            self.date1 = Some(value);
        }
        Ok(())
    }

    /// The reference sends the first two `-L` labels and warns about the rest
    /// (`src/diff.c:353-360`).
    fn add_label(&mut self, value: String) {
        self.labels += 1;
        if self.labels > 2 {
            self.warnings
                .push("cvs diff: extra -L arguments ignored".to_string());
            return;
        }
        self.format.push("-L".to_string());
        self.format.push(value);
    }

    /// `RCS_check_kflag` accepts exactly these modes and sends `-k<mode>` as one argument.
    fn set_kflag(&mut self, value: String) -> Result<()> {
        const MODES: [&str; 6] = ["kv", "kvl", "k", "v", "o", "b"];
        if !MODES.contains(&value.as_str()) {
            return Err(Error::plain("diff", "invalid RCS keyword expansion mode"));
        }
        self.kflag = Some(format!("-k{}", value));
        Ok(())
    }
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].clone();
        if arg == "--" {
            opts.names.extend(args[i + 1..].iter().cloned());
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            parse_long(&mut opts, long, args, &mut i)?;
        } else if arg.len() > 1 && arg.starts_with('-') {
            parse_short(&mut opts, &arg, args, &mut i)?;
        } else {
            // A lone `-` is a file name, as getopt treats it.
            opts.names.push(arg);
        }
        i += 1;
    }
    Ok(opts)
}

/// Split a short-option cluster the way getopt does. A letter that takes a value consumes
/// the rest of the word, or the next argument when the rest is empty (`-C3`, `-C 3`).
fn parse_short(opts: &mut Options, arg: &str, args: &[String], i: &mut usize) -> Result<()> {
    let text = &arg[1..];
    for (offset, letter) in text.char_indices() {
        match letter {
            'l' => opts.local = true,
            'R' => opts.local = false,
            'N' => opts.empty_files = true,
            'a'
            | 'b'
            | 'c'
            | 'd'
            | 'e'
            | 'f'
            | 'h'
            | 'i'
            | 'n'
            | 'p'
            | 's'
            | 't'
            | 'u'
            | 'w'
            | '0'..='9'
            | 'B'
            | 'H'
            | 'T' => {
                opts.format.push(format!("-{}", letter));
            }
            // `case 'y'` in `src/diff.c:342-344` spells the long name out.
            'y' => opts.format.push("--side-by-side".to_string()),
            'C' | 'D' | 'F' | 'I' | 'L' | 'U' | 'W' | 'k' | 'r' => {
                let attached = &text[offset + letter.len_utf8()..];
                let value = if attached.is_empty() {
                    take_value(args, i, letter)?
                } else {
                    attached.to_string()
                };
                match letter {
                    'C' | 'F' | 'I' | 'U' | 'W' => {
                        opts.format.push(format!("-{}", letter));
                        opts.format.push(value);
                    }
                    'L' => opts.add_label(value),
                    'k' => opts.set_kflag(value)?,
                    'r' => opts.add_revision(value)?,
                    'D' => opts.add_date(value)?,
                    _ => unreachable!(),
                }
                return Ok(());
            }
            other => {
                return Err(Error::plain(
                    "diff",
                    format!("invalid option -- '{}'", other),
                ))
            }
        }
    }
    Ok(())
}

/// Long options follow the reference's `longopts` table: aliases of short letters are sent
/// in their short form, while the options without a short spelling stay long.
fn parse_long(opts: &mut Options, text: &str, args: &[String], i: &mut usize) -> Result<()> {
    let (name, attached) = match text.split_once('=') {
        Some((name, value)) => (name, Some(value.to_string())),
        None => (text, None),
    };
    match name {
        // `has_arg == 2`: the number is optional and only an `=NUM` suffix attaches it.
        "context" | "unified" => {
            opts.format.push(format!("--{}", name));
            if let Some(value) = attached {
                opts.format.push(value);
            }
        }
        "ifdef"
        | "old-line-format"
        | "new-line-format"
        | "unchanged-line-format"
        | "line-format"
        | "old-group-format"
        | "new-group-format"
        | "unchanged-group-format"
        | "changed-group-format"
        | "horizon-lines" => {
            let value = long_value(args, i, name, attached)?;
            opts.format.push(format!("--{}", name));
            opts.format.push(value);
        }
        // These long names carry a short `val` in the reference's table, so the switch in
        // `diff` sends the letter (`src/diff.c:361-363`).
        "show-function-line" | "ignore-matching-lines" | "width" => {
            let value = long_value(args, i, name, attached)?;
            let short = match name {
                "show-function-line" => "-F",
                "ignore-matching-lines" => "-I",
                _ => "-W",
            };
            opts.format.push(short.to_string());
            opts.format.push(value);
        }
        "label" => {
            let value = long_value(args, i, name, attached)?;
            opts.add_label(value);
        }
        "new-file" => {
            reject_value(name, &attached)?;
            opts.empty_files = true;
        }
        "side-by-side" => {
            reject_value(name, &attached)?;
            opts.format.push("--side-by-side".to_string());
        }
        "brief" | "binary" | "left-column" | "suppress-common-lines" | "normal" => {
            reject_value(name, &attached)?;
            opts.format.push(format!("--{}", name));
        }
        _ => {
            if let Some(short) = short_alias(name) {
                reject_value(name, &attached)?;
                opts.format.push(short.to_string());
            } else {
                return Err(Error::plain(
                    "diff",
                    format!("unrecognized option '--{}'", name),
                ));
            }
        }
    }
    Ok(())
}

/// The long names whose `val` is a short letter (`src/diff.c:168-213`). getopt hands the
/// letter to `diff`, whose switch sends `-<letter>` (`src/diff.c:345-351`).
fn short_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "text" => "-a",
        "ignore-space-change" => "-b",
        "ignore-blank-lines" => "-B",
        "minimal" => "-d",
        "ed" => "-e",
        "forward-ed" => "-f",
        "speed-large-files" => "-H",
        "ignore-case" => "-i",
        "rcs" => "-n",
        "show-c-function" => "-p",
        "report-identical-files" => "-s",
        "expand-tabs" => "-t",
        "ignore-all-space" => "-w",
        "initial-tab" => "-T",
        _ => return None,
    })
}

fn take_value(args: &[String], i: &mut usize, letter: char) -> Result<String> {
    *i += 1;
    args.get(*i).cloned().ok_or_else(|| {
        Error::plain(
            "diff",
            format!("option requires an argument -- '{}'", letter),
        )
    })
}

fn long_value(
    args: &[String],
    i: &mut usize,
    name: &str,
    attached: Option<String>,
) -> Result<String> {
    match attached {
        Some(value) => Ok(value),
        None => {
            *i += 1;
            args.get(*i).cloned().ok_or_else(|| {
                Error::plain("diff", format!("option '--{}' requires an argument", name))
            })
        }
    }
}

fn reject_value(name: &str, attached: &Option<String>) -> Result<()> {
    if attached.is_some() {
        return Err(Error::plain(
            "diff",
            format!("option '--{}' doesn't allow an argument", name),
        ));
    }
    Ok(())
}

/// Run `cvs diff`. The exit code is 1 when the server reports a difference, 0 when there is
/// none; a server error with a message is a failure.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = parse_args(args)?;
    for warning in &opts.warnings {
        sink.err(warning.clone());
    }
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "diff",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let targets = targets::collect(cwd, !opts.local, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("diff", "no files to examine"));
    }

    let mut session = protocol::open(root, "diff", cancel, sink)?;
    send_options(&mut session, &opts)?;

    // The reference asks the server about untracked files (so it can honour its ignore
    // lists) when the protocol allows it, and names them itself otherwise
    // (`send_ignproc`, client.c:5227-5245).
    if session.supports("Questionable") {
        targets::send_blocks_quiet(&mut session, &targets)?;
    } else {
        report_untracked(&mut session, &targets);
        targets::send_blocks(&mut session, &targets, false)?;
    }
    targets::finish_to_top(&mut session, cwd, &opts.names, "diff")?;
    let code = read_diff(&mut session)?;
    session.close();
    Ok(code)
}

/// `-l`, `-N`, the format options, `-k`, then the revisions and dates, each value as its own
/// `Argument` (`src/diff.c:422-437`). `--` follows in `targets::begin_files`.
fn send_options(session: &mut protocol::Session, opts: &Options) -> Result<()> {
    if opts.local {
        session.argument("-l")?;
    }
    if opts.empty_files {
        session.argument("-N")?;
    }
    for option in &opts.format {
        session.argument(option)?;
    }
    if let Some(kflag) = &opts.kflag {
        session.argument(kflag)?;
    }
    if let Some(rev) = &opts.rev1 {
        session.argument("-r")?;
        session.argument(rev)?;
    }
    if let Some(date) = &opts.date1 {
        session.argument("-D")?;
        session.argument(&date::normalize(date))?;
    }
    if let Some(rev) = &opts.rev2 {
        session.argument("-r")?;
        session.argument(rev)?;
    }
    if let Some(date) = &opts.date2 {
        session.argument("-D")?;
        session.argument(&date::normalize(date))?;
    }
    Ok(())
}

/// The fallback for servers that do not know `Questionable`: print the same `? name` lines
/// the server would have sent.
fn report_untracked(session: &mut protocol::Session, targets: &BTreeMap<String, targets::Target>) {
    for target in targets.values() {
        if !target.selected {
            continue;
        }
        for name in targets::unknown_files(&target.dir.local, &target.entries) {
            session.out(format!(
                "? {}",
                dir::display_path(&target.dir.update_dir, &name)
            ));
        }
    }
}

/// Read the text-only answer. `ok` is "no differences"; any `error` is a difference or a
/// server failure, exactly as `get_server_responses` reports it (client.c:3483-3487).
/// The text is printed as it arrives.
fn read_diff(session: &mut protocol::Session) -> Result<i32> {
    loop {
        match session.read_response()? {
            Response::Ok => return Ok(0),
            Response::E(text) => session.err(text),
            Response::Notified | Response::Trace(_) | Response::Checksum(_) => {}
            Response::M(text) => session.out(text),
            Response::Error { message, .. } if message.trim().is_empty() => {
                // CVSNT's normal ending for a diff that found differences.
                session.err(String::new());
                return Ok(1);
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

    #[test]
    fn revisions_and_dates_share_two_slots() {
        // The first option fills rev1, the date after it fills date2, as the reference does.
        let opts = parse(&["-r1.1", "-D", "2020-01-01", "file"]);
        assert_eq!(opts.rev1.as_deref(), Some("1.1"));
        assert_eq!(opts.date1, None);
        assert_eq!(opts.date2.as_deref(), Some("2020-01-01"));
        assert!(parse_args(&[
            "-r1.1".to_string(),
            "-r1.2".to_string(),
            "-r1.3".to_string()
        ])
        .is_err());
    }

    #[test]
    fn a_cluster_splits_into_single_letter_options() {
        let opts = parse(&["-uN", "file"]);
        assert_eq!(opts.format, ["-u"]);
        assert!(opts.empty_files);

        // `-lR` ends recursive, `-Rl` stays local, just as getopt applies them in order.
        assert!(!parse(&["-lR"]).local);
        assert!(parse(&["-Rl"]).local);
    }

    #[test]
    fn an_option_value_may_be_attached_or_separate() {
        let opts = parse(&["-C3", "-U", "5", "-r1.1", "-k", "kv"]);
        assert_eq!(opts.format, ["-C", "3", "-U", "5"]);
        assert_eq!(opts.rev1.as_deref(), Some("1.1"));
        assert_eq!(opts.kflag.as_deref(), Some("-kkv"));
    }

    #[test]
    fn long_options_follow_the_reference_wire_form() {
        let opts = parse(&[
            "--context=3",
            "--unified",
            "--ifdef=NAME",
            "--brief",
            "--text",
            "--side-by-side",
            "--show-c-function",
            "--width=80",
        ]);
        assert_eq!(
            opts.format,
            [
                "--context",
                "3",
                "--unified",
                "--ifdef",
                "NAME",
                "--brief",
                "-a",
                "--side-by-side",
                "-p",
                "-W",
                "80"
            ]
        );
    }

    #[test]
    fn a_long_option_argument_may_be_a_separate_word() {
        let opts = parse(&["--ifdef", "NAME", "--label", "left"]);
        assert_eq!(opts.format, ["--ifdef", "NAME", "-L", "left"]);
    }

    #[test]
    fn numbers_are_context_options_and_y_names_its_long_form() {
        assert_eq!(parse(&["-3"]).format, ["-3"]);
        assert_eq!(parse(&["-12"]).format, ["-1", "-2"]);
        assert_eq!(parse(&["-y"]).format, ["--side-by-side"]);
    }

    #[test]
    fn two_dashes_end_the_options() {
        let opts = parse(&["-u", "--", "-C3", "file"]);
        assert!(opts.names.contains(&"-C3".to_string()));
        assert!(opts.names.contains(&"file".to_string()));
        assert_eq!(opts.format, ["-u"]);
    }

    #[test]
    fn a_third_label_is_dropped_with_a_warning() {
        let opts = parse(&["-L", "one", "-L", "two", "-L", "three"]);
        assert_eq!(opts.format, ["-L", "one", "-L", "two"]);
        assert_eq!(opts.warnings, ["cvs diff: extra -L arguments ignored"]);
    }

    #[test]
    fn an_unknown_or_invalid_option_is_refused() {
        assert!(parse_args(&["-Z".to_string()]).is_err());
        assert!(parse_args(&["--nonsense".to_string()]).is_err());
        assert!(parse_args(&["-k".to_string(), "bogus".to_string()]).is_err());
        assert!(parse_args(&["-C".to_string()]).is_err());
    }
}
