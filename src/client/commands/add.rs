//! `cvs add`: schedule new files and folders in `CVS/Entries`.
//!
//! The request is the reference client's (`add.c`, `client.c`): the `-k`/`-m` arguments,
//! `--`, a leading `Directory .` when a name carries a slash, the parent block of every new
//! folder, `Max-dotdot`, the folder blocks with each file's `Entry`/`Is-modified`, a closing
//! `Directory .` when the walk ended below it, the names as `Argument`, and `add`.
//!
//! New folders are created on disk before the request, exactly as `Create_Admin` does
//! (`add.c:249-277`): that is what lets `cvs add dir dir/file` work in one run. A file whose
//! `Entries` line starts with `-` is a resurrection: its line is sent unchanged and the server
//! restores the file with an `Updated` response (`add.c:594-663`).
//!
//! Each new file is registered locally with revision `0` and the `dummy timestamp` marker,
//! the way CVS does, and that index is only written once the server has said `ok`: a failed
//! `add` leaves the file entries as they were. A new folder's own `CVS/` files and its `D/`
//! line in the parent are written before the request, just as `Create_Admin` and
//! `Subdir_Register` do, so a rerun can recover a folder whose add failed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::SystemTime;

use crate::client::cli;
use crate::client::commands::targets;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, FileKind, Response};
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::dir::{self, WorkingDir};
use crate::client::work::entries::{Entries, Entry};
use crate::client::work::glob;

/// The RCS keyword modes `RCS_check_kflag` accepts (`rcs.c:47-48`), plus CVSNT's `u`, which
/// the desktop program uses for its Unicode files (`entries.rs::is_unicode`).
const KFLAGS: &[&str] = &["kv", "kvl", "k", "v", "o", "b", "u"];

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

fn valid_kflag(flag: &str) -> bool {
    KFLAGS.contains(&flag)
}

/// The lines `RCS_check_kflag` prints for a bad mode (`rcs.c:3353-3365`).
fn keyword_usage() -> [String; 9] {
    [
        "cvs add: invalid RCS keyword expansion mode".to_string(),
        "Valid expansion modes include:".to_string(),
        "   -kkv\tGenerate keywords using the default form.".to_string(),
        "   -kkvl\tLike -kkv, except locker's name inserted.".to_string(),
        "   -kk\tGenerate only keyword names in keyword strings.".to_string(),
        "   -kv\tGenerate only keyword values in keyword strings.".to_string(),
        "   -ko\tGenerate the old keyword string (no changes from checked in file).".to_string(),
        "   -kb\tGenerate binary file unmodified (merges not allowed) (RCS 5.7).".to_string(),
        "(Specify the --help global option for a list of other help options)".to_string(),
    ]
}

/// Everything the request needs: the folders whose entries change, the blocks in the order
/// the reference sends them, and the new folders created before the request.
struct Folder {
    working: WorkingDir,
    entries: Entries,
}

struct NewDir {
    working: WorkingDir,
    parent: WorkingDir,
    files: Vec<String>,
}

enum Item {
    /// A file named on the command line. `entry` is the `Entries` line to send, if the file
    /// has one; a brand-new file is only `Is-modified`.
    File {
        key: String,
        name: String,
        entry: Option<Entry>,
    },
    /// A new folder: its `Directory` block and the files inside it.
    Directory { key: String, files: Vec<String> },
}

struct Plan {
    top: WorkingDir,
    folders: BTreeMap<String, Folder>,
    items: Vec<Item>,
    new_dirs: Vec<NewDir>,
}

impl Plan {
    fn new(cwd: &Path) -> Result<Plan> {
        let repository = dir::read_repository(cwd)
            .ok_or_else(|| Error::aborted("add", "CVS/Repository is missing or empty"))?;
        Ok(Plan {
            top: WorkingDir::new(
                cwd.to_path_buf(),
                String::new(),
                repository,
                codec::for_dir(cwd),
            ),
            folders: BTreeMap::new(),
            items: Vec::new(),
            new_dirs: Vec::new(),
        })
    }

