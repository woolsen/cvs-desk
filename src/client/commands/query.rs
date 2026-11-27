//! Read-only queries the server answers with text: `annotate`, `editors`, `watchers`.
//!
//! They share `status`'s folder blocks: each folder, then each file's state, then the command
//! from the invocation folder. `annotate` additionally takes the revision options; `editors`
//! and `watchers` share `-l`/`-R`. All three recurse into the current folder when no name is
//! given, exactly as the reference client does.
//!
//! The file contents are never needed (`SEND_NO_CONTENTS`): the server only has to know
//! whether a file is modified, so a changed file is sent as `Is-modified` when the server
//! lists that request, with the reference's `Modified`-with-contents fallback otherwise.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::commands::date;
use crate::client::commands::targets::{self, Target};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;
use crate::client::work::dir::{self, WorkingDir};
use crate::client::work::entries::Entries;
use crate::client::work::glob;

#[derive(Debug, Default)]
struct AnnotateOptions {
    /// `-l`: only the invocation folder, no recursion; also sent as `Argument -l`.
    local: bool,
    /// `-f`: fall back to the head revision when the tag or date is not found.
    force: bool,
    /// `-F`: annotate binary files too. The server reads the RCS expand flag and skips
    /// binary files only when this is missing.
    binary: bool,
    revision: Option<String>,
    date: Option<String>,
    names: Vec<String>,
}

fn parse_annotate(args: &[String]) -> Result<AnnotateOptions> {
    let mut opts = AnnotateOptions::default();
    let names = parse_short_options("annotate", args, &['r', 'D'], |letter, value| {
        match letter {
            'l' => opts.local = true,
            'R' => opts.local = false,
            'f' => opts.force = true,
            'F' => opts.binary = true,
            'r' => opts.revision = value,
            'D' => opts.date = value,
            other => return Err(invalid_option("annotate", &format!("-{}", other))),
        }
        Ok(())
    })?;
    opts.names = names;
    Ok(opts)
}

#[derive(Debug, Default)]
struct QueryOptions {
    /// `-l`: only the invocation folder, no recursion; also sent as `Argument -l`.
    local: bool,
    names: Vec<String>,
}

fn parse_query(command: &str, args: &[String]) -> Result<QueryOptions> {
    let mut opts = QueryOptions::default();
    let names = parse_short_options(command, args, &[], |letter, _| match letter {
        'l' => {
            opts.local = true;
            Ok(())
        }
        'R' => {
            opts.local = false;
            Ok(())
        }
        other => Err(invalid_option(command, &format!("-{}", other))),
    })?;
    opts.names = names;
    Ok(opts)
}

/// `getopt("+…")`: the first name ends the options, and short options may be clustered.
/// A letter in `takes_value` consumes the rest of its cluster (`-r1.1`) or the next argument
/// (`-r 1.1`); every other letter is passed to `apply` as a flag.
fn parse_short_options(
    command: &str,
    args: &[String],
    takes_value: &[char],
    mut apply: impl FnMut(char, Option<String>) -> Result<()>,
) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            names.extend(args[i + 1..].iter().cloned());
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            // As with `getopt("+…")`, a name stops option parsing: `annotate a.txt -l` names
            // both `a.txt` and `-l`, as the reference does.
            names.extend(args[i..].iter().cloned());
            break;
        }
        if arg.starts_with("--") {
            return Err(invalid_option(command, arg));
        }
        let body = &arg[1..];
        for (position, letter) in body.char_indices() {
            if takes_value.contains(&letter) {
                let attached = &body[position + letter.len_utf8()..];
                let value = if attached.is_empty() {
                    i += 1;
                    args.get(i).cloned().ok_or_else(|| {
                        Error::aborted(command, format!("option -{} requires an argument", letter))
                    })?
                } else {
                    attached.to_string()
                };
                apply(letter, Some(value))?;
                // A value ends the cluster: `-r1.1f` is revision `1.1f`, not `-f`.
                break;
            }
            apply(letter, None)?;
        }
        i += 1;
    }
    Ok(names)
}

fn invalid_option(command: &str, text: &str) -> Error {
    Error::plain(command, format!("invalid option -- '{}'", text))
}

