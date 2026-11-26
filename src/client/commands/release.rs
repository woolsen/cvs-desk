//! `cvs release`: give up a working copy, and say so in the repository's history.
//!
//! The reference asks a child `cvs update -n -q` what is still altered, prints that report,
//! and only then asks whether to go on. This client makes the same report from the folders
//! and `CVS/Entries` it already reads, so no server is needed for the question; the `release`
//! request is sent only after the answer from the sink is yes, and `-d` deletes the folder
//! afterwards.
//!
//! One reference step is skipped: it runs `unedit` first so that watches are cleared. This
//! client does not record which files were marked with `cvs edit`, and running `unedit` over
//! every file would write them all read-only. The release itself is unaffected.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::cli;
use crate::client::commands::targets::{self, Target};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::dir;
use crate::client::work::entries::Entry;

/// Run `cvs release`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let mut delete = false;
    let mut names = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                names.extend(args[i + 1..].iter().cloned());
                break;
            }
            "-d" => delete = true,
            "-q" | "-Q" => {
                return Err(Error::plain(
                    "release",
                    "-q or -Q must be specified before \"release\"",
                ))
            }
            other if other.starts_with('-') => {
                usage(sink);
                return Ok(1);
            }
            other => names.push(other.to_string()),
        }
        i += 1;
    }
    if names.is_empty() {
        usage(sink);
        return Ok(1);
    }

    for name in &names {
        let folder = cwd.join(name);
        if !folder.is_dir() {
            sink.err(format!("cvs release: no such directory: {}", name));
            continue;
        }
        if !folder.join("CVS").is_dir() {
            sink.err(format!("cvs release: no repository directory: {}", name));
            continue;
        }

        let targets = targets::collect(&folder, true, &[])?;
        let altered = report_altered(&targets, sink);
        sink.out(format!(
            "You have [{}] altered files in this repository.",
            altered
        ));
        if !sink.confirm(format!(
            "Are you sure you want to release {}directory `{}': ",
            if delete { "(and delete) " } else { "" },
            name
        )) {
            sink.err("** `release' aborted by user choice.".to_string());
            continue;
        }

        let mut session = protocol::open(root.clone(), "release", cancel, sink)?;
        session.argument(name)?;
        session.send("release")?;
        session.flush()?;
        let _ = targets::read_text(&mut session, false)?;
        session.close();

        if delete {
            if let Err(e) = std::fs::remove_dir_all(&folder) {
                sink.err(format!(
                    "cvs release: deletion of directory {} failed: {}",
                    name, e
                ));
            }
        }
    }
    Ok(0)
}

fn usage(sink: &mut dyn Sink) {
    if let Some(text) = cli::command_help("release") {
        for line in text.lines() {
            sink.err(line.to_string());
        }
    }
}

/// Print the report `cvs update -n` would print, and return how many lines count as altered.
/// The letters are the reference's: `M` modified, `A` added, `R` removed, `C` conflict.
fn report_altered(targets: &BTreeMap<String, Target>, sink: &mut dyn Sink) -> usize {
    let mut altered = 0;
    for target in targets.values() {
        for entry in target.entries.files() {
            let Some(letter) = changed_letter(target, entry) else {
                continue;
            };
            sink.out(format!(
                "{} {}",
                letter,
                dir::display_path(&target.dir.update_dir, &entry.name)
            ));
            altered += 1;
        }
        // `?` files are shown, as update shows them, but do not count as altered.
        for name in targets::unknown_files(&target.dir.local, &target.entries) {
            sink.out(format!(
                "? {}",
                dir::display_path(&target.dir.update_dir, &name)
            ));
        }
    }
    altered
}

fn changed_letter(target: &Target, entry: &Entry) -> Option<char> {
    if entry.is_removed() {
        return Some('R');
    }
    if entry.is_conflict() {
        return Some('C');
    }
    if entry.is_added() {
        return Some('A');
    }
    // A file waiting for a commit, or one whose recorded time no longer matches.
    if entry.is_unfinished() {
        return Some('M');
    }
    let local = target.dir.local.join(&entry.name);
    let modified = codec::file_mtime(&local)?;
    if codec::timestamps_equal(&codec::asctime(modified), &entry.date) {
        None
    } else {
        Some('M')
    }
}
