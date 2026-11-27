//! `cvs rlog`: revision history for a module, with no working copy needed.
//!
//! The server does all the work; this client only names the modules and sends the options in
//! the reference's fixed order (`-b`, `-d`, `-h`, `-l`, `-N`, `-S`, `-R`, `-t`, `-r`, `-s`,
//! `-w`). `-r` takes its value attached (`-r1.1`) and may repeat; each comma-separated item is
//! widened to a range, as the reference does, while `1.1.` (a branch) and anything with a
//! colon stay as written. `-d` and `-s` take their value attached or as the next argument.
//! `-w` also takes only an attached value; a bare `-w` is `@@MYSELF`.

use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

#[derive(Debug, Default)]
struct Options {
    default_branch: bool,
    header: bool,
    local: bool,
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

fn parse_args(args: &[String]) -> Result<Options> {
    let args = super::log::expand_short_options(args);
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            opts.names.extend(args[i + 1..].iter().cloned());
            break;
        }
        // getopt runs with a leading `+`: the first module name ends the options.
        if !arg.starts_with('-') || arg == "-" {
            opts.names.extend(args[i..].iter().cloned());
            break;
        }

        if let Some(rest) = arg.strip_prefix("-d") {
            let value = if rest.is_empty() {
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| Error::aborted("rlog", "option -d requires an argument"))?
            } else {
                rest.to_string()
            };
            let dates = date::normalize_dates(&value)
                .map_err(|bad| Error::aborted("rlog", format!("Can't parse date/time: {}", bad)))?;
            opts.dates.extend(dates);
        } else if let Some(rest) = arg.strip_prefix("-s") {
            let value = if rest.is_empty() {
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| Error::aborted("rlog", "option -s requires an argument"))?
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
            opts.logins.push(String::new());
        } else if let Some(rest) = arg.strip_prefix("-w") {
            opts.logins.extend(split_list(rest));
        } else {
            match arg.as_str() {
                "-b" => opts.default_branch = true,
                "-h" => opts.header = true,
                "-l" => opts.local = true,
                "-N" => opts.no_tags = true,
                "-n" => opts.no_tags = false,
                "-S" => opts.sup_header = true,
                "-R" => opts.rcs_name_only = true,
                "-t" => opts.header_and_description = true,
                other => return Err(invalid_option("rlog", other)),
            }
        }
        i += 1;
    }
    Ok(opts)
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',').map(|item| item.to_string()).collect()
}

fn invalid_option(command: &str, arg: &str) -> Error {
    let letter = arg
        .strip_prefix('-')
        .and_then(|rest| rest.chars().next())
        .unwrap_or('?');
    Error::plain(command, format!("invalid option -- '{}'", letter))
}

/// Run `cvs rlog`.
pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    let opts = parse_args(args)?;

    let mut session = protocol::open(root, "rlog", cancel, sink)?;
    if !session.supports("rlog") {
        return Err(Error::plain("rlog", "server does not support rlog"));
    }
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
    if opts.local {
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
        for argument in super::log::revision_arguments(spec) {
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

    session.argument("--")?;
    for name in &opts.names {
        session.argument(name)?;
    }
    session.send("rlog")?;
    session.flush()?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::super::log::revision_arguments;
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn one_revision_is_widened_to_a_range_and_a_branch_is_left_alone() {
        assert_eq!(revision_arguments("1.1"), vec!["-r1.1:1.1".to_string()]);
        assert_eq!(revision_arguments("HEAD"), vec!["-rHEAD:HEAD".to_string()]);
        assert_eq!(revision_arguments("1.1:1.2"), vec!["-r1.1:1.2".to_string()]);
        assert_eq!(
            revision_arguments("1.1.2"),
            vec!["-r1.1.2:1.1.2".to_string()]
        );
        assert_eq!(revision_arguments("1.1."), vec!["-r1.1.".to_string()]);
        assert_eq!(revision_arguments(""), vec!["-r:".to_string()]);
    }

    #[test]
    fn a_comma_list_becomes_one_range_per_item() {
        assert_eq!(
            revision_arguments("1.1,1.3"),
            vec!["-r1.1:1.1".to_string(), "-r1.3:1.3".to_string()]
        );
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let error = parse(&["-q", "mod"]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = parse(&["-N", "--", "-mod"]).unwrap();
        assert!(opts.no_tags);
        assert_eq!(opts.names, vec!["-mod".to_string()]);
    }

    #[test]
    fn d_and_s_require_a_value() {
        for option in ["-d", "-s"] {
            let error = parse(&[option]).unwrap_err();
            assert!(
                error
                    .text()
                    .contains(&format!("option {} requires an argument", option)),
                "{}",
                error
            );
        }
    }

    #[test]
    fn a_revision_value_may_be_attached_or_missing() {
        let opts = parse(&["-r1.1", "-r1.2:1.3", "mod"]).unwrap();
        assert_eq!(opts.revisions, vec!["1.1", "1.2:1.3"]);

        // `-r` alone is the reference's empty filter, not a missing value.
        let opts = parse(&["-r", "mod"]).unwrap();
        assert_eq!(opts.revisions, vec![String::new()]);
        assert_eq!(opts.names, vec!["mod".to_string()]);
    }

    #[test]
    fn a_state_and_a_date_take_their_value_attached_or_next() {
        let expected = date::normalize("2020-01-01");
        let opts = parse(&["-sExp", "-d", "2020-01-01", "mod"]).unwrap();
        assert_eq!(opts.states, vec!["Exp"]);
        assert_eq!(opts.dates, vec![expected.clone()]);

        let opts = parse(&["-s", "Exp", "-d2020-01-01", "mod"]).unwrap();
        assert_eq!(opts.states, vec!["Exp"]);
        assert_eq!(opts.dates, vec![expected]);
    }

    #[test]
    fn repeated_dates_states_and_logins_accumulate() {
        let opts = parse(&[
            "-d",
            "2020-01-01",
            "-d2020-02-01",
            "-sExp,dead",
            "-sUnchanged",
            "-wann",
            "-w",
            "mod",
        ])
        .unwrap();
        assert_eq!(opts.dates.len(), 2);
        assert_eq!(opts.states, vec!["Exp", "dead", "Unchanged"]);
        assert_eq!(opts.logins, vec!["ann", ""]);
    }

    #[test]
    fn w_takes_only_an_attached_login() {
        let opts = parse(&["-w", "mod"]).unwrap();
        assert_eq!(opts.logins, vec![String::new()]);
        assert_eq!(opts.names, vec!["mod".to_string()]);

        let opts = parse(&["-wann", "mod"]).unwrap();
        assert_eq!(opts.logins, vec!["ann".to_string()]);
    }

    #[test]
    fn sup_header_n_and_clusters_are_accepted() {
        let opts = parse(&["-SN", "-n", "mod"]).unwrap();
        assert!(opts.sup_header);
        assert!(!opts.no_tags);

        let opts = parse(&["-hR", "mod"]).unwrap();
        assert!(opts.header);
        assert!(opts.rcs_name_only);
    }

    #[test]
    fn names_end_the_options_like_the_reference() {
        let opts = parse(&["mod", "-N"]).unwrap();
        assert!(!opts.no_tags);
        assert_eq!(opts.names, vec!["mod".to_string(), "-N".to_string()]);
    }

    #[test]
    fn no_arguments_leaves_every_filter_empty() {
        let opts = parse(&[]).unwrap();
        assert!(opts.names.is_empty());
        assert!(opts.revisions.is_empty());
        assert!(opts.dates.is_empty());
    }
}
