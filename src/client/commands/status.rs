//! `cvs status`: what the server knows about each file, and where the working copy differs.
//!
//! The server formats the answer: this client's job is to say which directories and files to
//! look at, and to tell it which files are unchanged, modified or absent. Sending a file's
//! contents is how CVS lets the server say "locally modified" or "had conflicts"; unchanged
//! files cost only one `Unchanged` line each.
//!
//! Two details are the command's own and are not shared with `log`/`diff`:
//!
//! * an explicitly named file that is not in `Entries` is *not* an error. A file that exists
//!   is sent as `Modified` without an `Entry` line, a missing one is only named as an
//!   `Argument`; the server then answers `Unknown`, as `status.c` does.
//! * a conflict that has not been touched since the merge is sent as `+=` + `Unchanged`, so
//!   the server can say `Unresolved Conflict` instead of `File had conflicts on merge`.
//!
//! `status` never writes the working copy, so `-n` needs no local guard: it is already sent as
//! `Global_option -n` by [`protocol::open`], and the command only reads files.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::commands::targets::{self, Target};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::ignore::Ignore;
use crate::client::protocol;
use crate::client::root::Root;
use crate::client::work::dir::{self, WorkingDir};
use crate::client::work::entries::{Entries, Entry};
use crate::client::work::{codec, glob};

#[derive(Debug, Default)]
struct Options {
    recursive: bool,
    verbose: bool,
    quiet: u32,
    names: Vec<String>,
}

/// `getopt("+vlR")`: the first non-option ends option parsing, and short options may be
/// clustered (`-vl`, `-lR`). `-q`/`-Q` are the CVSNT spelling of quiet; they stay local.
fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options {
        recursive: true,
        ..Default::default()
    };
    let mut options_done = false;
    for arg in args {
        if options_done {
            opts.names.push(arg.clone());
        } else if arg == "--" {
            options_done = true;
        } else if arg.len() > 1 && arg.starts_with('-') {
            if arg.starts_with("--") {
                return Err(invalid_option(arg));
            }
            for letter in arg.chars().skip(1) {
                match letter {
                    'v' => opts.verbose = true,
                    'l' => opts.recursive = false,
                    'R' => opts.recursive = true,
                    'q' => opts.quiet = opts.quiet.max(1),
                    'Q' => opts.quiet = 2,
                    other => return Err(invalid_option(&format!("-{}", other))),
                }
            }
        } else {
            // As with `getopt("+…")`, a name stops option parsing: `status a.txt -v` treats
            // `-v` as a second name.
            options_done = true;
            opts.names.push(arg.clone());
        }
    }
    Ok(opts)
}

fn invalid_option(text: &str) -> Error {
    Error::plain("status", format!("invalid option -- '{}'", text))
}

