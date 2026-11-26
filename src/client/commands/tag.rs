//! `cvs tag`: put a symbolic tag on the revisions checked out in a working copy.
//!
//! The tag name and the flags are `Argument`s before the files; the files themselves are sent
//! as the same `Directory`/`Entry`/state blocks `status` and `log` send. The server does the
//! work and answers `M`/`E` lines, which this client prints as they arrive.
//!
//! `-c` checks that every file that would be tagged is unmodified. The reference's server makes
//! the check, so its refusal names the server; this client makes the same check before the
//! request and keeps that wording. A missing file is not "modified" — there is nothing to tag
//! either way.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::cli;
use crate::client::commands::targets::Target;
use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::dir;
use crate::client::work::entries::Entry;

#[derive(Debug, Default)]
struct Options {
    branch: bool,
    check_modified: bool,
    delete: bool,
    move_tag: bool,
    force: bool,
    recursive: bool,
    revision: Option<String>,
    date: Option<String>,
    tag: String,
    names: Vec<String>,
}

/// `tag` stops taking options at the tag name, as getopt does with `+`: everything after it is
/// a file name, even when it starts with `-`.
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
            "-b" => opts.branch = true,
            "-c" => opts.check_modified = true,
            "-d" => opts.delete = true,
            "-F" => opts.move_tag = true,
            "-f" => opts.force = true,
            "-l" => opts.recursive = false,
            "-R" => opts.recursive = true,
            "-r" | "-D" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    Error::aborted("tag", format!("option {} requires an argument", arg))
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
                    "tag",
                    format!("invalid option -- '{}'", other),
                ))
            }
        }
        i += 1;
    }
    if let Some((tag, names)) = operands.split_first() {
        opts.tag = tag.clone();
        opts.names = names.to_vec();
    }
    Ok(opts)
}

/// Run `cvs tag`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = parse_args(args)?;
    if opts.tag.is_empty() {
        // The reference prints the command's usage and exits 1, without a prefix.
        if let Some(text) = cli::command_help("tag") {
            sink.err(text.to_string());
        }
        return Ok(1);
    }
    if opts.revision.is_some() && opts.date.is_some() {
        return Err(Error::aborted(
            "tag",
            "-r and -D options are mutually exclusive",
        ));
    }
    if opts.branch && opts.delete {
        sink.err("cvs tag: warning: -b ignored with -d options".to_string());
    }
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "tag",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("tag", "no files to tag"));
    }
    if opts.check_modified {
        check_modified(&targets, sink)?;
    }

    let mut session = protocol::open(root, "tag", cancel, sink)?;
    send_arguments(&mut session, &opts)?;
    let last = targets::send_blocks_quiet(&mut session, &targets)?;
    targets::finish_from_last(&mut session, cwd, &opts.names, "tag", last.as_deref())?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

