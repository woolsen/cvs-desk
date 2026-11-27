//! `cvs remove`: schedule files for deletion from the repository.
//!
//! The request repeats the folder blocks and file states `update` sends, then `remove`. With
//! `-f` the working file is deleted first, so no state line follows its `Entry`; without `-f`
//! the server itself refuses while the file is still on disk (`1 file exists; remove it
//! first`), which is the reference's behaviour and not a client-side failure. `-n` deletes
//! nothing and writes nothing.
//!
//! A successful removal returns `Checked-in` with the old revision negated. The local entry
//! keeps that revision and the `dummy timestamp` marker, so `commit` still sees a change to
//! send.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use crate::client::commands::targets::{self, Target};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, Response};
use crate::client::root::Root;
use crate::client::work::dir;
use crate::client::work::entries::Entry;
use crate::client::work::glob;

#[derive(Debug, Default)]
struct Options {
    force: bool,
    recursive: bool,
    names: Vec<String>,
}

/// `remove` stops taking options at the first name. The reference's getopt string is `+flR`,
/// so `-fl`, `-fR` and `-flR` are legal and mean the same as the separate options.
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
        for letter in arg[1..].chars() {
            match letter {
                'f' => opts.force = true,
                'l' => opts.recursive = false,
                'R' => opts.recursive = true,
                other => {
                    return Err(Error::plain(
                        "remove",
                        format!("invalid option -- '{}'", other),
                    ))
                }
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
    // The reference remote client has no working-copy check of its own: each name is looked
    // up where it lives (`remove.c:84-116`). Only a bare `remove` needs the current folder.
    if opts.names.is_empty() && !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "remove",
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    let no_execute = root.options.no_execute;
    let names = glob::expand_args(cwd, &opts.names);
    let mut targets = targets::collect_remove(cwd, opts.recursive, &names)?;
    if !opts.names.is_empty() && targets.is_empty() {
        return Err(Error::aborted("remove", "no files to remove"));
    }
    // A `../other/file` name is numbered from the invocation folder, not from its own folder
    // (`client.c:5562-5591` accepts the `..` after `Max-dotdot`).
    rekey_outside_dirs(cwd, &mut targets);
    // With no names the reference only wonders about files inside subdirectories; the
    // top-level folder is not entered by `send_dirent_proc` (recurse.c:677-678,793-796).
    if opts.names.is_empty() {
        keep_top_entries_only(&mut targets);
    }

    let mut session = protocol::open(root, "remove", cancel, sink)?;
    if !opts.recursive {
        session.argument("-l")?;
    }
    // `Argument --` and `Max-dotdot` go before any folder block (remove.c:105-111;
    // client.c:5562-5591).
    targets::begin_files(&mut session, &names)?;
    if opts.force && !no_execute {
        delete_working_files(&targets)?;
    }
    // Unknown files are sent as `Questionable`, which is also how the server prints `?`;
    // printing them here as well would duplicate every line (client.c:5226-5243).
    let last = targets::send_blocks_quiet(&mut session, &targets)?;
    targets::finish_from_last(&mut session, cwd, &names, "remove", last.as_deref())?;

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
                    if !no_execute {
                        let local = target.dir.local.join(&name);
                        match std::fs::remove_file(&local) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    target.entries.remove_file(&name);
                    changed = true;
                }
            }
            Response::Error { message, .. } => {
                // The reference closes the index after every response (client.c:1393-1421);
                // a later fatal error must not throw away the removals already confirmed.
                if changed {
                    write_targets(&targets, no_execute)?;
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
            | Response::NewEntry { .. }
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
        write_targets(&targets, no_execute)?;
    }
    session.close();
    Ok(0)
}

/// Apply the server's `Checked-in` line. The server writes the old revision negated and the
/// conflict marker in the timestamp field; CVS forces `dummy timestamp` for every negative
/// revision, whatever the server sent (`client.c:2256-2262`, `server.c:3653-3657`).
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
    if entry.revision.is_empty() || entry.revision == "0" || entry.revision.starts_with('-') {
        entry.date = "dummy timestamp".to_string();
    }
    let Some(target) = targets.get_mut(&targets::target_key(update_dir)) else {
        return Ok(false);
    };
    target.entries.set_file(entry);
    Ok(true)
}

fn write_targets(targets: &BTreeMap<String, Target>, no_execute: bool) -> Result<()> {
    if no_execute {
        return Ok(());
    }
    for target in targets.values() {
        target.dir.write_entries(&target.entries)?;
    }
    Ok(())
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

/// Numbers every folder that lies outside the invocation folder from that folder, so
/// `remove ../other/f` sends `Directory ../other` rather than `Directory .`.
fn rekey_outside_dirs(cwd: &Path, targets: &mut BTreeMap<String, Target>) {
    let mut moved = Vec::new();
    for key in targets.keys().cloned().collect::<Vec<_>>() {
        let Some(target) = targets.get(&key) else {
            continue;
        };
        let Some(relative) = relative_dir(cwd, &target.dir.local) else {
            continue;
        };
        if relative != target.dir.update_dir {
            let mut target = targets.remove(&key).expect("the key was just read");
            target.dir.update_dir = relative.clone();
            moved.push((relative, target));
        }
    }
    for (key, target) in moved {
        targets.insert(key, target);
    }
}

/// `cwd` and a folder below or beside it as a `/`-separated relative path (`..` allowed).
fn relative_dir(cwd: &Path, local: &Path) -> Option<String> {
    let base: Vec<Component> = cwd.components().collect();
    let target: Vec<Component> = local.components().collect();
    let mut common = 0;
    while common < base.len() && common < target.len() && base[common] == target[common] {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..base.len() {
        parts.push("..".to_string());
    }
    for component in &target[common..] {
        match component {
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
            Component::ParentDir => parts.push("..".to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// Send the list of entries but not the `Questionable` names for the invocation folder.
fn keep_top_entries_only(targets: &mut BTreeMap<String, Target>) {
    if let Some(top) = targets.get_mut("") {
        if top.selected {
            top.filter = top
                .entries
                .files()
                .map(|entry| entry.name.clone())
                .collect();
            top.selected = false;
        }
    }
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
    fn clustered_short_options_are_the_same_options() {
        let opts = parse_args(&["-flR".to_string(), "a.txt".to_string()]).unwrap();
        assert!(opts.force);
        assert!(opts.recursive);
        let opts = parse_args(&["-fl".to_string(), "a.txt".to_string()]).unwrap();
        assert!(opts.force);
        assert!(!opts.recursive);
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let error = parse_args(&["-Z".to_string()]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
        let error = parse_args(&["-fZ".to_string()]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
    }

    #[test]
    fn an_outside_folder_is_numbered_with_dotdot() {
        let cwd = Path::new("C:\\work\\mod");
        assert_eq!(
            relative_dir(cwd, Path::new("C:\\work\\mod\\sub")),
            Some("sub".to_string())
        );
        assert_eq!(
            relative_dir(cwd, Path::new("C:\\work\\mod\\..\\other")),
            Some("../other".to_string())
        );
    }
}
