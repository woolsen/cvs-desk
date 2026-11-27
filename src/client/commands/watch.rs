//! `cvs watch`: turn watch mode on or off, or change which actions are watched.
//!
//! The subcommand comes first, before every option: `watch on -l file`, never
//! `watch -l on file`. `on` and `off` carry no action of their own; `add` and `remove` name
//! actions with `-a`, and a command with no `-a` watches all three.
//!
//! `-a` is a set of flags, not a list: `all` sets all three, `none` clears the ones named so
//! far, and repeated flags collapse. The request always sends them in the reference's fixed
//! order `edit`, `unedit`, `commit`; when none is set it sends a single `-a none`
//! (`watch.c:322-329`).
//!
//! The files are sent as the `status` blocks: one `Directory`/`Entry`/state group per folder,
//! then the names as `Argument`s, then the request. `-l` goes to the server as an `Argument`;
//! `-R` is the default and stays on the client. `Argument --` ends the options on the wire
//! before any folder block (`watch.c:330`, `edit.c:103`).
//!
//! There is no `nochange` response and no `.cvswatch` file: 1.11.23 keeps watch state in the
//! repository's `CVS/fileattr`, and neither token exists in its sources.

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

#[derive(Debug, Default)]
struct Actions {
    edit: bool,
    unedit: bool,
    commit: bool,
    /// At least one `-a` was named; without one the reference watches all three.
    named: bool,
}

impl Actions {
    fn set_all(&mut self) {
        self.edit = true;
        self.unedit = true;
        self.commit = true;
    }

    fn apply(&mut self, action: &str) {
        self.named = true;
        match action {
            "edit" => self.edit = true,
            "unedit" => self.unedit = true,
            "commit" => self.commit = true,
            "all" => self.set_all(),
            "none" => {
                self.edit = false;
                self.unedit = false;
                self.commit = false;
            }
            _ => {}
        }
    }

    /// The `-a` values in the reference's fixed order, or `none` when every flag is clear
    /// (`watch.c:322-329`).
    fn wire(&self) -> Vec<&'static str> {
        let mut out = Vec::with_capacity(3);
        if self.edit {
            out.push("edit");
        }
        if self.unedit {
            out.push("unedit");
        }
        if self.commit {
            out.push("commit");
        }
        if out.is_empty() {
            out.push("none");
        }
        out
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
    actions: Actions,
    names: Vec<String>,
}

/// One parsed command line, or the usage the reference prints instead.
enum Parsed {
    Options(Options),
    /// The usage, with the complaint line that goes before it when there is one.
    Usage(Option<String>),
}

