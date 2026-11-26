//! `cvs rlog`: revision history for a module, with no working copy needed.
//!
//! The server does all the work; this client only names the modules and sends the options in
//! the reference's fixed order (`-b`, `-d`, `-h`, `-l`, `-N`, `-R`, `-t`, `-r`, `-s`, `-w`).
//! `-r` takes its value attached (`-r1.1`) and may repeat; a single revision is widened to a
//! range, as the reference does, while `1.1.` (a branch) and anything with a colon stay as
//! written. `-d` and `-s` take their value attached or as the next argument.

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
    rcs_name_only: bool,
    header_and_description: bool,
    dates: Option<String>,
    revisions: Vec<String>,
    states: Option<String>,
    logins: Option<String>,
    names: Vec<String>,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--" => {
                opts.names.extend(args[i + 1..].iter().cloned());
                break;
            }
            "-b" => opts.default_branch = true,
            "-h" => opts.header = true,
            "-l" => opts.local = true,
            "-N" => opts.no_tags = true,
            "-R" => opts.rcs_name_only = true,
            "-t" => opts.header_and_description = true,
            "-r" => opts.revisions.push(String::new()),
            "-w" => opts.logins = Some(String::new()),
            "-d" | "-s" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    Error::aborted("rlog", format!("option {} requires an argument", arg))
                })?;
                if arg == "-d" {
                    opts.dates = Some(value.clone());
                } else {
                    opts.states = Some(value.clone());
                }
            }
            other if other.starts_with("-d") && other.len() > 2 => {
                opts.dates = Some(other[2..].to_string());
            }
            other if other.starts_with("-s") && other.len() > 2 => {
                opts.states = Some(other[2..].to_string());
            }
            other if other.starts_with("-r") && other.len() > 2 => {
                opts.revisions.push(other[2..].to_string());
            }
            other if other.starts_with("-w") && other.len() > 2 => {
                opts.logins = Some(other[2..].to_string());
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "rlog",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.names.push(other.to_string()),
        }
        i += 1;
    }
    Ok(opts)
}

/// The reference widens a single revision to a range (`1.1` becomes `1.1:1.1`), but leaves a
/// branch (`1.1.`) and a range already written with a colon alone.
fn normalize_revisions(text: &str) -> String {
    if text.contains(':') || text.ends_with('.') {
        text.to_string()
    } else {
        format!("{}:{}", text, text)
    }
}

/// Run `cvs rlog`.
pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    if args.is_empty() {
        // The reference looks for a module named like the command and reports it; saying so
        // without a connection gives the same message and exit code.
        return Err(Error::plain("rlog", "cannot find module `rlog' - ignored"));
    }
    let opts = parse_args(args)?;

    let mut session = protocol::open(root, "rlog", cancel, sink)?;
    if opts.default_branch {
        session.argument("-b")?;
    }
    if let Some(dates) = &opts.dates {
        session.argument("-d")?;
        session.argument(&date::normalize(dates))?;
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
    if opts.rcs_name_only {
        session.argument("-R")?;
    }
    if opts.header_and_description {
        session.argument("-t")?;
    }
    for revisions in &opts.revisions {
        session.argument(&format!("-r{}", normalize_revisions(revisions)))?;
    }
    if let Some(states) = &opts.states {
        session.argument(&format!("-s{}", states))?;
    }
    if let Some(logins) = &opts.logins {
        session.argument(&format!("-w{}", logins))?;
    }
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
    use super::*;
    use crate::client::engine::test_sink::RecordingSink;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn one_revision_is_widened_to_a_range_and_a_branch_is_left_alone() {
        assert_eq!(normalize_revisions("1.1"), "1.1:1.1");
        assert_eq!(normalize_revisions("HEAD"), "HEAD:HEAD");
        assert_eq!(normalize_revisions("1.1:1.2"), "1.1:1.2");
        assert_eq!(normalize_revisions("1.1.2"), "1.1.2:1.1.2");
        assert_eq!(normalize_revisions("1.1."), "1.1.");
        assert_eq!(normalize_revisions(""), ":");
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
        let opts = parse(&["-sExp", "-d", "2020-01-01", "mod"]).unwrap();
        assert_eq!(opts.states.as_deref(), Some("Exp"));
        assert_eq!(opts.dates.as_deref(), Some("2020-01-01"));

        let opts = parse(&["-s", "Exp", "-d2020-01-01", "mod"]).unwrap();
        assert_eq!(opts.states.as_deref(), Some("Exp"));
        assert_eq!(opts.dates.as_deref(), Some("2020-01-01"));
    }

    #[test]
    fn w_takes_only_an_attached_login() {
        let opts = parse(&["-w", "mod"]).unwrap();
        assert_eq!(opts.logins.as_deref(), Some(""));
        assert_eq!(opts.names, vec!["mod".to_string()]);

        let opts = parse(&["-wann", "mod"]).unwrap();
        assert_eq!(opts.logins.as_deref(), Some("ann"));
    }

    #[test]
    fn no_arguments_is_the_missing_module_the_reference_reports() {
        let mut sink = RecordingSink::default();
        let error = run(
            Root::parse(":pserver:ann@host:/repo").unwrap(),
            &[],
            &CancelToken::default(),
            &mut sink,
        )
        .unwrap_err();
        assert_eq!(
            error.text(),
            "cvs rlog: cannot find module `rlog' - ignored"
        );
        assert!(sink.lines.is_empty());
    }
}
