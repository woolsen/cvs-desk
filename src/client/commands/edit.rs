//! `cvs edit`: make a watched file writable and tell the server it is being edited.
//!
//! There is no `edit` request on the wire. The client names the folder, then sends one
//! `Notify` per file with the raw notification line, and finishes with `noop`; the server
//! hands the notice to the file's watchers. The local side effect is the read-only bit: `edit`
//! makes the file writable so the user can work on it.
//!
//! `-c` is the careful form. It first asks the server who is already editing (`editors`) and
//! stops with `files being edited!` when the answer names anyone; only a clean answer goes on
//! to the notification and the writable bit.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::commands::targets::{self, Target};
use crate::client::commands::{notify, watch};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, Response};
use crate::client::root::Root;
use crate::client::work::dir;
use crate::client::work::entries::Entry;

#[derive(Debug)]
struct Options {
    /// `-c`: check with the server that nobody else is editing before going ahead.
    check: bool,
    /// `-f`: the default, kept so a script that names it keeps working.
    force: bool,
    recursive: bool,
    /// The `-a` actions, which become the watches field of the notification.
    actions: Vec<String>,
    names: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            check: false,
            force: false,
            recursive: true,
            actions: Vec::new(),
            names: Vec::new(),
        }
    }
}

/// Like `add` and `remove`, `edit` stops taking options at the first name. `-c` and `-f` are
/// opposites; the last one wins, as it does for a script that names both.
fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options::default();
    let mut i = 0;
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
            "-c" => {
                opts.check = true;
                opts.force = false;
            }
            "-f" => {
                opts.force = true;
                opts.check = false;
            }
            "-l" => opts.recursive = false,
            "-R" => opts.recursive = true,
            "-a" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| Error::aborted("edit", "option -a requires an argument"))?;
                if watch::action_letters(value).is_none() {
                    return Err(Error::plain("edit", format!("invalid action '{}'", value)));
                }
                opts.actions.push(value.clone());
            }
            other => {
                return Err(Error::plain(
                    "edit",
                    format!("invalid option -- '{}'", other),
                ))
            }
        }
        i += 1;
    }
    Ok(opts)
}

/// The watches field of the notification: the letters for the `-a` actions, or all three
/// (`EUC`) when the user named none, which is what the reference sends.
fn watches(opts: &Options) -> String {
    let mut letters = String::new();
    for action in &opts.actions {
        if let Some(text) = watch::action_letters(action) {
            for letter in text.chars() {
                if !letters.contains(letter) {
                    letters.push(letter);
                }
            }
        }
    }
    if letters.is_empty() && opts.actions.is_empty() {
        return "EUC".to_string();
    }
    letters
}

/// Run `cvs edit`.
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
            "edit",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    let files: Vec<(&Target, &Entry)> = notify::selected(&targets)
        .into_iter()
        .filter(|(target, entry)| target.dir.local.join(&entry.name).is_file())
        .collect();
    if files.is_empty() {
        return Err(Error::aborted("edit", "no files to edit"));
    }

    let mut session = protocol::open(root, "edit", cancel, sink)?;
    if opts.check && !check_edited(&mut session, &targets, &files, &opts)? {
        session.close();
        return Err(Error::aborted("edit", "files being edited!"));
    }

    let watches = watches(&opts);
    let mut notices = Vec::new();
    for (target, entry) in &files {
        let local = target.dir.local.join(&entry.name);
        // The base is what `unedit` puts back when the user says to revert.
        crate::client::work::base::save(&target.dir.local, &entry.name, &entry.revision)?;
        dir::clear_readonly(&local).ok();
        notices.push(notify::Notice {
            dir: target.dir.update_dir.clone(),
            name: entry.name.clone(),
            kind: 'E',
            watches: watches.clone(),
        });
    }
    notify::send(&mut session, &targets, &notices, cwd)?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

/// `-c`: send the file blocks and one `editors` request per file. Returns true when nobody
/// else is editing; false has already sent the closing `noop` and leaves the stop to the
/// caller.
fn check_edited(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
    files: &[(&Target, &Entry)],
    opts: &Options,
) -> Result<bool> {
    if !opts.recursive {
        session.argument("-l")?;
    }
    let _ = targets::send_blocks_watch(session, targets)?;
    for (target, entry) in files {
        let name = dir::display_path(&target.dir.update_dir, &entry.name);
        session.argument(&name)?;
        session.send("editors")?;
    }
    session.flush()?;

    let mut edited = false;
    for _ in 0..files.len() {
        loop {
            match session.read_response()? {
                Response::Ok => break,
                Response::M(text) => {
                    edited = true;
                    session.out(text);
                }
                Response::E(text) => session.err(text),
                Response::Notified | Response::Trace(_) | Response::Checksum(_) => {}
                Response::Error { message, .. } => {
                    return Err(Error::aborted(
                        "edit",
                        if message.trim().is_empty() {
                            "the server reported an error".to_string()
                        } else {
                            message
                        },
                    ))
                }
                other => {
                    return Err(Error::aborted(
                        "edit",
                        format!("unexpected answer: {:?}", other),
                    ))
                }
            }
        }
    }
    if edited {
        // The reference has already queued its closing `noop` when it gives up. Read its
        // answer too: a server that is left with an unread reply does not always notice the
        // pipe close on Windows.
        session.send("noop")?;
        session.flush()?;
        let _ = session.read_to_ok();
        return Ok(false);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn the_flags_are_read_until_the_first_name() {
        let opts = parse(&["-c", "-l", "-R", "a.txt"]).unwrap();
        assert!(opts.check);
        assert!(opts.recursive, "-R after -l turns recursion back on");
        assert_eq!(opts.names, vec!["a.txt"]);
    }

    #[test]
    fn the_last_of_c_and_f_wins() {
        let opts = parse(&["-c", "-f"]).unwrap();
        assert!(!opts.check);
        assert!(opts.force);

        let opts = parse(&["-f", "-c"]).unwrap();
        assert!(opts.check);
        assert!(!opts.force);
    }

    #[test]
    fn an_action_may_be_repeated_and_is_kept_in_order() {
        let opts = parse(&["-a", "unedit", "-a", "edit", "a.txt"]).unwrap();
        assert_eq!(opts.actions, vec!["unedit", "edit"]);
        assert_eq!(watches(&opts), "UE");
    }

    #[test]
    fn no_action_means_all_three_watches() {
        let opts = parse(&["a.txt"]).unwrap();
        assert_eq!(watches(&opts), "EUC");
    }

    #[test]
    fn a_name_then_a_dash_word_is_a_name() {
        let opts = parse(&["a.txt", "-c"]).unwrap();
        assert!(!opts.check);
        assert_eq!(opts.names, vec!["a.txt", "-c"]);
    }

    #[test]
    fn an_unknown_option_or_action_is_refused() {
        assert!(parse(&["-Z"])
            .unwrap_err()
            .text()
            .contains("invalid option"));
        assert!(parse(&["-a", "bogus"])
            .unwrap_err()
            .text()
            .contains("invalid action"));
    }

    #[test]
    fn a_missing_action_is_refused() {
        let error = parse(&["-a"]).unwrap_err();
        assert!(
            error.text().contains("option -a requires an argument"),
            "{}",
            error
        );
    }
}