/// The reference's `getopt ("+lRa:")` for add/remove and `getopt ("+lR")` for on/off
/// (`watch.c:263`, `edit.c:75`): options stop at the first name, short options cluster
/// (`-lR`), and a value ends its cluster (`-aedit`) or is the next argument (`-a edit`).
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
        actions: Actions::default(),
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
        let cluster: Vec<char> = arg[1..].chars().collect();
        let mut j = 0;
        while j < cluster.len() {
            match cluster[j] {
                'l' => {
                    opts.recursive = false;
                    j += 1;
                }
                'R' => {
                    opts.recursive = true;
                    j += 1;
                }
                'a' if sub.takes_actions() => {
                    // The value is the rest of the cluster (`-aedit`) or the next argument.
                    let value: String = if j + 1 < cluster.len() {
                        cluster[j + 1..].iter().collect()
                    } else {
                        i += 1;
                        match args.get(i) {
                            Some(value) => value.clone(),
                            None => {
                                return Parsed::Usage(Some(format!(
                                    "{}: option requires an argument -- a",
                                    sub.name()
                                )))
                            }
                        }
                    };
                    if action_letters(&value).is_none() {
                        return Parsed::Usage(None);
                    }
                    opts.actions.apply(&value);
                    j = cluster.len();
                }
                other => {
                    return Parsed::Usage(Some(format!(
                        "{}: invalid option -- {}",
                        sub.name(),
                        other
                    )))
                }
            }
        }
        i += 1;
    }
    if sub.takes_actions() && !opts.actions.named {
        // The reference watches every action when no `-a` is named (`watch.c:305-310`).
        opts.actions.set_all();
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

    // Watch changes only the repository's file attributes. There is no local write for `-n`
    // to suppress; the request still goes out, and `Session::start` has already sent
    // `Global_option -n` so the server leaves its own disk alone.
    let mut session = protocol::open(root, "watch", cancel, sink)?;
    if !opts.recursive {
        session.argument("-l")?;
    }
    if opts.sub.takes_actions() {
        for action in opts.actions.wire() {
            session.argument("-a")?;
            session.argument(action)?;
        }
    }
    // `Argument --` and `Max-dotdot` must precede every folder block (`watch.c:330`).
    targets::begin_files(&mut session, &opts.names)?;
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

        let line = usage(&["off", "-aedit", "a.txt"]).expect("a complaint first");
        assert!(line.starts_with("off: invalid option -- a"), "{}", line);
    }

    #[test]
    fn a_missing_action_value_complains_before_the_usage() {
        let line = usage(&["add", "-la"]).expect("a complaint first");
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
    fn short_options_may_be_clustered() {
        let opts = options(&["on", "-lR", "a.txt"]);
        assert!(opts.recursive, "-R after -l turns recursion back on");
        assert_eq!(opts.names, vec!["a.txt"]);

        let opts = options(&["add", "-aedit", "a.txt"]);
        assert_eq!(opts.actions.wire(), vec!["edit"]);

        let opts = options(&["remove", "-lRa", "unedit", "a.txt"]);
        assert!(opts.recursive);
        assert_eq!(opts.actions.wire(), vec!["unedit"]);
    }

    #[test]
    fn a_value_ends_its_cluster() {
        let opts = options(&["add", "-aedit", "-l", "a.txt"]);
        assert!(!opts.recursive);
        assert_eq!(opts.actions.wire(), vec!["edit"]);
    }

    #[test]
    fn add_without_an_action_watches_all_three() {
        let opts = options(&["add", "a.txt"]);
        assert_eq!(opts.actions.wire(), vec!["edit", "unedit", "commit"]);
        assert!(opts.recursive);
    }

    #[test]
    fn actions_keep_the_fixed_e_u_c_order() {
        let opts = options(&["remove", "-a", "commit", "-a", "edit", "a.txt"]);
        assert_eq!(opts.actions.wire(), vec!["edit", "commit"]);
    }

    #[test]
    fn all_expands_and_none_clears_what_was_named() {
        let opts = options(&["add", "-a", "all", "a.txt"]);
        assert_eq!(opts.actions.wire(), vec!["edit", "unedit", "commit"]);

        let opts = options(&["add", "-a", "all", "-a", "none", "a.txt"]);
        assert_eq!(opts.actions.wire(), vec!["none"]);

        let opts = options(&["add", "-a", "edit", "-a", "none", "-a", "unedit", "a.txt"]);
        assert_eq!(opts.actions.wire(), vec!["unedit"]);
    }

    #[test]
    fn an_explicit_none_is_sent_even_without_other_actions() {
        let opts = options(&["add", "-a", "none", "a.txt"]);
        assert_eq!(opts.actions.wire(), vec!["none"]);
    }

    #[test]
    fn repeated_actions_collapse_into_one_flag() {
        let opts = options(&["add", "-a", "edit", "-a", "edit", "a.txt"]);
        assert_eq!(opts.actions.wire(), vec!["edit"]);
    }

    #[test]
    fn on_off_and_the_directory_default_are_recursive() {
        let opts = options(&["on"]);
        assert!(opts.sub == Subcommand::On);
        assert!(opts.recursive);
        assert!(
            !opts.actions.edit && !opts.actions.unedit && !opts.actions.commit,
            "on carries no action"
        );
        assert!(opts.names.is_empty());
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = options(&["add", "--", "-a"]);
        assert_eq!(opts.names, vec!["-a"]);

        let opts = options(&["on", "--", "-l"]);
        assert!(opts.recursive);
        assert_eq!(opts.names, vec!["-l"]);
    }

    #[test]
    fn a_name_ends_the_options() {
        let opts = options(&["add", "a.txt", "-l"]);
        assert!(opts.recursive);
        assert_eq!(opts.names, vec!["a.txt", "-l"]);

        let opts = options(&["on", "-", "a.txt"]);
        assert_eq!(opts.names, vec!["-", "a.txt"]);
    }
}
