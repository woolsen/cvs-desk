//! `cvs rtag`: put a symbolic tag on repository modules, with no working copy.
//!
//! Every flag, the tag name and each module are plain `Argument`s; there are no
//! `Directory`/`Entry` blocks. `-n` ("no execution of 'tag program'") is sent like any other
//! flag and the server decides what to do with it, exactly as the reference does.

use crate::client::cli;
use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

#[derive(Debug, Default)]
struct Options {
    clear_removed: bool,
    branch: bool,
    delete: bool,
    move_tag: bool,
    force: bool,
    recursive: bool,
    no_execute: bool,
    revision: Option<String>,
    date: Option<String>,
    tag: String,
    modules: Vec<String>,
}

/// `rtag` stops taking options at the tag name, as `tag` does.
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
        match arg.as_str() {
            "-a" => opts.clear_removed = true,
            "-b" => opts.branch = true,
            "-d" => opts.delete = true,
            "-F" => opts.move_tag = true,
            "-f" => opts.force = true,
            "-l" => opts.recursive = false,
            "-R" => opts.recursive = true,
            "-n" => opts.no_execute = true,
            "-r" | "-D" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    Error::aborted("rtag", format!("option {} requires an argument", arg))
                })?;
                if arg == "-r" {
                    opts.revision = Some(value.clone());
                } else {
                    opts.date = Some(value.clone());
                }
            }
            other if other.starts_with("-r") && other.len() > 2 => {
                opts.revision = Some(other[2..].to_string())
            }
            other if other.starts_with("-D") && other.len() > 2 => {
                opts.date = Some(other[2..].to_string())
            }
            other => {
                return Err(Error::plain(
                    "rtag",
                    format!("invalid option -- '{}'", other),
                ))
            }
        }
        i += 1;
    }
    if let Some((tag, modules)) = operands.split_first() {
        opts.tag = tag.clone();
        opts.modules = modules.to_vec();
    }
    Ok(opts)
}

/// Run `cvs rtag`.
pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    let opts = parse_args(args)?;
    if opts.tag.is_empty() || opts.modules.is_empty() {
        // The reference prints the command's usage and exits 1, without a prefix.
        if let Some(text) = cli::command_help("rtag") {
            sink.err(text.to_string());
        }
        return Ok(1);
    }
    if opts.revision.is_some() && opts.date.is_some() {
        return Err(Error::aborted(
            "rtag",
            "-r and -D options are mutually exclusive",
        ));
    }
    if opts.branch && opts.delete {
        sink.err("cvs rtag: warning: -b ignored with -d options".to_string());
    }

    let mut session = protocol::open(root, "rtag", cancel, sink)?;
    send_arguments(&mut session, &opts)?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

/// The flags, the tag name and the modules, in the order the reference writes them.
fn send_arguments(session: &mut protocol::Session, opts: &Options) -> Result<()> {
    if opts.clear_removed {
        session.argument("-a")?;
    }
    if opts.branch {
        session.argument("-b")?;
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
    if opts.no_execute {
        session.argument("-n")?;
    }
    if let Some(revision) = &opts.revision {
        session.argument("-r")?;
        session.argument(revision)?;
    }
    if let Some(date) = &opts.date {
        session.argument("-D")?;
        session.argument(&date::normalize(date))?;
    }
    session.argument(&opts.tag)?;
    for module in &opts.modules {
        session.argument(module)?;
    }
    session.send("rtag")?;
    session.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn all_the_flags_are_read_before_the_tag_name() {
        let opts = parse(&["-a", "-b", "-d", "-F", "-f", "-l", "-n", "T1", "mod"]).unwrap();
        assert!(opts.clear_removed);
        assert!(opts.branch);
        assert!(opts.delete);
        assert!(opts.move_tag);
        assert!(opts.force);
        assert!(!opts.recursive);
        assert!(opts.no_execute);
        assert_eq!(opts.tag, "T1");
        assert_eq!(opts.modules, vec!["mod"]);
    }

    #[test]
    fn a_dash_word_after_the_tag_is_a_module() {
        let opts = parse(&["T1", "mod", "-n"]).unwrap();
        assert!(!opts.no_execute);
        assert_eq!(opts.tag, "T1");
        assert_eq!(opts.modules, vec!["mod", "-n"]);
    }

    #[test]
    fn r_is_the_default_again_after_l() {
        let opts = parse(&["-l", "-R", "T1", "mod"]).unwrap();
        assert!(opts.recursive);
    }

    #[test]
    fn a_revision_and_a_date_take_a_separate_or_attached_value() {
        let opts = parse(&["-r", "1.1", "-D2020-01-01", "T1", "mod"]).unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
        assert_eq!(opts.date.as_deref(), Some("2020-01-01"));
    }

    #[test]
    fn a_missing_value_is_refused() {
        for option in ["-r", "-D"] {
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
    fn an_unknown_option_is_refused() {
        let error = parse(&["-Z", "T1", "mod"]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = parse(&["--", "T1", "-mod"]).unwrap();
        assert_eq!(opts.tag, "T1");
        assert_eq!(opts.modules, vec!["-mod"]);
    }
}
