//! `cvs add`: schedule new files and folders in `CVS/Entries`.
//!
//! The request is the reference client's: the `-k`/`-m` arguments first, one `Directory`
//! block per folder (the invocation folder first when a file is named), an `Is-modified` line
//! for each file, the names as `Argument`, then `add`. The server answers with `Checked-in`
//! for each file it scheduled; a new folder also gets its own `CVS/Root`, `CVS/Repository`
//! and an `Entries` holding the `D` marker.
//!
//! Each file is registered locally with revision `0` and the `dummy timestamp` marker, the
//! way CVS does, and the index is only written once the server has said `ok`: a failed `add`
//! leaves the working copy as it was.

use std::path::Path;

use crate::client::cli;
use crate::client::commands::targets::{self, AddPlan};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, Response};
use crate::client::root::Root;
use crate::client::work::entries::{Entries, Entry};

#[derive(Debug, Default)]
struct Options {
    /// The RCS keyword flag without its `-k`: `b` or `u` for the desktop program.
    kflag: Option<String>,
    message: Option<String>,
    names: Vec<String>,
}

/// `add` stops taking options at the first name, as getopt does with `+`: everything after
/// that is a name, even when it starts with `-`.
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
            "-k" => {
                i += 1;
                opts.kflag = Some(require(args, i, "-k")?);
            }
            "-m" => {
                i += 1;
                opts.message = Some(require(args, i, "-m")?);
            }
            other if other.starts_with("-k") && other.len() > 2 => {
                opts.kflag = Some(other[2..].to_string())
            }
            other if other.starts_with("-m") && other.len() > 2 => {
                opts.message = Some(other[2..].to_string())
            }
            other => {
                return Err(Error::plain(
                    "add",
                    format!("invalid option -- '{}'", other),
                ))
            }
        }
        i += 1;
    }
    Ok(opts)
}

fn require(args: &[String], i: usize, option: &str) -> Result<String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| Error::aborted("add", format!("option {} requires an argument", option)))
}

/// Run `cvs add`.
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
            "add",
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    if opts.names.is_empty() {
        // The reference prints the command's usage and exits 1, without a prefix.
        if let Some(text) = cli::command_help("add") {
            sink.err(text.to_string());
        }
        return Ok(1);
    }

    let options = match &opts.kflag {
        Some(flag) => format!("-k{}", flag),
        None => String::new(),
    };
    let mut plan = targets::collect_add(cwd, &opts.names, &options)?;

    let mut session = protocol::open(root, "add", cancel, sink)?;
    if let Some(flag) = &opts.kflag {
        session.argument(&format!("-k{}", flag))?;
    }
    if let Some(message) = &opts.message {
        session.argument("-m")?;
        session.argument(message)?;
    }
    targets::send_add(&mut session, &plan)?;
    targets::finish_arguments(&mut session, &opts.names, "add")?;

    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::CheckedIn {
                update_dir, entry, ..
            } => {
                record_checked_in(&mut plan, &update_dir, &entry)?;
            }
            Response::Error { message, .. } => {
                // Nothing has been written yet, so a failed add leaves the index alone.
                session.close();
                return Err(Error::aborted(
                    "add",
                    if message.trim().is_empty() {
                        "the server reported an error".to_string()
                    } else {
                        message
                    },
                ));
            }
            Response::Mode(_)
            | Response::NewEntry(_)
            | Response::Removed(_)
            | Response::RemoveEntry(_)
            | Response::CopyFile { .. }
            | Response::ModTime(_)
            | Response::File { .. }
            | Response::SetStaticDirectory { .. }
            | Response::ClearStaticDirectory { .. }
            | Response::SetSticky { .. }
            | Response::ClearSticky { .. }
            | Response::F(_)
            | Response::ValidRequests(_)
            | Response::ModuleExpansion(_)
            | Response::Notified
            | Response::Trace(_)
            | Response::Checksum(_)
            | Response::Template { .. } => {}
        }
    }

    write_plan(&plan, &session.root)?;
    session.close();
    Ok(0)
}

/// Apply the entry the server sent back. An empty date means "still to be committed", which
/// CVS writes as `dummy timestamp`.
fn record_checked_in(plan: &mut AddPlan, update_dir: &str, entry_line: &str) -> Result<()> {
    let mut entry = Entry::parse(entry_line).ok_or_else(|| {
        Error::aborted(
            "add",
            format!(
                "the server sent an entry this client cannot read: {}",
                entry_line
            ),
        )
    })?;
    if entry.date.is_empty() {
        entry.date = "dummy timestamp".to_string();
    }
    if let Some(target) = plan.targets.get_mut(&targets::target_key(update_dir)) {
        target.entries.set_file(entry);
    }
    Ok(())
}

/// Write the index now that the server accepted the command: first the new folders' own
/// `CVS/` files, then the parent index with its `D/…` line and the added entries.
fn write_plan(plan: &AddPlan, root: &Root) -> Result<()> {
    for dir in &plan.new_dirs {
        dir.ensure(root)?;
        dir.write_entries(&Entries::parse("D"))?;
    }
    for target in plan.targets.values() {
        target.dir.write_entries(&target.entries)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_attached_kflag_is_read_as_one_argument() {
        let opts = parse_args(&["-kb".to_string(), "bin.dat".to_string()]).unwrap();
        assert_eq!(opts.kflag.as_deref(), Some("b"));
        assert_eq!(opts.names, vec!["bin.dat"]);
    }

    #[test]
    fn a_message_is_two_words_and_the_names_stay_in_order() {
        let opts = parse_args(&[
            "-m".to_string(),
            "creation log".to_string(),
            "a.txt".to_string(),
            "b.txt".to_string(),
        ])
        .unwrap();
        assert_eq!(opts.message.as_deref(), Some("creation log"));
        assert_eq!(opts.names, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn names_after_the_first_one_are_not_options() {
        let opts = parse_args(&["dir1".to_string(), "-kb".to_string()]).unwrap();
        assert!(opts.kflag.is_none());
        assert_eq!(opts.names, vec!["dir1", "-kb"]);
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let error = parse_args(&["-Z".to_string()]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
    }
}