    /// The folder's index, read the first time the command touches it.
    fn entries_for(&mut self, working: &WorkingDir) -> Result<&mut Entries> {
        let key = working.update_dir.clone();
        if !self.folders.contains_key(&key) {
            let entries = Entries::read(&working.local)?;
            let working = WorkingDir::new(
                working.local.clone(),
                working.update_dir.clone(),
                working.repository.clone(),
                working.codec,
            );
            self.folders
                .insert(key.clone(), Folder { working, entries });
        }
        Ok(&mut self
            .folders
            .get_mut(&key)
            .expect("the folder was just inserted")
            .entries)
    }
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
    // getopt handles `-k` before it looks at the names, so a bad mode is reported first
    // (`add.c:84-86`, `rcs.c:3350-3382`).
    if let Some(flag) = &opts.kflag {
        if !valid_kflag(flag) {
            for line in keyword_usage() {
                sink.err(line);
            }
            return Ok(1);
        }
    }
    if opts.names.is_empty() {
        // The reference prints the command's usage and exits 1, without a prefix, before it
        // looks at the working copy (`add.c:72-73`).
        if let Some(text) = cli::command_help("add") {
            sink.err(text.to_string());
        }
        return Ok(1);
    }
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "add",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let no_execute = root.options.no_execute;
    let quiet = root.options.quiet > 0;
    let mut errors = 0usize;

    // `add.c:109-156`: `.`, `..` and anything whose last component is `CVS` are skipped with
    // a complaint; the rest of the command still runs.
    let mut names: Vec<String> = Vec::new();
    for raw in glob::expand_args(cwd, &opts.names) {
        let shown = raw.trim_end_matches(['/', '\\']);
        if shown.is_empty() {
            errors += 1;
            continue;
        }
        if shown == "." || shown == ".." || last_component(shown).eq_ignore_ascii_case("CVS") {
            if !quiet {
                sink.err(format!(
                    "cvs add: cannot add special file `{}'; skipping",
                    shown
                ));
            }
            errors += 1;
            continue;
        }
        let name = normalize(shown);
        if name.is_empty() {
            errors += 1;
            continue;
        }
        // The same name twice is one entry, not two requests.
        if names.iter().any(|existing| existing == &name) {
            continue;
        }
        names.push(name);
    }
    if names.is_empty() {
        return Ok(1);
    }

    let options = match &opts.kflag {
        Some(flag) => format!("-k{}", flag),
        None => String::new(),
    };
    if dir::read_repository(cwd).is_none() {
        // `Name_Repository` prints these two lines when `CVS/Repository` is missing or
        // empty (repos.c:73-76).
        sink.err("cvs add: in directory .:".to_string());
        return Err(Error::aborted(
            "add",
            "*PANIC* administration files missing",
        ));
    }
    let named_files: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let mut plan = Plan::new(cwd)?;

    // First pass: every directory argument is created before the request, as `Create_Admin`
    // does (`add.c:197-279`). This is what makes a later `dir/file` name resolvable.
    for name in &names {
        let path = cwd.join(name);
        if !path.is_dir() {
            continue;
        }
        if path.join("CVS").is_dir() {
            errors += 1;
            sink.err(format!("cvs add: {}/CVS already exists", name));
            continue;
        }
        let (parent_name, base) = split_parent(name);
        let parent_local = cwd.join(&parent_name);
        let Some(parent_repository) = dir::read_repository(&parent_local) else {
            errors += 1;
            sink.err(format!("cvs add: there is no version here for '{}'", name));
            continue;
        };
        let repository = join_repository(&parent_repository, base);
        let working = WorkingDir::new(
            path.clone(),
            name.clone(),
            repository,
            codec::for_dir(&path),
        );
        let parent = WorkingDir::new(
            parent_local.clone(),
            parent_name.to_string(),
            parent_repository,
            codec::for_dir(&parent_local),
        );
        if !no_execute {
            working.ensure(&root)?;
            copy_sticky_tag(&parent_local, &working)?;
            working.write_entries(&Entries::parse("D"))?;
            // The parent records the new folder immediately (`Subdir_Register`, add.c:270).
            let parent_entries = plan.entries_for(&parent)?;
            parent_entries.set_directory(base);
            parent.write_entries(parent_entries)?;
        }
        // `send_files` only sends `Questionable` for the files of a new folder that the
        // command line did not name: a named file is processed by `send_fileproc` instead
        // (`client.c:5227-5267`).
        let mut files = targets::unknown_files(&working.local, &Entries::default());
        let prefix = format!("{}/", name);
        files.retain(|file| !named_files.contains(format!("{}{}", prefix, file).as_str()));
        plan.new_dirs.push(NewDir {
            working,
            parent,
            files,
        });
    }

