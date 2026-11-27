//! `cvs rtag`: put a symbolic tag on repository modules, with no working copy.
//!
//! Every flag, the `--` that ends them, the tag name and each module are plain `Argument`s;
//! there are no `Directory`/`Entry` blocks. `-n` ("no execution of 'tag program'") is sent
//! like any other flag and the server decides what to do with it, exactly as the reference
//! does.

use crate::client::cli;
use crate::client::commands::tag::{check_date, check_tag_name};
use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

#[derive(Debug, Default)]
struct Options {
    clear_removed: bool,
    branch: bool,
    /// `-B`: let `-F`/`-d` move or delete a branch tag.
    disturb_branch: bool,
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

/// `rtag` stops taking options at the tag name, as `tag` does. Clusters are split the way
/// getopt splits them, including a value attached to `-r`/`-D`.
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
                'a' => opts.clear_removed = true,
                'b' => opts.branch = true,
                'B' => opts.disturb_branch = true,
                'd' => opts.delete = true,
                'F' => opts.move_tag = true,
                'f' => opts.force = true,
                'l' => opts.recursive = false,
                'R' => opts.recursive = true,
                'n' => opts.no_execute = true,
                'q' | 'Q' => {
                    // The reference sends these from its own newer client, so the command
                    // refuses them when they are not a global option.
                    return Err(Error::aborted(
                        "rtag",
                        "-q or -Q must be specified before \"rtag\"",
                    ));
                }
                'r' | 'D' => {
                    let attached = &body[next..];
                    let value = if attached.is_empty() {
                        i += 1;
                        args.get(i).cloned().ok_or_else(|| {
                            Error::plain(
                                "rtag",
                                format!("option requires an argument -- {}", option),
                            )
                        })?
                    } else {
                        attached.to_string()
                    };
                    if option == 'r' {
                        opts.revision = Some(value);
                    } else {
                        check_date("rtag", &value)?;
                        opts.date = Some(value);
                    }
                    offset = body.len();
                    continue;
                }
                other => return Err(Error::plain("rtag", format!("invalid option -- {}", other))),
            }
            offset = next;
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
    let opts = match parse_args(args) {
        Ok(opts) => opts,
        Err(error) => {
            sink.err(error.text().to_string());
            let text = error.text();
            if text.contains("invalid option --") || text.contains("option requires an argument") {
                print_usage(sink);
            }
            return Ok(1);
        }
    };
    if opts.tag.is_empty() || opts.modules.is_empty() {
        // The reference prints the command's usage and exits 1, without a prefix.
        print_usage(sink);
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
    check_tag_name("rtag", &opts.tag)?;

    let mut session = protocol::open(root, "rtag", cancel, sink)?;
    send_arguments(&mut session, &opts)?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

fn print_usage(sink: &mut dyn Sink) {
    if let Some(text) = cli::command_help("rtag") {
        sink.err(text.to_string());
    }
}

/// The flags, the tag name and the modules, in the order the reference writes them.
fn send_arguments(session: &mut protocol::Session, opts: &Options) -> Result<()> {
    if opts.clear_removed {
        session.argument("-a")?;
    }
    if opts.branch {
        session.argument("-b")?;
    }
    if opts.disturb_branch {
        session.argument("-B")?;
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
    // The server must not read the tag name or a module as one of its own options.
    session.argument("--")?;
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
        let opts = parse(&["-a", "-b", "-B", "-d", "-F", "-f", "-l", "-n", "T1", "mod"]).unwrap();
        assert!(opts.clear_removed);
        assert!(opts.branch);
        assert!(opts.disturb_branch);
        assert!(opts.delete);
        assert!(opts.move_tag);
        assert!(opts.force);
        assert!(!opts.recursive);
        assert!(opts.no_execute);
        assert_eq!(opts.tag, "T1");
        assert_eq!(opts.modules, vec!["mod"]);
    }

    #[test]
    fn clustered_flags_are_split_like_getopt() {
        let opts = parse(&["-aBd", "T1", "mod"]).unwrap();
        assert!(opts.clear_removed);
        assert!(opts.disturb_branch);
        assert!(opts.delete);

        let opts = parse(&["-nbr1.1", "T1", "mod"]).unwrap();
        assert!(opts.no_execute);
        assert!(opts.branch);
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
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
        for option in ["-r", "-D", "-aBdFfr"] {
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
        let error = parse(&["-Z", "T1", "mod"]).unwrap_err();
        assert!(error.text().contains("invalid option -- Z"), "{}", error);
    }

    #[test]
    fn q_and_q_after_the_command_are_refused() {
        for option in ["-q", "-Q"] {
            let error = parse(&[option, "T1", "mod"]).unwrap_err();
            assert!(
                error
                    .text()
                    .contains("-q or -Q must be specified before \"rtag\""),
                "{}",
                error
            );
        }
    }

    #[test]
    fn a_bad_date_is_refused_before_connecting() {
        let error = parse(&["-D", "garbage", "T1", "mod"]).unwrap_err();
        assert!(
            error.text().contains("Can't parse date/time: garbage"),
            "{}",
            error
        );
    }

    #[test]
    fn a_tag_name_must_follow_the_rcs_rule() {
        let error = check_tag_name("rtag", "1.2").unwrap_err();
        assert!(
            error.text().contains("must start with a letter"),
            "{}",
            error
        );
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = parse(&["--", "T1", "-mod"]).unwrap();
        assert_eq!(opts.tag, "T1");
        assert_eq!(opts.modules, vec!["-mod"]);
    }
}
