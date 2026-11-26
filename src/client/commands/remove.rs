//! `cvs remove`: schedule files for deletion from the repository.
//!
//! The request repeats the folder blocks and file states `update` sends, then `remove`. With
//! `-f` the working file is deleted first, so no state line follows its `Entry`; without `-f`
//! the server itself refuses while the file is still on disk (`1 file exists; remove it
//! first`), which is the reference's behaviour and not a client-side failure.
//!
//! A successful removal returns `Checked-in` with the old revision negated. The local entry
//! keeps that revision and the `dummy timestamp` marker, so `commit` still sees a change to
//! send.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::commands::targets::{self, Target};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, Response};
use crate::client::root::Root;
use crate::client::work::dir;
use crate::client::work::entries::Entry;

#[derive(Debug, Default)]
struct Options {
    force: bool,
    recursive: bool,
    names: Vec<String>,
}

/// Like `add`, `remove` stops taking options at the first name.
fn parse_args(args: &[String]) -> Result<Options> {
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
        if !arg.starts_with('-') || arg == "-" {
            opts.names.extend(args[i..].iter().cloned());
            break;
        }
        match arg.as_str() {
            "-f" => opts.force = true,
            "-l" => opts.recursive = false,
            "-R" => opts.recursive = true,
            other => {
                return Err(Error::plain(
                    "remove",
                    format!("invalid option -- '{}'", other),
                ))
            }
        }
        i += 1;
    }
    Ok(opts)
}

/// Run `cvs remove`.
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
            "remove",
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    let mut targets = targets::collect_remove(cwd, opts.recursive, &opts.names)?;
    if !opts.names.is_empty() && targets.is_empty() {
        return Err(Error::aborted("remove", "no files to remove"));
    }

    let mut session = protocol::open(root, "remove", cancel, sink)?;
    if !opts.recursive {
        session.argument("-l")?;
    }
    if opts.force {
        delete_working_files(&targets)?;
    }
    // Unknown files are only wondered about in a folder the command selected as a whole;
    // `send_blocks_tracked` already skips folders selected by a file name.
    let last = targets::send_blocks_tracked(&mut session, &targets, true)?;
    targets::finish_from_last(&mut session, cwd, &opts.names, "remove", last.as_deref())?;

    let mut changed = false;
    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::CheckedIn {
                update_dir, entry, ..
            } => {
                changed |= record_checked_in(&mut targets, &update_dir, &entry)?;
            }
            Response::RemoveEntry(path) => {
                let (dir, name) = targets::split_local(&path);
                if let Some(target) = targets.get_mut(&dir) {
                    target.entries.remove_file(&name);
                    changed = true;
                }
            }
            Response::Removed(path) => {
                let (dir, name) = targets::split_local(&path);
                if let Some(target) = targets.get_mut(&dir) {
                    let local = target.dir.local.join(&name);
                    match std::fs::remove_file(&local) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                    target.entries.remove_file(&name);
                    changed = true;
                }
            }
            Response::Error { message, .. } => {
                if message.trim().is_empty() && changed {
                    // Some CVSNT servers end a successful removal with an empty error.
                    break;
                }
                session.close();
                return Err(Error::aborted(
                    "remove",
                    if message.trim().is_empty() {
                        "the server reported an error".to_string()
                    } else {
                        message
                    },
                ));
            }
            Response::Mode(_)
            | Response::NewEntry(_)
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

    if changed {
        for target in targets.values() {
            target.dir.write_entries(&target.entries)?;
        }
    }
    session.close();
    Ok(0)
}

/// Apply the server's `Checked-in` line. An empty date becomes `dummy timestamp`, exactly as
/// it does for `add`; the rest of the entry is the server's.
fn record_checked_in(
    targets: &mut BTreeMap<String, Target>,
    update_dir: &str,
    entry_line: &str,
) -> Result<bool> {
    let mut entry = Entry::parse(entry_line).ok_or_else(|| {
        Error::aborted(
            "remove",
            format!(
                "the server sent an entry this client cannot read: {}",
                entry_line
            ),
        )
    })?;
    if entry.date.is_empty() {
        entry.date = "dummy timestamp".to_string();
    }
    let Some(target) = targets.get_mut(&targets::target_key(update_dir)) else {
        return Ok(false);
    };
    target.entries.set_file(entry);
    Ok(true)
}

/// `-f`: delete the working files before the request, so the server sees them gone. CVS keeps
/// checked-out files read-only, so the bit is cleared first.
fn delete_working_files(targets: &BTreeMap<String, Target>) -> Result<()> {
    for target in targets.values() {
        for entry in target.entries.files() {
            if !target.selected && !target.filter.iter().any(|name| name == &entry.name) {
                continue;
            }
            delete_working_file(&target.dir, &entry.name)?;
        }
        for name in &target.loose {
            delete_working_file(&target.dir, name)?;
        }
    }
    Ok(())
}

fn delete_working_file(dir: &dir::WorkingDir, name: &str) -> Result<()> {
    let local = dir.local.join(name);
    dir::clear_readonly(&local)?;
    match std::fs::remove_file(&local) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f_and_l_are_read_and_r_is_the_default_again() {
        let opts = parse_args(&[
            "-f".to_string(),
            "-l".to_string(),
            "-R".to_string(),
            "a.txt".to_string(),
        ])
        .unwrap();
        assert!(opts.force);
        assert!(opts.recursive);
        assert_eq!(opts.names, vec!["a.txt"]);
    }

    #[test]
    fn a_name_then_a_dash_word_is_a_name() {
        let opts = parse_args(&["a.txt".to_string(), "-f".to_string()]).unwrap();
        assert!(!opts.force);
        assert_eq!(opts.names, vec!["a.txt", "-f"]);
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let error = parse_args(&["-Z".to_string()]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
    }
}