/// The flags and the tag name, in the order the reference writes them. `-R` is the default and
/// is never sent; `-l` is sent when recursion was turned off.
fn send_arguments(session: &mut protocol::Session, opts: &Options) -> Result<()> {
    if opts.branch {
        session.argument("-b")?;
    }
    if opts.check_modified {
        session.argument("-c")?;
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
    if let Some(revision) = &opts.revision {
        session.argument("-r")?;
        session.argument(revision)?;
    }
    if let Some(date) = &opts.date {
        session.argument("-D")?;
        session.argument(&date::normalize(date))?;
    }
    session.argument(&opts.tag)
}

/// `-c`: name every file that would be tagged and is locally modified, then stop the command.
///
/// The reference makes this check on the server, so the lines a pserver user sees carry the
/// server's name (`cvs server:` and `cvs [server aborted]:`). This client makes the same check
/// before it connects and keeps that wording, so the output does not change when the GUI stops
/// starting `cvs.exe`.
fn check_modified(targets: &BTreeMap<String, Target>, sink: &mut dyn Sink) -> Result<()> {
    let mut modified = false;
    for target in targets.values() {
        for entry in target.entries.files() {
            if !target.selected && !target.filter.iter().any(|name| name == &entry.name) {
                continue;
            }
            if is_locally_modified(target, entry) {
                sink.err(format!(
                    "cvs server: {} is locally modified",
                    dir::display_path(&target.dir.update_dir, &entry.name)
                ));
                modified = true;
            }
        }
    }
    if modified {
        return Err(Error::aborted("server", "correct the above errors first!"));
    }
    Ok(())
}

/// A file whose recorded time no longer matches, and every file waiting for a commit (added,
/// removed, conflicted, or half-updated), counts as modified. A file that is gone does not:
/// the reference lets that through, because there is no working file to compare.
fn is_locally_modified(target: &Target, entry: &Entry) -> bool {
    if entry.is_added() || entry.is_removed() || entry.is_conflict() || entry.is_unfinished() {
        return true;
    }
    let local = target.dir.local.join(&entry.name);
    let Some(modified) = codec::file_mtime(&local) else {
        return false;
    };
    !codec::timestamps_equal(&codec::asctime(modified), &entry.date)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn flags_are_read_until_the_tag_name() {
        let opts = parse(&["-b", "-c", "-d", "-F", "-f", "-l", "-R", "T1", "a.txt"]).unwrap();
        assert!(opts.branch);
        assert!(opts.check_modified);
        assert!(opts.delete);
        assert!(opts.move_tag);
        assert!(opts.force);
        assert!(opts.recursive, "-R after -l turns recursion back on");
        assert_eq!(opts.tag, "T1");
        assert_eq!(opts.names, vec!["a.txt"]);
    }

    #[test]
    fn a_dash_word_after_the_tag_is_a_file() {
        let opts = parse(&["T1", "-b"]).unwrap();
        assert!(!opts.branch);
        assert_eq!(opts.tag, "T1");
        assert_eq!(opts.names, vec!["-b"]);
    }

    #[test]
    fn a_revision_and_a_date_take_a_separate_or_attached_value() {
        let opts = parse(&["-r", "1.1", "T1"]).unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
        assert_eq!(opts.tag, "T1");

        let opts = parse(&["-r1.1", "-D2020-01-01", "T1"]).unwrap();
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
        let error = parse(&["-Z", "T1"]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = parse(&["-b", "--", "-T1"]).unwrap();
        assert!(opts.branch);
        assert_eq!(opts.tag, "-T1");
        assert!(opts.names.is_empty());
    }

    #[test]
    fn no_tag_name_leaves_the_tag_empty() {
        let opts = parse(&["-b"]).unwrap();
        assert!(opts.tag.is_empty());
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-tag-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn target(dir: &std::path::Path) -> Target {
        Target {
            dir: dir::WorkingDir::new(
                dir.to_path_buf(),
                String::new(),
                "mod".to_string(),
                codec::for_dir(dir),
            ),
            entries: crate::client::work::entries::Entries::default(),
            filter: Vec::new(),
            selected: true,
            loose: Vec::new(),
        }
    }

    #[test]
    fn a_missing_file_is_not_modified_but_a_waiting_one_is() {
        let dir = scratch("modified");
        std::fs::write(dir.join("gone.txt"), "gone\n").unwrap();
        std::fs::write(dir.join("changed.txt"), "changed\n").unwrap();
        let target = target(&dir);

        let gone = Entry::new("gone.txt", "1.1", "Sat Jan  1 00:00:00 2000", "", "");
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        assert!(
            !is_locally_modified(&target, &gone),
            "a file that is gone is not modified"
        );

        let changed = Entry::new("changed.txt", "1.1", "Sat Jan  1 00:00:00 2000", "", "");
        assert!(is_locally_modified(&target, &changed));

        let recorded = codec::asctime(codec::file_mtime(&dir.join("changed.txt")).unwrap());
        let unchanged = Entry::new("changed.txt", "1.1", &recorded, "", "");
        assert!(!is_locally_modified(&target, &unchanged));

        let added = Entry::new("changed.txt", "0", "dummy timestamp", "", "");
        assert!(is_locally_modified(&target, &added));

        let removed = Entry::new("changed.txt", "-1.1", "dummy timestamp", "", "");
        assert!(is_locally_modified(&target, &removed));

        std::fs::remove_dir_all(&dir).ok();
    }
}
