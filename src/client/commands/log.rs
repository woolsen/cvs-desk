//! `cvs log`: the revision history of files, formatted by the server.
//!
//! Like `status`, this client only names the folders and files and prints the server's text.
//! Options that change what is printed (`-N`/`-n`, `-h`, `-R`, `-r`, `-d`, `-s`, `-w`, `-b`,
//! `-t`, `-S`) are passed as protocol arguments in the reference's fixed order; `-l` also stops
//! the local recursion and is sent as well.

use std::path::Path;

use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

#[derive(Debug, Default)]
struct Options {
    recursive: bool,
    default_branch: bool,
    header: bool,
    no_tags: bool,
    sup_header: bool,
    rcs_name_only: bool,
    header_and_description: bool,
    /// Normalized values, one `Argument -d` each.
    dates: Vec<String>,
    /// Each `-r` value as written; one value may list several revisions.
    revisions: Vec<String>,
    states: Vec<String>,
    /// An empty entry is a bare `-w`, the reference's `@@MYSELF`.
    logins: Vec<String>,
    names: Vec<String>,
}

/// Split clustered flags (`-lN`) into single ones, stopping at `--`. A flag that takes a value
/// keeps the rest of the word attached (`-r1.1`, `-d2020-01-01`), as the reference's getopt
/// does; an unknown letter leaves the whole word for the parser to refuse.
pub(super) fn expand_short_options(args: &[String]) -> Vec<String> {
    const NO_ARG: &str = "bhlNnSRt";
    const WITH_ARG: &str = "dsrw";

    let mut out = Vec::with_capacity(args.len());
    let mut after_double_dash = false;
    for arg in args {
        if after_double_dash {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            after_double_dash = true;
            out.push(arg.clone());
            continue;
        }
        let Some(text) = arg.strip_prefix('-') else {
            out.push(arg.clone());
            continue;
        };
        if text.len() < 2 || text.starts_with('-') || !text.is_ascii() {
            out.push(arg.clone());
            continue;
        }
        let mut parts = Vec::new();
        let mut rest = text;
        let mut valid = true;
        while let Some(ch) = rest.chars().next() {
            rest = &rest[ch.len_utf8()..];
            if NO_ARG.contains(ch) {
                parts.push(format!("-{}", ch));
            } else if WITH_ARG.contains(ch) {
                parts.push(format!("-{}{}", ch, rest));
                rest = "";
            } else {
                valid = false;
                break;
            }
        }
        if valid {
            out.extend(parts);
        } else {
            out.push(arg.clone());
        }
    }
    out
}

fn parse_args(args: &[String]) -> Result<Options> {
    let args = expand_short_options(args);
    let mut opts = Options {
        recursive: true,
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            opts.names.extend(args[i + 1..].iter().cloned());
            break;
        }
        // getopt runs with a leading `+`: the first name ends the options.
        if !arg.starts_with('-') || arg == "-" {
            opts.names.extend(args[i..].iter().cloned());
            break;
        }

        if let Some(rest) = arg.strip_prefix("-d") {
            let value = if rest.is_empty() {
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| Error::aborted("log", "option -d requires an argument"))?
            } else {
                rest.to_string()
            };
            let dates = date::normalize_dates(&value)
                .map_err(|bad| Error::aborted("log", format!("Can't parse date/time: {}", bad)))?;
            opts.dates.extend(dates);
        } else if let Some(rest) = arg.strip_prefix("-s") {
            let value = if rest.is_empty() {
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| Error::aborted("log", "option -s requires an argument"))?
            } else {
                rest.to_string()
            };
            opts.states.extend(split_list(&value));
        } else if arg == "-r" {
            // `r::`: a bare `-r` is the head of the default branch, not the next argument.
            opts.revisions.push(String::new());
        } else if let Some(rest) = arg.strip_prefix("-r") {
            opts.revisions.push(rest.to_string());
        } else if arg == "-w" {
            // A bare `-w` filters on the user running the command, `@@MYSELF`.
            opts.logins.push(String::new());
        } else if let Some(rest) = arg.strip_prefix("-w") {
            opts.logins.extend(split_list(rest));
        } else {
            match arg.as_str() {
                "-b" => opts.default_branch = true,
                "-h" => opts.header = true,
                "-l" => opts.recursive = false,
                "-N" => opts.no_tags = true,
                "-n" => opts.no_tags = false,
                "-S" => opts.sup_header = true,
                "-R" => opts.rcs_name_only = true,
                "-t" => opts.header_and_description = true,
                other => return Err(invalid_option("log", other)),
            }
        }
        i += 1;
    }
    Ok(opts)
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',').map(|item| item.to_string()).collect()
}

/// `cvs log: invalid option -- 'x'`, naming the first unknown letter.
fn invalid_option(command: &str, arg: &str) -> Error {
    let letter = arg
        .strip_prefix('-')
        .and_then(|rest| rest.chars().next())
        .unwrap_or('?');
    Error::plain(command, format!("invalid option -- '{}'", letter))
}