/// Run `cvs annotate`.
pub fn annotate(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = parse_annotate(args)?;
    require_working_copy(cwd, "annotate", sink)?;
    // `send_file_names` expands wildcards on the client, on Windows where cmd does not.
    let names = glob::expand_args(cwd, &opts.names);
    let targets = collect(cwd, !opts.local, &names)?;

    let mut session = protocol::open(root, "annotate", cancel, sink)?;
    // The reference's fixed order, whatever order the user typed them in: `-l`, `-f`, `-F`,
    // `-r rev`, `-D date`, then `--` before the folder blocks.
    if opts.local {
        session.argument("-l")?;
    }
    if opts.force {
        session.argument("-f")?;
    }
    if opts.binary {
        session.argument("-F")?;
    }
    if let Some(revision) = &opts.revision {
        session.argument("-r")?;
        session.argument(revision)?;
    }
    if let Some(when) = &opts.date {
        session.argument("-D")?;
        session.argument(&date::normalize(when))?;
    }
    targets::begin_files(&mut session, &names)?;
    let last = send_no_contents(&mut session, &targets)?;
    targets::finish_from_last(&mut session, cwd, &names, "annotate", last.as_deref())?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

/// Run `cvs editors` or `cvs watchers`. With no names both recurse into the invocation folder;
/// a query with nothing to report still sends the request and exits 0 after the server's `ok`.
pub fn file_query(
    cwd: &Path,
    root: Root,
    command: &str,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = parse_query(command, args)?;
    require_working_copy(cwd, command, sink)?;
    let names = glob::expand_args(cwd, &opts.names);
    let targets = collect(cwd, !opts.local, &names)?;

    let mut session = protocol::open(root, command, cancel, sink)?;
    if opts.local {
        session.argument("-l")?;
    }
    targets::begin_files(&mut session, &names)?;
    let last = send_no_contents(&mut session, &targets)?;
    targets::finish_from_last(&mut session, cwd, &names, command, last.as_deref())?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

/// The reference's two-line complaint when the invocation folder is not a working copy.
fn require_working_copy(cwd: &Path, command: &str, sink: &mut dyn Sink) -> Result<()> {
    if cwd.join("CVS").is_dir() {
        return Ok(());
    }
    sink.err(format!("cvs {}: in directory .:", command));
    Err(Error::aborted(
        command,
        "there is no version here; run 'cvs checkout' first",
    ))
}

/// Work out which folders and files the answer covers.
///
/// Unlike [`targets::collect`], a named file that has no `Entries` line is not refused here:
/// the reference still sends the name and lets the server answer. It stays part of its
/// folder's block, so the folder's other entries are sent as usual.
fn collect(cwd: &Path, recursive: bool, names: &[String]) -> Result<BTreeMap<String, Target>> {
    let mut targets: BTreeMap<String, Target> = BTreeMap::new();
    if names.is_empty() {
        for dir in dir::walk(cwd, recursive)? {
            let entries = dir.entries()?;
            insert(&mut targets, dir, entries, Vec::new());
        }
        return Ok(targets);
    }

    for name in names {
        let path = cwd.join(name);
        if path.is_dir() {
            let prefix = normalize_relative(name);
            for mut dir in dir::walk(&path, recursive)? {
                dir.update_dir = join_update(&prefix, &dir.update_dir);
                let entries = dir.entries()?;
                insert(&mut targets, dir, entries, Vec::new());
            }
            continue;
        }
        let parent = path.parent().unwrap_or(cwd);
        let Some(mut dir) = dir::walk(parent, false)?.into_iter().next() else {
            return Err(Error::aborted(
                "cvs",
                format!("there is no version here for '{}'", name),
            ));
        };
        // `walk(parent)` numbers the folder from the parent; the protocol numbers it from the
        // invocation folder.
        dir.update_dir = path
            .parent()
            .and_then(|folder| folder.strip_prefix(cwd).ok())
            .map(|folder| normalize_relative(&folder.to_string_lossy()))
            .unwrap_or_default();
        let entries = dir.entries()?;
        let file = path
            .file_name()
            .map(|file| file.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());
        insert(&mut targets, dir, entries, vec![file]);
    }
    Ok(targets)
}

fn insert(
    targets: &mut BTreeMap<String, Target>,
    dir: WorkingDir,
    entries: Entries,
    filter: Vec<String>,
) {
    let selected = filter.is_empty();
    let key = dir.update_dir.clone();
    let target = targets.entry(key).or_insert_with(|| Target {
        dir,
        entries,
        filter: Vec::new(),
        selected,
        loose: Vec::new(),
    });
    target.selected |= selected;
    for name in filter {
        if !target.filter.iter().any(|existing| existing == &name) {
            target.filter.push(name);
        }
    }
}

/// Send the folder blocks without file contents (`SEND_NO_CONTENTS`). A changed file is one
/// `Is-modified` line when the server lists the request; an older server gets the reference's
/// fallback, `Modified` with the file's bytes.
fn send_no_contents(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
) -> Result<Option<String>> {
    if session.supports("Is-modified") {
        targets::send_blocks_watch(session, targets)
    } else {
        targets::send_blocks_quiet(session, targets)
    }
}

fn normalize_relative(text: &str) -> String {
    text.replace('\\', "/")
        .trim_matches('/')
        .trim_start_matches("./")
        .to_string()
}

fn join_update(prefix: &str, child: &str) -> String {
    match (prefix.is_empty(), child.is_empty()) {
        (true, _) => child.to_string(),
        (_, true) => prefix.to_string(),
        _ => format!("{}/{}", prefix, child),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn annotate_reads_clustered_flags() {
        let opts = parse_annotate(&args(&["-lf", "a.txt"])).unwrap();
        assert!(opts.local);
        assert!(opts.force);
        assert!(!opts.binary);
        assert_eq!(opts.names, vec!["a.txt".to_string()]);

        let opts = parse_annotate(&args(&["-lRf", "a.txt"])).unwrap();
        assert!(!opts.local);
        assert!(opts.force);

        let opts = parse_annotate(&args(&["-F", "bin"])).unwrap();
        assert!(opts.binary);
    }

    #[test]
    fn annotate_takes_attached_or_separate_values() {
        let opts = parse_annotate(&args(&["-r1.1", "-D", "2020-01-01", "a.txt"])).unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
        assert_eq!(opts.date.as_deref(), Some("2020-01-01"));
        assert_eq!(opts.names, vec!["a.txt".to_string()]);

        // A value ends its cluster: `-r1.1f` is a revision, not a flag.
        let opts = parse_annotate(&args(&["-r1.1f", "a.txt"])).unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.1f"));
        assert!(!opts.force);
    }

    #[test]
    fn the_last_revision_and_date_win() {
        let opts = parse_annotate(&args(&[
            "-r",
            "1.1",
            "-r1.2",
            "-D2020-01-01",
            "-D2021-02-02",
        ]))
        .unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.2"));
        assert_eq!(opts.date.as_deref(), Some("2021-02-02"));
    }

    #[test]
    fn a_missing_value_is_an_error() {
        for option in ["-r", "-D"] {
            let error = parse_annotate(&args(&[option])).unwrap_err();
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
    fn the_first_name_ends_the_options() {
        let opts = parse_annotate(&args(&["a.txt", "-l", "-F"])).unwrap();
        assert!(!opts.local);
        assert!(!opts.binary);
        assert_eq!(
            opts.names,
            vec!["a.txt".to_string(), "-l".to_string(), "-F".to_string()]
        );
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = parse_annotate(&args(&["-f", "--", "-name", "a.txt"])).unwrap();
        assert!(opts.force);
        assert_eq!(opts.names, vec!["-name".to_string(), "a.txt".to_string()]);
    }

    #[test]
    fn annotate_refuses_options_it_does_not_know() {
        let error = parse_annotate(&args(&["-q", "a.txt"])).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);

        let error = parse_annotate(&args(&["--help"])).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
    }

    #[test]
    fn file_query_reads_clustered_local_and_recursive_flags() {
        let opts = parse_query("editors", &args(&["-lR", "a.txt"])).unwrap();
        assert!(!opts.local);
        assert_eq!(opts.names, vec!["a.txt".to_string()]);

        let opts = parse_query("watchers", &args(&["-Rl", "a.txt"])).unwrap();
        assert!(opts.local);

        let opts = parse_query("watchers", &args(&["a.txt", "-l"])).unwrap();
        assert!(!opts.local);
        assert_eq!(opts.names, vec!["a.txt".to_string(), "-l".to_string()]);
    }

    #[test]
    fn file_query_refuses_other_options() {
        let error = parse_query("watchers", &args(&["-f", "a.txt"])).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
    }
}
