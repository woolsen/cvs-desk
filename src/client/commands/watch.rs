//! `cvs watch`: turn watch mode on or off, or change which actions are watched.
//!
//! The subcommand comes first, before every option: `watch on -l file`, never
//! `watch -l on file`. `on` and `off` carry no action of their own; `add` and `remove` name
//! one action per `-a`, and a command with no `-a` watches all three, exactly as the
//! reference does.
//!
//! The files are sent as the `status` blocks: one `Directory`/`Entry`/state group per folder,
//! then the names as `Argument`s, then the request. `-l` goes to the server as an `Argument`;
//! `-R` is the default and stays on the client.

use std::path::Path;

use crate::client::cli;
use crate::client::commands::targets;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

/// The actions `-a` accepts, and the watch letters each one stands for. `edit` uses the same
/// table for the watches field of its notification.
pub(crate) fn action_letters(action: &str) -> Option<&'static str> {
    match action {
        "edit" => Some("E"),
        "unedit" => Some("U"),
        "commit" => Some("C"),
        "all" => Some("EUC"),
        "none" => Some(""),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subcommand {
    On,
    Off,
    Add,
    Remove,
}

impl Subcommand {
    fn parse(text: &str) -> Option<Subcommand> {
        match text {
            "on" => Some(Subcommand::On),
            "off" => Some(Subcommand::Off),
            "add" => Some(Subcommand::Add),
            "remove" => Some(Subcommand::Remove),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Subcommand::On => "on",
            Subcommand::Off => "off",
            Subcommand::Add => "add",
            Subcommand::Remove => "remove",
        }
    }

    /// The request this subcommand sends.
    fn request(self) -> &'static str {
        match self {
            Subcommand::On => "watch-on",
            Subcommand::Off => "watch-off",
            Subcommand::Add => "watch-add",
            Subcommand::Remove => "watch-remove",
        }
    }

    /// Only `add` and `remove` take `-a`; the reference refuses it for `on` and `off`.
    fn takes_actions(self) -> bool {
        matches!(self, Subcommand::Add | Subcommand::Remove)
    }
}

#[derive(Debug)]
struct Options {
    sub: Subcommand,
    recursive: bool,
    actions: Vec<String>,
    names: Vec<String>,
}

/// One parsed command line, or the usage the reference prints instead.
enum Parsed {
    Options(Options),
    /// The usage, with the complaint line that goes before it when there is one.
    Usage(Option<String>),
}

fn parse_args(args: &[String]) -> Parsed {
    let Some(first) = args.first() else {
        return Parsed::Usage(None);
    };
    let Some(sub) = Subcommand::parse(first) else {
        return Parsed::Usage(None);
    };
    let mut opts = Options {
        sub,
        recursive: true,
        actions: Vec::new(),
        names: Vec::new(),
    };
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            opts.names.extend(args[i + 1..].iter().cloned());
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            opts.names.extend(args[i..].iter().cloned());
            break;
        }
        match arg.as_str() {
            "-l" => opts.recursive = false,
            "-R" => opts.recursive = true,
            "-a" if sub.takes_actions() => {
                i += 1;
                let Some(value) = args.get(i) else {
                    return Parsed::Usage(Some(format!(
                        "{}: option requires an argument -- a",
                        sub.name()
                    )));
                };
                if action_letters(value).is_none() {
                    return Parsed::Usage(None);
                }
                opts.actions.push(value.clone());
            }
            other => {
                return Parsed::Usage(Some(format!(
                    "{}: invalid option -- {}",
                    sub.name(),
                    other.trim_start_matches('-')
                )))
            }
        }
        i += 1;
    }
    if opts.actions.is_empty() && sub.takes_actions() {
        // The reference watches every action when no `-a` is named.
        opts.actions = ["edit", "unedit", "commit"]
            .iter()
            .map(|action| action.to_string())
            .collect();
    }
    Parsed::Options(opts)
}

/// Run `cvs watch`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = match parse_args(args) {
        Parsed::Options(opts) => opts,
        Parsed::Usage(complaint) => {
            if let Some(line) = complaint {
                sink.err(line);
            }
            if let Some(text) = cli::command_help("watch") {
                sink.err(text.to_string());
            }
            return Ok(1);
        }
    };
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "watch",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("watch", "no files to watch"));
    }

    let mut session = protocol::open(root, "watch", cancel, sink)?;
    if !opts.recursive {
        session.argument("-l")?;
    }
    for action in &opts.actions {
        session.argument("-a")?;
        session.argument(action)?;
    }
    let last = targets::send_blocks_watch(&mut session, &targets)?;
    targets::finish_from_last(
        &mut session,
        cwd,
        &opts.names,
        opts.sub.request(),
        last.as_deref(),
    )?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Parsed {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    fn options(args: &[&str]) -> Options {
        match parse(args) {
            Parsed::Options(opts) => opts,
            Parsed::Usage(line) => panic!("expected options, got usage: {:?}", line),
        }
    }

    fn usage(args: &[&str]) -> Option<String> {
        match parse(args) {
            Parsed::Options(_) => panic!("expected usage"),
            Parsed::Usage(line) => line,
        }
    }

    #[test]
    fn the_subcommand_must_come_first() {
        assert!(usage(&["-l", "on", "a.txt"]).is_none());
        assert!(usage(&[]).is_none());
        assert!(usage(&["bogus", "a.txt"]).is_none());
    }

    #[test]
    fn on_and_off_take_no_actions() {
        let line = usage(&["on", "-a", "commit", "a.txt"]).expect("a complaint first");
        assert!(line.starts_with("on: invalid option -- a"), "{}", line);
    }

    #[test]
    fn a_missing_action_value_complains_before_the_usage() {
        let line = usage(&["add", "-a"]).expect("a complaint first");
        assert!(
            line.contains("option requires an argument -- a"),
            "{}",
            line
        );
    }

    #[test]
    fn an_unknown_action_prints_the_usage_alone() {
        assert!(usage(&["add", "-a", "bogus"]).is_none());
        assert!(usage(&["add", "-a", "edit,unedit"]).is_none());
    }

    #[test]
    fn add_without_an_action_watches_all_three() {
        let opts = options(&["add", "a.txt"]);
        assert_eq!(opts.actions, vec!["edit", "unedit", "commit"]);
        assert!(opts.recursive);
    }

    #[test]
    fn repeated_actions_keep_their_order_and_r_undoes_l() {
        let opts = options(&["remove", "-l", "-R", "-a", "unedit", "-a", "edit", "a.txt"]);
        assert_eq!(opts.actions, vec!["unedit", "edit"]);
        assert!(opts.recursive);
        assert_eq!(opts.names, vec!["a.txt"]);
    }

    #[test]
    fn on_off_and_the_directory_default_are_recursive() {
        let opts = options(&["on"]);
        assert!(opts.sub == Subcommand::On);
        assert!(opts.recursive);
        assert!(opts.actions.is_empty(), "on carries no action");
        assert!(opts.names.is_empty());
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = options(&["add", "--", "-a"]);
        assert_eq!(opts.names, vec!["-a"]);
    }
}