/// Expand one `-r` value the way the reference does: split on commas, widen a single
/// revision to `from:to`, keep ranges and branch heads, and send a bare `-r` as `-r:`.
pub(super) fn revision_arguments(spec: &str) -> Vec<String> {
    if spec.is_empty() {
        return vec!["-r:".to_string()];
    }
    spec.split(',')
        .map(|item| format!("-r{}", widen_revision(item)))
        .collect()
}

fn widen_revision(item: &str) -> String {
    if let Some((first, rest)) = item.split_once(':') {
        // A second colon marks an exclusive range (`1.1::1.2`) and stays.
        if let Some(last) = rest.strip_prefix(':') {
            format!("{}::{}", first, last)
        } else {
            format!("{}:{}", first, rest)
        }
    } else if item.ends_with('.') {
        // `1.1.` is a branch head; the reference sends it as written.
        item.to_string()
    } else {
        format!("{}:{}", item, item)
    }
}

/// Run `cvs log`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = parse_args(args)?;
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "log",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;

    let mut session = protocol::open(root, "log", cancel, sink)?;
    if opts.default_branch {
        session.argument("-b")?;
    }
    for dates in &opts.dates {
        session.argument("-d")?;
        session.argument(dates)?;
    }
    if opts.header {
        session.argument("-h")?;
    }
    if !opts.recursive {
        session.argument("-l")?;
    }
    if opts.no_tags {
        session.argument("-N")?;
    }
    if opts.sup_header {
        session.argument("-S")?;
    }
    if opts.rcs_name_only {
        session.argument("-R")?;
    }
    if opts.header_and_description {
        session.argument("-t")?;
    }
    for spec in &opts.revisions {
        for argument in revision_arguments(spec) {
            session.argument(&argument)?;
        }
    }
    for states in &opts.states {
        session.argument(&format!("-s{}", states))?;
    }
    for logins in &opts.logins {
        if logins.is_empty() || logins == "@@MYSELF" {
            session.argument("-w")?;
        } else {
            session.argument(&format!("-w{}", logins))?;
        }
    }

    targets::begin_files(&mut session, &opts.names)?;
    targets::send_blocks(&mut session, &targets, false)?;
    targets::finish_to_top(&mut session, cwd, &opts.names, "log")?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_bare_r_is_a_filter_and_never_swallows_the_next_option() {
        let opts = parse(&["-r", "-N", "file"]).unwrap();
        assert_eq!(opts.revisions, vec![String::new()]);
        assert!(opts.no_tags);
        assert_eq!(opts.names, vec!["file".to_string()]);
    }

    #[test]
    fn a_bare_w_is_myself_and_attached_logins_are_split() {
        let opts = parse(&["-w", "-wann,joe", "file"]).unwrap();
        assert_eq!(opts.logins, vec!["", "ann", "joe"]);
    }

    #[test]
    fn n_overrides_an_earlier_capital_n() {
        let opts = parse(&["-N", "-n", "file"]).unwrap();
        assert!(!opts.no_tags);
    }

    #[test]
    fn clustered_flags_are_expanded() {
        let opts = parse(&["-lN", "-bS", "file"]).unwrap();
        assert!(!opts.recursive);
        assert!(opts.no_tags);
        assert!(opts.default_branch);
        assert!(opts.sup_header);
        assert_eq!(opts.names, vec!["file".to_string()]);
    }

    #[test]
    fn an_attached_value_survives_clustering() {
        let opts = parse(&["-lr1.1", "file"]).unwrap();
        assert!(!opts.recursive);
        assert_eq!(opts.revisions, vec!["1.1".to_string()]);
    }

    #[test]
    fn repeated_dates_states_and_logins_accumulate() {
        let opts = parse(&[
            "-d",
            "2020-01-01",
            "-d",
            "2020-02-01",
            "-s",
            "Exp",
            "-sdead",
            "-wann",
            "-w",
            "file",
        ])
        .unwrap();
        assert_eq!(opts.dates.len(), 2);
        assert_eq!(opts.states, vec!["Exp", "dead"]);
        assert_eq!(opts.logins, vec!["ann", ""]);
    }

    #[test]
    fn a_bad_date_is_refused_before_connecting() {
        let error = parse(&["-d", "garbage", "file"]).unwrap_err();
        assert!(
            error.text().contains("Can't parse date/time: garbage"),
            "{}",
            error
        );

        let error = parse(&["-d", "2020-02-01<2020-01-01", "file"]).unwrap_err();
        assert!(error.text().contains("Can't parse date/time"), "{}", error);
    }

    #[test]
    fn names_end_the_options_like_the_reference() {
        let opts = parse(&["file", "-N"]).unwrap();
        assert!(!opts.no_tags);
        assert_eq!(opts.names, vec!["file".to_string(), "-N".to_string()]);
    }

    #[test]
    fn a_bare_r_sends_the_empty_range_and_a_list_is_split() {
        assert_eq!(revision_arguments(""), vec!["-r:".to_string()]);
        assert_eq!(
            revision_arguments("1.1,1.3"),
            vec!["-r1.1:1.1".to_string(), "-r1.3:1.3".to_string()]
        );
        assert_eq!(revision_arguments("1.1."), vec!["-r1.1.".to_string()]);
    }
}
