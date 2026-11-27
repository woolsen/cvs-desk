//! Tests against a **real CVS working copy** on the local machine.
//!
//! The folder comes from `CVSDESK_REAL_FOLDER`; nothing here writes to it. Two things are
//! checked that a made-up sample cannot show:
//!
//! 1. The `CVS/Entries` files CVSNT wrote are **GBK**, not UTF-8. Reading them as UTF-8
//!    turns every Chinese name into replacement marks, so the name matches no file on disk
//!    and every row shows the wrong state. This is what `core::text::decode` exists for.
//! 2. Every name read from those files must match a real file or folder on disk. That is the
//!    check that proves the decoding is right rather than merely plausible: if a name is
//!    decoded wrongly, it will not be found in the listing, and the test says which.
//!
//! ## Running them
//!
//! These are `#[ignore]`d, because a folder that exists on one computer will not exist on
//! another. Set `CVSDESK_REAL_FOLDER` to point at any CVS working copy and run:
//!
//! ```text
//! cargo test --test real_folder -- --ignored --nocapture
//! ```
//!
//! Note what these tests do **not** do: they never run `cvs` and never contact the server, so
//! no password is needed and none is stored. A test of the network side has to be started by
//! hand, because it needs a live account — see `tests/README.md`.

use chrono::{DateTime, Utc};
use cvsdesk::entries::{self, State};
use cvsdesk::text;
use std::fs;
use std::path::{Path, PathBuf};

/// The folder named by `CVSDESK_REAL_FOLDER`.
fn real_folder() -> Option<PathBuf> {
    std::env::var("CVSDESK_REAL_FOLDER")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from)
}

/// Skip (rather than fail) when no folder is set or it is not present locally.
fn folder_or_skip() -> Option<PathBuf> {
    let Some(p) = real_folder() else {
        eprintln!("skipping: set CVSDESK_REAL_FOLDER to a CVS working copy");
        return None;
    };
    if p.join("CVS").is_dir() {
        Some(p)
    } else {
        eprintln!("skipping: {} is not a CVS working copy here", p.display());
        None
    }
}

#[test]
#[ignore = "needs a real working copy; run with --ignored"]
fn the_real_entries_file_is_not_utf8() {
    let Some(root) = folder_or_skip() else { return };
    let path = root.join("CVS").join("Entries");
    let bytes = fs::read(&path).expect("read Entries");
    assert!(
        std::str::from_utf8(&bytes).is_err(),
        "{} happens to be valid UTF-8; this test's premise no longer holds",
        path.display()
    );
    let decoded = text::decode(&bytes);
    assert!(!decoded.was_utf8, "the reader must notice it is not UTF-8");
    assert_eq!(
        decoded.replaced, 0,
        "the real Entries file must decode without replacement; {} mark(s) appeared",
        decoded.replaced
    );
    assert!(
        decoded.note().unwrap().contains("GBK"),
        "the note should say how it was read, got {:?}",
        decoded.note()
    );
}