    // Second pass: the blocks, in the order the user typed the names.
    for name in &names {
        let new_dir_files = plan
            .new_dirs
            .iter()
            .find(|dir| &dir.working.update_dir == name)
            .map(|dir| dir.files.clone());
        if let Some(files) = new_dir_files {
            plan.items.push(Item::Directory {
                key: name.clone(),
                files,
            });
            continue;
        }
        // A directory that the first pass refused (it already has `CVS/`) is not a file.
        if cwd.join(name).is_dir() {
            continue;
        }
        let (parent_name, base) = split_parent(name);
        let parent_local = cwd.join(&parent_name);
        let Some(repository) = dir::read_repository(&parent_local) else {
            errors += 1;
            sink.err(format!("cvs add: there is no version here for '{}'", name));
            continue;
        };
        let working = WorkingDir::new(
            parent_local.clone(),
            parent_name.to_string(),
            repository,
            codec::for_dir(&parent_local),
        );
        let entry = find_entry(&Entries::read(&working.local)?, base);
        let file_exists = cwd.join(name).is_file();
        match entry {
            Some(existing) if existing.revision.starts_with('-') => {
                if file_exists {
                    errors += 1;
                    sink.err(format!(
                        "cvs add: {} should be removed and is still there (or is back again)",
                        name
                    ));
                    continue;
                }
                // A resurrection: the server checks out the last revision and sends it back
                // as `Updated` (add.c:594-663). The local line changes only then.
                plan.entries_for(&working)?;
                plan.items.push(Item::File {
                    key: parent_name.to_string(),
                    name: base.to_string(),
                    entry: Some(existing),
                });
            }
            Some(existing) if existing.revision == "0" => {
                errors += 1;
                if !quiet {
                    sink.err(format!("cvs add: {} has already been entered", name));
                }
            }
            Some(existing) => {
                errors += 1;
                if !quiet {
                    sink.err(format!(
                        "cvs add: {} already exists, with version number {}",
                        name, existing.revision
                    ));
                }
            }
            None => {
                if !file_exists {
                    errors += 1;
                    sink.err(format!("cvs add: nothing known about {}", name));
                    continue;
                }
                let tag = dir::sticky_tag(&parent_local).unwrap_or_default();
                let entries = plan.entries_for(&working)?;
                entries.set_file(Entry::new(base, "0", "dummy timestamp", &options, &tag));
                plan.items.push(Item::File {
                    key: parent_name.to_string(),
                    name: base.to_string(),
                    entry: None,
                });
            }
        }
    }
    if plan.items.is_empty() {
        return Ok(if errors > 0 { 1 } else { 0 });
    }

    let mut session = protocol::open(root, "add", cancel, sink)?;
    if let Some(flag) = &opts.kflag {
        session.argument(&format!("-k{}", flag))?;
    }
    if let Some(message) = &opts.message {
        session.argument("-m")?;
        session.argument(message)?;
    }
    session.argument("--")?;

    // `add.c:186-195`: a name with a slash first needs the invocation folder on the wire.
    let found_slash = names.iter().any(|name| name.contains('/'));
    let mut last: Option<String> = None;
    if found_slash {
        send_directory(&mut session, &plan.top)?;
        last = Some(String::new());
    }
    // `add.c:249-277`: each new folder's parent goes before the folder blocks, but only
    // when a name has a slash; without one CVS 1.9 compatibility sends no leading
    // `Directory` at all.
    if found_slash {
        for new_dir in &plan.new_dirs {
            let key = new_dir.parent.update_dir.clone();
            if last.as_deref() != Some(key.as_str()) {
                send_directory(&mut session, &new_dir.parent)?;
                last = Some(key);
            }
        }
    }
    send_max_dotdot(&mut session, &names)?;