/// Run `cvs status`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = parse_args(args)?;
    // Command-level `-q`/`-Q` joins the global setting; `Session::start` turns it into one
    // `Global_option` before the command's own arguments.
    let mut root = root;
    root.options.quiet = root.options.quiet.max(opts.quiet);

    if opts.names.is_empty() && !cwd.join("CVS").is_dir() {
        sink.err("cvs status: in directory .:".to_string());
        return Err(Error::plain(
            "status",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    // `send_file_names` expands wildcards on the client, on Windows where cmd does not.
    let expanded = glob::expand_args(cwd, &opts.names);
    let targets = collect(cwd, opts.recursive, &expanded, sink)?;

    let mut session = protocol::open(root, "status", cancel, sink)?;
    send_options(&mut session, &opts)?;
    let last = send_blocks(&mut session, &targets)?;
    finish_from_top(&mut session, cwd, &expanded, &targets, last.as_deref())?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

/// The command's own options, then `Max-dotdot`, before the first folder block. The reference
/// sends `--` even when the user did not type it (`status.c:81-85`), so a name starting with
/// `-` cannot be read as an option by the server.
fn send_options(session: &mut protocol::Session, opts: &Options) -> Result<()> {
    if opts.verbose {
        session.argument("-v")?;
    }
    if !opts.recursive {
        session.argument("-l")?;
    }
    session.argument("--")?;

    let levels = opts
        .names
        .iter()
        .map(|name| pathname_levels(name))
        .max()
        .unwrap_or(0);
    if levels > 0 {
        if session.supports("Max-dotdot") {
            session.send(&format!("Max-dotdot {}", levels))?;
        } else {
            return Err(Error::aborted(
                "status",
                "backreference in path (`..') not supported by old (pre-Max-dotdot) servers",
            ));
        }
    }
    Ok(())
}

/// Work out which folders and files the answer should cover.
///
/// Unlike [`targets::collect`], a named file that has no `Entries` line does not stop the
/// command: an existing file joins the folder's `loose` names (sent as `Modified` without an
/// `Entry`), a missing one contributes only its `Argument`. A folder that is not a working
/// copy is the reference's two-line complaint.
fn collect(
    cwd: &Path,
    recursive: bool,
    names: &[String],
    sink: &mut dyn Sink,
) -> Result<BTreeMap<String, Target>> {
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
            let found = dir::walk(&path, recursive)?;
            if found.is_empty() {
                return Err(no_version_in(sink, &prefix));
            }
            for mut dir in found {
                dir.update_dir = join_update(&prefix, &dir.update_dir);
                let entries = dir.entries()?;
                insert(&mut targets, dir, entries, Vec::new());
            }
            continue;
        }

        let parent = path.parent().unwrap_or(cwd);
        let Some(mut dir) = dir::walk(parent, false)?.into_iter().next() else {
            let shown = parent
                .strip_prefix(cwd)
                .map(|p| normalize_relative(&p.to_string_lossy()))
                .unwrap_or_default();
            return Err(no_version_in(sink, &shown));
        };
        // `walk(parent)` numbers the folder from the parent; the protocol numbers it from
        // the invocation folder.
        dir.update_dir = path
            .parent()
            .and_then(|p| p.strip_prefix(cwd).ok())
            .map(|p| normalize_relative(&p.to_string_lossy()))
            .unwrap_or_default();
        let entries = dir.entries()?;
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());

        if entries.file(&file_name).is_some() {
            insert(&mut targets, dir, entries, vec![file_name]);
        } else if path.exists() {
            // No `Entry`, but the file is there: `send_fileproc` sends its contents as
            // `Modified` anyway, and the server's `Classify_File` answers `Unknown`.
            let key = dir.update_dir.clone();
            let target = targets.entry(key).or_insert_with(|| Target {
                dir,
                entries,
                filter: Vec::new(),
                selected: false,
                loose: Vec::new(),
            });
            if !target.loose.iter().any(|known| known == &file_name) {
                target.loose.push(file_name);
            }
        }
        // A missing file sends nothing here; its `Argument` alone lets the server answer
        // `nothing known about …` and `Status: Unknown`, as the reference does.
    }
    Ok(targets)
}

fn no_version_in(sink: &mut dyn Sink, shown: &str) -> Error {
    let shown = if shown.is_empty() { "." } else { shown };
    sink.err(format!("cvs status: in directory {}:", shown));
    Error::plain(
        "status",
        "there is no version here; run 'cvs checkout' first",
    )
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
        if !target.filter.iter().any(|known| known == &name) {
            target.filter.push(name);
        }
    }
}

/// Send every `Directory` block, every file state, and the `?` lines for files CVS does not
/// know. Returns the update directory of the last block, so the final `Directory .` is only
/// sent when the blocks did not already end at the invocation folder.
fn send_blocks(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
) -> Result<Option<String>> {
    let base_ignore = Ignore::new(&[]);
    let mut last = None;
    for target in targets.values() {
        let absolute =
            dir::absolute_repository(session.root.protocol_root(), &target.dir.repository);
        let local = targets::display_dir(&target.dir.update_dir);
        session.directory(&local, &absolute, &target.dir.local)?;

        // The client's ignore list — defaults, `$CVSIGNORE`, `~/.cvsignore` and this folder's
        // `.cvsignore` — decides which unknown files are candidates, as `ignore_files()` does
        // before `send_ignproc`. A server with `Questionable` prints the `?` after its own
        // ignore rules; an old one leaves the printing to the client.
        if target.selected {
            let ignore = base_ignore.enter(&target.dir.local);
            let ask_server = session.supports("Questionable");
            for name in unknown_files(&target.dir.local, &target.entries, &ignore) {
                if ask_server {
                    session.send(&format!("Questionable {}", name))?;
                } else {
                    session.out(format!(
                        "? {}",
                        dir::display_path(&target.dir.update_dir, &name)
                    ));
                }
            }
        }

        let empty_conflicts = session.supports("Empty-conflicts");
        for entry in target.entries.files() {
            if !target.selected && !target.filter.iter().any(|name| name == &entry.name) {
                continue;
            }
            send_entry_state(session, &target.dir, entry, empty_conflicts)?;
        }

        // A named but unversioned file. When the folder was selected as a whole the server
        // already sees it as unknown, so the explicit `Modified` would be a duplicate.
        if !target.selected {
            for name in &target.loose {
                send_loose_state(session, &target.dir, name)?;
            }
        }
        last = Some(target.dir.update_dir.clone());
    }
    Ok(last)
}