#[test]
#[ignore = "needs a real working copy; run with --ignored"]
fn every_name_the_program_reads_exists_on_disk() {
    let Some(root) = folder_or_skip() else { return };

    // Visit this folder and its immediate subfolders that are themselves working copies.
    let mut folders = vec![root.clone()];
    if let Ok(list) = fs::read_dir(&root) {
        for e in list.flatten() {
            let p = e.path();
            if p.is_dir() && p.join("CVS").is_dir() {
                folders.push(p);
            }
        }
    }

    let mut checked = 0usize;
    let mut unmatched: Vec<String> = Vec::new();
    for folder in &folders {
        let loaded = entries::load(folder);
        let on_disk: Vec<String> = fs::read_dir(folder)
            .map(|list| {
                list.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        for row in &loaded.rows {
            checked += 1;
            // A row for a file that CVS knows about but which is not on disk is genuinely
            // "missing" — that is a real state, not a decoding failure. Everything else must
            // be found.
            if row.state == State::Missing {
                continue;
            }
            if !on_disk.iter().any(|n| *n == row.name) {
                unmatched.push(format!(
                    "{} in {} ({:?})",
                    row.name,
                    folder.display(),
                    row.state
                ));
            }
        }
    }

    assert!(
        checked > 0,
        "no entries were read at all; the reader is not working"
    );
    eprintln!(
        "checked {} row(s) across {} folder(s)",
        checked,
        folders.len()
    );
    assert!(
        unmatched.is_empty(),
        "{} name(s) did not match a file on disk, which means the decoding is wrong:\n{}",
        unmatched.len(),
        unmatched.join("\n")
    );
}

#[test]
#[ignore = "needs a real working copy; run with --ignored"]
fn the_real_root_and_repository_are_read() {
    let Some(root) = folder_or_skip() else { return };
    let loaded = entries::load(&root);
    eprintln!("repository = {:?}", loaded.repository);
    assert!(
        !loaded.root.trim().is_empty(),
        "CVS/Root must be read; without it no command can run"
    );
    assert!(
        !loaded.repository.trim().is_empty(),
        "CVS/Repository must be read; without it the module name is unknown"
    );
    let (kind, user, host, port, path) = entries::parse_cvsroot(&loaded.root);
    // The method and host matter; the account name is not this program's business, so it is
    // reported by presence only and never printed.
    assert!(!user.is_empty(), "a pserver root names its account");
    assert!(!host.is_empty(), "a pserver root names its host");
    assert!(path.starts_with('/'), "the repository path is absolute");
    eprintln!(
        "connection kind = {:?}, host = {}, port = {:?}, path = {}, account present = {}",
        kind,
        host,
        port,
        path,
        !user.is_empty()
    );
}

#[test]
#[ignore = "needs a real working copy; run with --ignored"]
fn a_file_with_revision_zero_shows_as_added() {
    let Some(root) = folder_or_skip() else { return };
    // A revision-0 entry is a file added but not yet committed. That is the state the CVS
    // rule calls "added".
    let mut folders = vec![root.clone()];
    if let Ok(list) = fs::read_dir(&root) {
        for e in list.flatten() {
            let p = e.path();
            if p.is_dir() && p.join("CVS").is_dir() {
                folders.push(p);
            }
        }
    }
    let mut found = false;
    for folder in folders {
        for row in entries::load(&folder).rows {
            if row.revision == "0" && row.name.to_lowercase().contains("test") {
                assert_eq!(
                    row.state,
                    State::Added,
                    "revision 0 must show as added ({} in {})",
                    row.name,
                    folder.display()
                );
                found = true;
            }
        }
    }
    if !found {
        eprintln!("note: no revision-0 file in this copy, so that case was not exercised");
    }
}

/// A guard for the rule that matters most about a real working copy: it holds an account
/// name, and nothing under the project may carry it. Run with `--ignored`, like the rest.
#[test]
#[ignore = "scans the project's own files; run with --ignored"]
fn no_real_credentials_are_committed() {
    let Some(root) = folder_or_skip() else { return };
    let loaded = entries::load(&root);
    let root_text = loaded.root.clone();
    if root_text.trim().is_empty() {
        eprintln!("no Root in the real folder; nothing to look for");
        return;
    }
    // Only the account and host are looked for: a password is never written by CVS into Root.
    let parts: Vec<&str> = root_text.split(':').collect();
    let needle = parts.get(2).copied().unwrap_or("").to_string();
    if needle.is_empty() {
        eprintln!("no account name in the real Root; nothing to look for");
        return;
    }

    let project = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut hits = Vec::new();
    walk(project, &needle, &mut hits);
    assert!(
        hits.is_empty(),
        "the real account name appears in {} project file(s), which must not be committed:\n{}",
        hits.len(),
        hits.join("\n")
    );
    eprintln!(
        "the account name appears in no file under {}",
        project.display()
    );
}

#[test]
#[ignore = "needs a real working copy; run with --ignored"]
fn recorded_times_are_utc_so_unchanged_files_stay_unchanged() {
    let Some(root) = folder_or_skip() else { return };
    let loaded = entries::load(&root);
    let mut checked = 0usize;
    let mut disagreements = Vec::new();
    for row in loaded.rows.iter().filter(|row| !row.is_dir) {
        if row.state != State::Unchanged && row.state != State::Modified {
            continue;
        }
        if rows_timestamp_is_special(row) {
            continue;
        }
        let Ok(metadata) = fs::metadata(root.join(&row.name)) else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let utc: DateTime<Utc> = modified.into();
        let text = utc.naive_utc().format("%a %b %e %H:%M:%S %Y").to_string();
        let equal = entries::timestamps_equal(&text, &row.timestamp);
        checked += 1;
        if equal != (row.state == State::Unchanged) {
            disagreements.push(format!(
                "{}: Entries says '{}', file UTC time is '{}', state is {:?}",
                row.name, row.timestamp, text, row.state
            ));
        }
    }
    assert!(
        checked > 0,
        "no ordinary files were compared; the reader found nothing to check"
    );
    eprintln!("compared {} file time(s)", checked);
    assert!(
        disagreements.is_empty(),
        "{} file(s) disagree about being unchanged, which means the time zone is wrong:\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
}

fn rows_timestamp_is_special(row: &cvsdesk::entries::Row) -> bool {
    row.timestamp.trim().is_empty()
        || row.timestamp.starts_with('/')
        || row
            .timestamp
            .trim()
            .to_ascii_lowercase()
            .starts_with("result of merge")
        || row.revision == "0"
        || row.revision.starts_with("-1")
}

fn walk(dir: &Path, needle: &str, hits: &mut Vec<String>) {
    let Ok(list) = fs::read_dir(dir) else { return };
    for e in list.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        // Never look inside these: build output and the Git store are not our source, and
        // walking them is slow and noisy.
        if name == "target" || name == ".git" {
            continue;
        }
        if p.is_dir() {
            walk(&p, needle, hits);
        } else if p.metadata().map(|m| m.len() < 2_000_000).unwrap_or(false) {
            if let Ok(bytes) = fs::read(&p) {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    if text.contains(needle) {
                        hits.push(p.display().to_string());
                    }
                }
            }
        }
    }
}