    // `ignore_files` runs once the folder's own files are done (`client.c:5251-5267`), so the
    // `Questionable` names of a new folder go after the files named on the command line.
    let mut questionable: Option<(String, Vec<String>)> = None;
    for item in &plan.items {
        let item_key = match item {
            Item::File { key, .. } | Item::Directory { key, .. } => key,
        };
        if questionable
            .as_ref()
            .is_some_and(|(key, _)| key != item_key)
        {
            flush_questionable(&mut session, &mut questionable)?;
        }
        match item {
            Item::File { key, name, entry } => {
                // With `-n` a new folder has no `CVS/`, so `send_dirent_proc` returns
                // `R_SKIP_ALL` and none of its files reach the wire (client.c:5332-5352).
                if no_execute
                    && plan
                        .new_dirs
                        .iter()
                        .any(|dir| &dir.working.update_dir == key)
                {
                    continue;
                }
                if last.as_deref() != Some(key.as_str()) {
                    let folder = plan.folders.get(key).ok_or_else(|| {
                        Error::aborted("add", format!("the folder '{}' is missing", key))
                    })?;
                    send_directory(&mut session, &folder.working)?;
                    last = Some(key.clone());
                }
                let folder = plan
                    .folders
                    .get(key)
                    .expect("the folder was checked just above");
                send_file_state(&mut session, folder, name, entry.as_ref())?;
            }
            Item::Directory { key, files } => {
                let folder = plan
                    .new_dirs
                    .iter()
                    .find(|dir| &dir.working.update_dir == key)
                    .expect("every directory item has a new folder");
                // A folder without `CVS/` is still named under `-n`, but its files are not
                // (`client.c:5344-5352`).
                if last.as_deref() != Some(key.as_str()) {
                    send_directory(&mut session, &folder.working)?;
                    last = Some(key.clone());
                }
                if no_execute {
                    continue;
                }
                questionable = Some((key.clone(), files.clone()));
            }
        }
    }
    flush_questionable(&mut session, &mut questionable)?;
    // `send_files` always finishes with the invocation folder (client.c:5643).
    if last.as_deref().is_some_and(|key| !key.is_empty()) {
        send_directory(&mut session, &plan.top)?;
    }
    targets::finish_arguments(&mut session, &names, "add")?;