/// The regular files in a folder that are neither in `Entries` nor ignored.
fn unknown_files(local: &Path, entries: &Entries, ignore: &Ignore) -> Vec<String> {
    let Ok(list) = std::fs::read_dir(local) else {
        return Vec::new();
    };
    let mut unknown = Vec::new();
    for item in list.flatten() {
        let Ok(kind) = item.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        if entries.file(&name).is_none() && !ignore.is_ignored(&name) {
            unknown.push(name);
        }
    }
    unknown.sort();
    unknown
}

fn send_entry_state(
    session: &mut protocol::Session,
    dir: &WorkingDir,
    entry: &Entry,
    empty_conflicts: bool,
) -> Result<()> {
    let local = dir.local.join(&entry.name);
    let metadata = match std::fs::metadata(&local) {
        Ok(metadata) => Some(metadata),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let ts_user = metadata
        .as_ref()
        .and_then(|_| codec::file_mtime(&local))
        .map(codec::asctime);

    let (conflict_field, unchanged) = entry_wire_state(entry, ts_user.as_deref(), empty_conflicts);
    session.send(&format!(
        "Entry /{}/{}/{}/{}/{}",
        entry.name, entry.revision, conflict_field, entry.options, entry.tag
    ))?;

    if entry.is_removed() {
        // The file is recorded as removed and may already be gone; the entry alone says it.
        return Ok(());
    }
    let Some(metadata) = metadata else {
        // A missing file is not "Questionable" (that word is for files CVS has never heard
        // of). The entry alone is enough; the server answers "Needs Checkout".
        return Ok(());
    };

    if unchanged {
        session.send(&format!("Unchanged {}", entry.name))?;
        return Ok(());
    }

    let mut content = std::fs::read(&local)?;
    if !entry.is_binary() && !entry.is_unicode() {
        content = codec::to_repository_newlines(&content);
    }
    let mode = if metadata.permissions().readonly() {
        "u=r,g=r,o=r"
    } else {
        "u=rw,g=rw,o=rw"
    };
    session.send(&format!("Modified {}", entry.name))?;
    session.send(mode)?;
    session.send(&content.len().to_string())?;
    session.send_bytes(&content)?;
    Ok(())
}

/// The `Entry` conflict field and whether the file can be called `Unchanged`.
///
/// `+=` means the conflict timestamp matches the file (nothing happened since the merge), so
/// the server can answer `Unresolved Conflict`; `+modified` means the user has touched the
/// file, and the server sees markers and answers `File had conflicts on merge`. Only with
/// `Empty-conflicts` may the client say `Unchanged` for the matching case (`client.c:5100-5107`,
/// `5162-5205`).
fn entry_wire_state(
    entry: &Entry,
    ts_user: Option<&str>,
    empty_conflicts: bool,
) -> (&'static str, bool) {
    let (base_ts, conflict_ts) = match entry.date.split_once('+') {
        Some((base, conflict)) => (base, Some(conflict)),
        None => (entry.date.as_str(), None),
    };
    let conflict_matches = conflict_ts.is_some_and(|recorded| time_matches(ts_user, recorded));
    let conflict_field = if entry.is_conflict() {
        if conflict_matches {
            "+="
        } else {
            "+modified"
        }
    } else {
        ""
    };
    let state_matches = if entry.is_conflict() && empty_conflicts {
        conflict_matches
    } else {
        time_matches(ts_user, base_ts)
    };
    let unchanged = !entry.is_added() && !entry.is_unfinished() && state_matches;
    (conflict_field, unchanged)
}

/// Does the file still have the time CVS recorded? Both sides are UTC, as CVSNT writes them.
fn time_matches(ts_user: Option<&str>, recorded: &str) -> bool {
    match ts_user {
        Some(user) if !recorded.trim().is_empty() => codec::timestamps_equal(user, recorded),
        _ => false,
    }
}

/// A named file with no `Entry`: the reference sends its contents as `Modified` without an
/// `Entry` line. There is no recorded mode or timestamp, so the read-only bit decides the
/// mode and the bytes are treated as text.
fn send_loose_state(session: &mut protocol::Session, dir: &WorkingDir, name: &str) -> Result<()> {
    let local = dir.local.join(name);
    let metadata = match std::fs::metadata(&local) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let content = codec::to_repository_newlines(&std::fs::read(&local)?);
    let mode = if metadata.permissions().readonly() {
        "u=r,g=r,o=r"
    } else {
        "u=rw,g=rw,o=rw"
    };
    session.send(&format!("Modified {}", name))?;
    session.send(mode)?;
    session.send(&content.len().to_string())?;
    session.send_bytes(&content)?;
    Ok(())
}

/// `Directory .` when the last block was below the invocation folder, then the `Argument`
/// lines and the command. The reference's `send_repository("", …, ".")` skips a duplicate.
fn finish_from_top(
    session: &mut protocol::Session,
    cwd: &Path,
    names: &[String],
    targets: &BTreeMap<String, Target>,
    last: Option<&str>,
) -> Result<()> {
    if last != Some("") {
        let repository = dir::read_repository(cwd)
            .or_else(|| {
                targets
                    .values()
                    .next()
                    .map(|target| target.dir.repository.clone())
            })
            .ok_or_else(|| Error::aborted("status", "CVS/Repository is missing or empty"))?;
        let absolute = dir::absolute_repository(session.root.protocol_root(), &repository);
        session.directory(".", &absolute, cwd)?;
    }
    targets::finish_arguments(session, names, "status")
}

/// How many `..` components a path has, the way `pathname_levels` counts them.
fn pathname_levels(name: &str) -> usize {
    let mut level: isize = 0;
    let mut max = 0;
    for part in name.split(['/', '\\']) {
        match part {
            ".." => {
                level += 1;
                if level > max {
                    max = level;
                }
            }
            "." | "" => {}
            _ => level -= 1,
        }
    }
    max.max(0) as usize
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
        list.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn short_options_may_be_clustered() {
        let opts = parse_args(&args(&["-vlR"])).unwrap();
        assert!(opts.verbose);
        assert!(opts.recursive);

        let opts = parse_args(&args(&["-lv"])).unwrap();
        assert!(opts.verbose);
        assert!(!opts.recursive);

        let opts = parse_args(&args(&["-qQ"])).unwrap();
        assert_eq!(opts.quiet, 2);
    }

    #[test]
    fn the_first_name_stops_option_parsing() {
        let opts = parse_args(&args(&["a.txt", "-v"])).unwrap();
        assert_eq!(opts.names, args(&["a.txt", "-v"]));
        assert!(!opts.verbose);
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = parse_args(&args(&["--", "-v", "-l"])).unwrap();
        assert_eq!(opts.names, args(&["-v", "-l"]));
        assert!(!opts.verbose);
        assert!(opts.recursive);
    }

    #[test]
    fn an_unknown_letter_is_refused() {
        let error = parse_args(&args(&["-vx"])).unwrap_err();
        assert!(error.text().contains("invalid option"));
    }

    #[test]
    fn a_fresh_conflict_is_unchanged_with_the_plus_equals_marker() {
        let entry = Entry::new(
            "a.txt",
            "1.2",
            "Result of merge+Tue Nov 24 09:52:53 2026",
            "",
            "",
        );
        let (field, unchanged) = entry_wire_state(&entry, Some("Tue Nov 24 09:52:53 2026"), true);
        assert_eq!(field, "+=");
        assert!(unchanged);
    }

    #[test]
    fn a_touched_conflict_is_modified_with_the_plus_modified_marker() {
        let entry = Entry::new(
            "a.txt",
            "1.2",
            "Result of merge+Tue Nov 24 09:52:53 2026",
            "",
            "",
        );
        let (field, unchanged) = entry_wire_state(&entry, Some("Wed Nov 25 08:00:00 2026"), true);
        assert_eq!(field, "+modified");
        assert!(!unchanged);
    }

    #[test]
    fn a_plain_entry_compares_its_recorded_time() {
        let entry = Entry::new("a.txt", "1.2", "Tue Nov 24 09:52:53 2026", "", "");
        assert!(entry_wire_state(&entry, Some("Tue Nov 24 09:52:53 2026"), true).1);
        assert!(!entry_wire_state(&entry, Some("Wed Nov 25 08:00:00 2026"), true).1);
    }

    #[test]
    fn levels_count_leading_parent_folders() {
        assert_eq!(pathname_levels("a/b"), 0);
        assert_eq!(pathname_levels("../a"), 1);
        assert_eq!(pathname_levels("../../a/b"), 2);
    }
}