    let mut pending_mode: Option<String> = None;
    let mut pending_mtime: Option<SystemTime> = None;
    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::Mode(mode) => pending_mode = Some(mode),
            Response::ModTime(text) => pending_mtime = codec::parse_mod_time(&text),
            Response::CheckedIn {
                update_dir, entry, ..
            } => {
                record_checked_in(&mut plan, &update_dir, &entry)?;
                if let Some(mode) = pending_mode.take() {
                    let key = targets::target_key(&update_dir);
                    if let Some(folder) = plan.folders.get(&key) {
                        let path = folder.working.local.join(&entry_name(&entry));
                        apply_mode(&path, &mode, no_execute)?;
                    }
                }
                pending_mtime = None;
            }
            Response::File {
                kind,
                update_dir,
                entry,
                mode,
                content,
                ..
            } => {
                if kind == FileKind::Patched {
                    return Err(Error::aborted(
                        "add",
                        "the server sent a patch this client did not ask for",
                    ));
                }
                write_server_file(
                    &mut plan,
                    &update_dir,
                    &entry,
                    &mode,
                    &content,
                    pending_mtime,
                    no_execute,
                )?;
                pending_mode = None;
                pending_mtime = None;
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
            Response::NewEntry {
                update_dir, entry, ..
            } => {
                // `handle_new_entry` registers the line with the `dummy timestamp` marker
                // (client.c:2303-2311); the file's contents did not travel.
                record_checked_in(&mut plan, &update_dir, &entry)?;
            }
            Response::Removed(_)
            | Response::RemoveEntry(_)
            | Response::CopyFile { .. }
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

    write_plan(&plan, no_execute)?;
    session.close();
    Ok(if errors > 0 { 1 } else { 0 })
}

/// Send the `Questionable` names saved for the folder whose block is open.
fn flush_questionable(
    session: &mut protocol::Session,
    pending: &mut Option<(String, Vec<String>)>,
) -> Result<()> {
    if let Some((_, files)) = pending.take() {
        for name in files {
            session.send(&format!("Questionable {}", name))?;
        }
    }
    Ok(())
}

/// Apply the entry the server sent back. A new file (`0`) or a removed one (`-…`) is written
/// with `dummy timestamp`, exactly as `client.c:2261-2262` does.
fn record_checked_in(plan: &mut Plan, update_dir: &str, entry_line: &str) -> Result<()> {
    let mut entry = Entry::parse(entry_line).ok_or_else(|| {
        Error::aborted(
            "add",
            format!(
                "the server sent an entry this client cannot read: {}",
                entry_line
            ),
        )
    })?;
    let key = targets::target_key(update_dir);
    if entry.revision.is_empty() || entry.revision == "0" || entry.revision.starts_with('-') {
        entry.date = "dummy timestamp".to_string();
    } else if entry.date.is_empty() {
        if let Some(folder) = plan.folders.get(&key) {
            let local = folder.working.local.join(&entry.name);
            entry.date = codec::file_mtime(&local)
                .map(codec::asctime)
                .unwrap_or_default();
        }
    }
    if let Some(folder) = plan.folders.get_mut(&key) {
        folder.entries.set_file(entry);
    }
    Ok(())
}

/// Write the content of a resurrection. The entry's date is the file's own time, as
/// `client.c:2250-2254` does when the server sent no timestamp.
fn write_server_file(
    plan: &mut Plan,
    update_dir: &str,
    entry_line: &str,
    mode: &str,
    content: &[u8],
    mtime: Option<SystemTime>,
    no_execute: bool,
) -> Result<()> {
    let mut entry = Entry::parse(entry_line).ok_or_else(|| {
        Error::aborted(
            "add",
            format!(
                "the server sent an entry this client cannot read: {}",
                entry_line
            ),
        )
    })?;
    let key = targets::target_key(update_dir);
    let Some(folder) = plan.folders.get_mut(&key) else {
        return Ok(());
    };
    let local = folder.working.local.join(&entry.name);
    if !no_execute {
        let text = !entry.is_binary() && !entry.is_unicode();
        dir::write_file(&local, content, mode, mtime, text)?;
        let time = mtime
            .or_else(|| codec::file_mtime(&local))
            .unwrap_or_else(SystemTime::now);
        entry.date = merge_date(&entry.date, time);
        folder.entries.set_file(entry);
    }
    Ok(())
}

/// What CVS writes in the entry's date field after a merge.
fn merge_date(server_date: &str, time: SystemTime) -> String {
    let stamp = codec::asctime(time);
    if server_date.starts_with("+=") {
        format!("Result of merge+{}", stamp)
    } else {
        stamp
    }
}

/// Write the index now that the server accepted the command: the new folders' own `CVS/`
/// files were written before the request, so only the parent indexes are left.
fn write_plan(plan: &Plan, no_execute: bool) -> Result<()> {
    if no_execute {
        return Ok(());
    }
    for target in plan.folders.values() {
        target.working.write_entries(&target.entries)?;
    }
    Ok(())
}

/// The state line of a named file: its `Entry` when it has one, then `Is-modified` or
/// `Unchanged` when the file is on disk (`client.c:5073-5205`).
fn send_file_state(
    session: &mut protocol::Session,
    folder: &Folder,
    name: &str,
    entry: Option<&Entry>,
) -> Result<()> {
    if let Some(entry) = entry {
        session.send(&format!("Entry {}", protocol_entry(entry)))?;
    }
    let local = folder.working.local.join(name);
    if !local.is_file() {
        return Ok(());
    }
    let changed = entry.is_none_or(|entry| {
        entry.is_added()
            || entry.is_conflict()
            || entry.is_unfinished()
            || !matches_recorded_time(&local, &entry.date)
    });
    if changed {
        session.send(&format!("Is-modified {}", name))?;
    } else {
        session.send(&format!("Unchanged {}", name))?;
    }
    Ok(())
}

fn send_directory(session: &mut protocol::Session, dir: &WorkingDir) -> Result<()> {
    let absolute = dir::absolute_repository(session.root.protocol_root(), &dir.repository);
    session.directory(
        &targets::display_dir(&dir.update_dir),
        &absolute,
        &dir.local,
    )
}

/// `client.c:5562-5591`: `Max-dotdot` must reach the server before the `Directory` requests
/// whose names climb out of the invocation folder.
fn send_max_dotdot(session: &mut protocol::Session, names: &[String]) -> Result<()> {
    let levels = names
        .iter()
        .map(|name| pathname_levels(name))
        .max()
        .unwrap_or(0);
    if levels == 0 {
        return Ok(());
    }
    if !session.supports("Max-dotdot") {
        return Err(Error::aborted(
            &session.command,
            "backreference in path (`..') not supported by old (pre-Max-dotdot) servers",
        ));
    }
    session.send(&format!("Max-dotdot {}", levels))
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

fn matches_recorded_time(local: &Path, recorded: &str) -> bool {
    if recorded.trim().is_empty() {
        return false;
    }
    let Some(modified) = codec::file_mtime(local) else {
        return false;
    };
    codec::timestamps_equal(&codec::asctime(modified), recorded)
}

fn apply_mode(path: &Path, mode: &str, no_execute: bool) -> Result<()> {
    if no_execute || mode.trim().is_empty() || !path.is_file() {
        return Ok(());
    }
    dir::set_readonly(path, dir::mode_is_readonly(mode))
}

/// The entry line as the protocol carries it (`client.c:5092-5125`).
fn protocol_entry(entry: &Entry) -> String {
    let date = if entry.is_conflict() { "+=" } else { "" };
    format!(
        "/{}/{}/{}/{}/{}",
        entry.name, entry.revision, date, entry.options, entry.tag
    )
}

fn entry_name(line: &str) -> String {
    Entry::parse(line)
        .map(|entry| entry.name)
        .unwrap_or_default()
}

/// The `Entries` line for a name, matched the way `fncmp` matches on Windows
/// (`add.c:356`, `lib/fncase.c:135`): `readme` finds a recorded `README`.
fn find_entry(entries: &Entries, name: &str) -> Option<Entry> {
    if let Some(entry) = entries.file(name) {
        return Some(entry.clone());
    }
    #[cfg(windows)]
    {
        entries
            .files()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
            .cloned()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// `d/e/f` → (`d/e`, `f`); `f` → ("", `f`).
fn split_parent(name: &str) -> (&str, &str) {
    match name.rsplit_once('/') {
        Some((parent, base)) => (parent, base),
        None => ("", name),
    }
}

fn join_repository(repository: &str, name: &str) -> String {
    format!("{}/{}", repository.trim_end_matches('/'), name)
}

fn last_component(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// A new folder inherits the parent's sticky tag (`Create_Admin` writes `CVS/Tag`,
/// create_adm.c:170-171): without it updates in the new folder would use HEAD/trunk.
fn copy_sticky_tag(parent_local: &Path, new_dir: &WorkingDir) -> Result<()> {
    if let Some(tag) = dir::sticky_tag(parent_local) {
        std::fs::write(
            new_dir.cvs_dir().join("Tag"),
            format!("{}{}", tag, codec::newline()),
        )?;
    }
    Ok(())
}

fn normalize(text: &str) -> String {
    text.replace('\\', "/")
        .trim_matches('/')
        .trim_start_matches("./")
        .to_string()
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

    #[test]
    fn only_the_reference_keyword_modes_are_accepted() {
        for flag in ["kv", "kvl", "k", "v", "o", "b", "u"] {
            assert!(valid_kflag(flag), "{}", flag);
        }
        for flag in ["q", "foo", "kb2", ""] {
            assert!(!valid_kflag(flag), "{}", flag);
        }
    }

    #[test]
    fn dotdot_paths_are_counted_like_the_reference() {
        assert_eq!(pathname_levels("a/b.txt"), 0);
        assert_eq!(pathname_levels("../a.txt"), 1);
        assert_eq!(pathname_levels("../a/../../b"), 2);
        assert_eq!(pathname_levels("./a/../b"), 0);
    }

    #[test]
    fn a_dotted_name_is_parent_and_base() {
        assert_eq!(split_parent("dir/file.txt"), ("dir", "file.txt"));
        assert_eq!(split_parent("file.txt"), ("", "file.txt"));
        assert_eq!(split_parent("../dir/file.txt"), ("../dir", "file.txt"));
    }
}
