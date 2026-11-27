//! The part `status`, `log`, `diff`, `add` and `remove` share: which folders to name, which
//! files they hold, and what state each file is in.
//!
//! The protocol wants one `Directory`/`Entry` block per folder, followed by one state line per
//! file (`Unchanged`, `Modified` with its contents, or nothing for a missing file). The file
//! is then named again in the command's `Argument` list. Doing this in one place keeps the
//! commands from drifting apart.
//!
//! `add` is the one that starts from files CVS does not know: [`collect_add`] builds its plan
//! and [`send_add`] writes the blocks, which are `Is-modified` for a new file and
//! `Questionable` for a file inside a new folder.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::error::{Error, Result};
use crate::client::protocol::{self, Response};
use crate::client::work::codec;
use crate::client::work::dir::{self, WorkingDir};
use crate::client::work::entries::{Entries, Entry};

pub struct Target {
    pub dir: WorkingDir,
    pub entries: Entries,
    /// The names asked for in this folder; when `selected` is false only these entries are
    /// sent.
    pub filter: Vec<String>,
    /// The folder was named as a whole (no names, or a directory argument): every entry is
    /// sent and unknown files are reported.
    pub selected: bool,
    /// Names `remove` was given that have no `Entries` line. They are sent as `Modified`
    /// without an `Entry` line, or not at all when the file is already gone.
    pub loose: Vec<String>,
}

/// What one `add` will name, in the order the user typed the names.
pub enum AddItem {
    /// A file to schedule; `dir` is the key of the folder holding it.
    File { dir: String, name: String },
    /// A folder the repository does not know yet. `files` are sent as `Questionable`,
    /// which asks the server whether any of them already exists.
    Directory { dir: String, files: Vec<String> },
}

/// Everything an `add` needs: the index of every folder it touches, the names in the user's
/// order, and the new folders whose `CVS/` files are written once the server says `ok`.
pub struct AddPlan {
    /// The invocation folder; a file add opens with its `Directory .` block.
    pub top: WorkingDir,
    pub targets: BTreeMap<String, Target>,
    pub items: Vec<AddItem>,
    pub new_dirs: Vec<WorkingDir>,
}

impl AddPlan {
    fn folder(&self, key: &str) -> Option<&WorkingDir> {
        if key.is_empty() {
            return Some(&self.top);
        }
        self.targets
            .get(key)
            .map(|target| &target.dir)
            .or_else(|| self.new_dirs.iter().find(|dir| dir.update_dir == key))
    }
}

/// Work out which folders and files the answer should cover.
pub fn collect(cwd: &Path, recursive: bool, names: &[String]) -> Result<BTreeMap<String, Target>> {
    let expanded = crate::client::work::glob::expand_args(cwd, names);
    let names = expanded.as_slice();
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
        let mut dir = match dir::walk(parent, false)?.into_iter().next() {
            Some(dir) => dir,
            None => {
                return Err(Error::aborted(
                    "cvs",
                    format!("there is no version here for '{}'", name),
                ))
            }
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
        if entries.file(&file_name).is_none() {
            return Err(Error::aborted(
                "cvs",
                format!("{} is not a versioned file", name),
            ));
        }
        insert(&mut targets, dir, entries, vec![file_name]);
    }
    Ok(targets)
}

/// Work out what one `remove` will name. A file with no `Entries` line is still named: the
/// reference deletes it with `-f` and lets the server answer `nothing known about` otherwise,
/// so it becomes a loose name rather than a client-side error.
pub fn collect_remove(
    cwd: &Path,
    recursive: bool,
    names: &[String],
) -> Result<BTreeMap<String, Target>> {
    let expanded = crate::client::work::glob::expand_args(cwd, names);
    let names = expanded.as_slice();
    let mut known = Vec::new();
    let mut loose = Vec::new();
    for name in names {
        let path = cwd.join(name);
        if path.is_dir() {
            known.push(name.clone());
            continue;
        }
        let parent = path.parent().unwrap_or(cwd);
        let Some(mut dir) = dir::walk(parent, false)?.pop() else {
            return Err(Error::aborted(
                "remove",
                format!("there is no version here for '{}'", name),
            ));
        };
        // `walk` numbers a folder from itself; the protocol numbers it from the invocation
        // folder, so re-number it the way `collect` does.
        dir.update_dir = parent
            .strip_prefix(cwd)
            .ok()
            .map(|folder| normalize_relative(&folder.to_string_lossy()))
            .unwrap_or_default();
        let file_name = path
            .file_name()
            .map(|file| file.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());
        let entries = dir.entries()?;
        if entries.file(&file_name).is_some() {
            known.push(name.clone());
        } else {
            loose.push((dir, entries, file_name));
        }
    }

    let mut targets = if known.is_empty() && !names.is_empty() {
        BTreeMap::new()
    } else {
        collect(cwd, recursive, &known)?
    };
    for (dir, entries, name) in loose {
        let key = dir.update_dir.clone();
        targets
            .entry(key)
            .or_insert_with(|| Target {
                dir,
                entries,
                filter: Vec::new(),
                selected: false,
                loose: Vec::new(),
            })
            .loose
            .push(name);
    }
    Ok(targets)
}

/// Work out what one `add` will name. `options` is the entry's options field, e.g. `-kb`.
///
/// A file that is already in `Entries`, or is not on disk, cannot be added; both are the
/// reference client's complaints. A folder that already has `CVS/` is versioned already.
pub fn collect_add(cwd: &Path, names: &[String], options: &str) -> Result<AddPlan> {
    let expanded = crate::client::work::glob::expand_args(cwd, names);
    let names = expanded.as_slice();
    let repository = dir::read_repository(cwd)
        .ok_or_else(|| Error::aborted("add", "CVS/Repository is missing or empty"))?;
    let mut plan = AddPlan {
        top: WorkingDir::new(
            cwd.to_path_buf(),
            String::new(),
            repository,
            codec::for_dir(cwd),
        ),
        targets: BTreeMap::new(),
        items: Vec::new(),
        new_dirs: Vec::new(),
    };
    for name in names {
        let relative = normalize_relative(name);
        if relative.is_empty() || relative == "." {
            return Err(Error::plain("add", "cannot add the working folder itself"));
        }
        if cwd.join(&relative).is_dir() {
            add_directory(cwd, &relative, &mut plan)?;
        } else {
            add_file(cwd, &relative, options, &mut plan)?;
        }
    }
    Ok(plan)
}

fn add_file(cwd: &Path, relative: &str, options: &str, plan: &mut AddPlan) -> Result<()> {
    let (key, parent) = folder_for(cwd, relative)?;
    let name = base_name(relative).to_string();
    if !parent.local.join(&name).is_file() {
        return Err(Error::plain(
            "add",
            format!("nothing known about {}", relative),
        ));
    }
    {
        let target = target_for(plan, parent)?;
        if target.filter.iter().any(|existing| existing == &name) {
            // The same name twice in one command is one entry, not an error.
            return Ok(());
        }
        if let Some(entry) = target.entries.file(&name) {
            return Err(Error::plain(
                "add",
                format!(
                    "{} already exists, with version number {}",
                    relative, entry.revision
                ),
            ));
        }
        // The entry is written with `dummy timestamp`: the file is not in the repository
        // yet, but CVS still wants it to look changed to `commit`.
        target
            .entries
            .set_file(Entry::new(&name, "0", "dummy timestamp", options, ""));
        target.filter.push(name.clone());
    }
    plan.items.push(AddItem::File { dir: key, name });
    Ok(())
}

fn add_directory(cwd: &Path, relative: &str, plan: &mut AddPlan) -> Result<()> {
    let path = cwd.join(relative);
    if path.join("CVS").is_dir() {
        return Err(Error::plain(
            "add",
            format!("{}/CVS already exists", relative),
        ));
    }
    let (_, parent) = folder_for(cwd, relative)?;
    let name = base_name(relative).to_string();
    let repository = format!("{}/{}", parent.repository.trim_end_matches('/'), name);
    let working = WorkingDir::new(path, relative.to_string(), repository, parent.codec);
    let files = unknown_files(&working.local, &Entries::default());
    {
        let target = target_for(plan, parent)?;
        target.entries.set_directory(&name);
    }
    plan.new_dirs.push(working);
    plan.items.push(AddItem::Directory {
        dir: relative.to_string(),
        files,
    });
    Ok(())
}

/// The folder a relative name lives in, keyed the way the protocol numbers it.
fn folder_for(cwd: &Path, relative: &str) -> Result<(String, WorkingDir)> {
    let key = match relative.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    };
    let local = cwd.join(&key);
    let repository = dir::read_repository(&local).ok_or_else(|| {
        Error::aborted(
            "add",
            format!("there is no version here for '{}'", relative),
        )
    })?;
    let codec = codec::for_dir(&local);
    Ok((key.clone(), WorkingDir::new(local, key, repository, codec)))
}

fn target_for(plan: &mut AddPlan, working: WorkingDir) -> Result<&mut Target> {
    let key = working.update_dir.clone();
    if !plan.targets.contains_key(&key) {
        let entries = Entries::read(&working.local)?;
        plan.targets.insert(
            key.clone(),
            Target {
                dir: working,
                entries,
                filter: Vec::new(),
                selected: false,
                loose: Vec::new(),
            },
        );
    }
    Ok(plan
        .targets
        .get_mut(&key)
        .expect("the target was just inserted"))
}

fn base_name(relative: &str) -> &str {
    relative
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(relative)
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

/// The blocks of `watch` and `edit -c`: each file's `Entry`, then `Unchanged` or
/// `Is-modified` — never its contents, because neither command compares them. Unknown files
/// in a folder the command selected as a whole are sent as `Questionable` but not listed, as
/// the reference does.
pub fn send_blocks_watch(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
) -> Result<Option<String>> {
    let mut last = None;
    for target in targets.values() {
        let absolute =
            dir::absolute_repository(session.root.protocol_root(), &target.dir.repository);
        let local = display_dir(&target.dir.update_dir);
        session.directory(&local, &absolute, &target.dir.local)?;
        let unknown = if target.selected {
            unknown_files(&target.dir.local, &target.entries)
        } else {
            Vec::new()
        };
        for entry in target.entries.files() {
            if !target.selected && !target.filter.iter().any(|name| name == &entry.name) {
                continue;
            }
            session.send(&format!("Entry {}", protocol_entry(entry)))?;
            if entry.is_removed() {
                continue;
            }
            let file = target.dir.local.join(&entry.name);
            if !file.is_file() {
                continue;
            }
            if entry.is_added()
                || entry.is_conflict()
                || entry.is_unfinished()
                || !matches_recorded_time(&file, &entry.date)
            {
                session.send(&format!("Is-modified {}", entry.name))?;
            } else {
                session.send(&format!("Unchanged {}", entry.name))?;
            }
        }
        for name in unknown {
            session.send(&format!("Questionable {}", name))?;
        }
        last = Some(target.dir.update_dir.clone());
    }
    Ok(last)
}

/// Send every `Directory` block and every file's state.
pub fn send_blocks(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
    report_unknown: bool,
) -> Result<()> {
    send_blocks_tracked(session, targets, report_unknown).map(|_| ())
}

/// The same blocks, answering the last folder named. `add` and `remove` use it to decide
/// whether the command still has to return to the invocation folder.
pub fn send_blocks_tracked(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
    report_unknown: bool,
) -> Result<Option<String>> {
    send_blocks_with(session, targets, report_unknown, report_unknown)
}

/// The blocks of `tag`: unknown files are sent as `Questionable` but not listed. The
/// reference's `tag` says nothing about them, unlike `status`, which prints `?` for each.
pub fn send_blocks_quiet(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
) -> Result<Option<String>> {
    send_blocks_with(session, targets, true, false)
}

fn send_blocks_with(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
    report_unknown: bool,
    list_unknown: bool,
) -> Result<Option<String>> {
    let mut last = None;
    for target in targets.values() {
        let absolute =
            dir::absolute_repository(session.root.protocol_root(), &target.dir.repository);
        let local = display_dir(&target.dir.update_dir);
        session.directory(&local, &absolute, &target.dir.local)?;
        let unknown = if report_unknown && target.selected {
            if list_unknown {
                print_unknown(session, &target.dir, &target.entries)
            } else {
                unknown_files(&target.dir.local, &target.entries)
            }
        } else {
            Vec::new()
        };
        for entry in target.entries.files() {
            if !target.selected && !target.filter.iter().any(|name| name == &entry.name) {
                continue;
            }
            send_file_state(session, &target.dir, entry)?;
        }
        // The reference client names files it knows nothing about after the known ones.
        for name in unknown {
            session.send(&format!("Questionable {}", name))?;
        }
        for name in &target.loose {
            send_loose_state(session, &target.dir, name)?;
        }
        last = Some(target.dir.update_dir.clone());
    }
    Ok(last)
}

/// Send the blocks of an `add`: the invocation folder first when any file is named, then one
/// block per folder as the names move between them, and a last `Directory .` when the blocks
/// ended below it. A new folder's block carries `Questionable` lines instead of file states.
pub fn send_add(session: &mut protocol::Session, plan: &AddPlan) -> Result<()> {
    let mut last = None;
    if plan
        .items
        .iter()
        .any(|item| matches!(item, AddItem::File { .. }))
    {
        send_directory(session, &plan.top)?;
        last = Some(String::new());
    }
    for item in &plan.items {
        match item {
            AddItem::File { dir, name } => {
                if last.as_deref() != Some(dir.as_str()) {
                    send_named_directory(session, plan, dir)?;
                    last = Some(dir.clone());
                }
                session.send(&format!("Is-modified {}", name))?;
            }
            AddItem::Directory { dir, files } => {
                send_named_directory(session, plan, dir)?;
                for name in files {
                    session.send(&format!("Questionable {}", name))?;
                }
                last = Some(dir.clone());
            }
        }
    }
    if last.as_deref().is_some_and(|key| !key.is_empty()) {
        send_directory(session, &plan.top)?;
    }
    Ok(())
}

fn send_named_directory(session: &mut protocol::Session, plan: &AddPlan, key: &str) -> Result<()> {
    let folder = plan.folder(key).ok_or_else(|| {
        Error::aborted(
            "add",
            format!("the folder '{}' is missing from the request", key),
        )
    })?;
    send_directory(session, folder)
}

fn send_directory(session: &mut protocol::Session, dir: &WorkingDir) -> Result<()> {
    let absolute = dir::absolute_repository(session.root.protocol_root(), &dir.repository);
    session.directory(&display_dir(&dir.update_dir), &absolute, &dir.local)
}

/// Finish a command from the invocation folder: `Directory .`, the arguments the user
/// typed, then the command name.
pub fn finish_to_top(
    session: &mut protocol::Session,
    cwd: &Path,
    names: &[String],
    command: &str,
) -> Result<()> {
    send_top_directory(session, cwd, command)?;
    finish_arguments(session, names, command)
}

/// The same finish, but only when the blocks did not end at the invocation folder. The
/// reference client leaves the duplicate `Directory .` out of `add` and `remove`.
pub fn finish_from_last(
    session: &mut protocol::Session,
    cwd: &Path,
    names: &[String],
    command: &str,
    last: Option<&str>,
) -> Result<()> {
    if last.is_some_and(|update_dir| !update_dir.is_empty()) {
        send_top_directory(session, cwd, command)?;
    }
    finish_arguments(session, names, command)
}

fn send_top_directory(session: &mut protocol::Session, cwd: &Path, command: &str) -> Result<()> {
    let top_repo = dir::read_repository(cwd)
        .ok_or_else(|| Error::aborted(command, "CVS/Repository is missing or empty"))?;
    let top_absolute = dir::absolute_repository(session.root.protocol_root(), &top_repo);
    session.directory(".", &top_absolute, cwd)
}

/// The options and the `--` that end them go before any folder block. The reference sends
/// `Argument --` first, then `Max-dotdot` inside `send_files` (client.c:5606-5616).
pub fn begin_files(session: &mut protocol::Session, names: &[String]) -> Result<()> {
    session.argument("--")?;
    let levels = names
        .iter()
        .map(|name| pathname_levels(name))
        .max()
        .unwrap_or(0);
    if levels > 0 {
        if session.supports("Max-dotdot") {
            session.send(&format!("Max-dotdot {}", levels))?;
        } else {
            return Err(Error::aborted(
                &session.command,
                "backreference in path (`..') not supported by old (pre-Max-dotdot) servers",
            ));
        }
    }
    Ok(())
}

/// The `Argument` lines for the names as typed, then the command name.
pub fn finish_arguments(
    session: &mut protocol::Session,
    names: &[String],
    command: &str,
) -> Result<()> {
    for name in names {
        session.argument(&normalize_relative(name))?;
    }
    session.send(command)?;
    session.flush()
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

/// Read the text-only answer of `status`, `log` or `diff`, printing it the way cvs does.
/// Returns whether any text was printed, which is how `diff` decides its exit code.
pub fn read_text(session: &mut protocol::Session, suppress_unknown_marks: bool) -> Result<bool> {
    read_text_with(session, suppress_unknown_marks, false)
}

/// The same, but an empty `error` ends the command successfully. Only `diff`/`rdiff` use
/// this: CVSNT ends a diff that found differences with an empty `error` instead of `ok`.
/// Every other command must treat any `error` as a failure, as the reference client does.
pub fn read_text_allow_empty_error(
    session: &mut protocol::Session,
    suppress_unknown_marks: bool,
) -> Result<bool> {
    read_text_with(session, suppress_unknown_marks, true)
}

fn read_text_with(
    session: &mut protocol::Session,
    suppress_unknown_marks: bool,
    allow_empty_error: bool,
) -> Result<bool> {
    let mut printed = false;
    loop {
        match session.read_response()? {
            Response::Ok => return Ok(printed),
            Response::E(text) => session.err(text),
            Response::Notified | Response::Trace(_) | Response::Checksum(_) => {}
            Response::M(text) if suppress_unknown_marks && text.starts_with("? ") => {}
            Response::M(text) => {
                printed = true;
                session.out(text);
            }
            Response::Error { message, .. } if allow_empty_error && message.trim().is_empty() => {
                // CVSNT's normal ending for a diff that found differences.
                session.err(String::new());
                return Ok(printed);
            }
            Response::Error { message, .. } => {
                return Err(Error::aborted(&session.command, message))
            }
            other => {
                return Err(Error::aborted(
                    &session.command,
                    format!("unexpected answer: {:?}", other),
                ))
            }
        }
    }
}

pub fn display_dir(update_dir: &str) -> String {
    if update_dir.is_empty() {
        ".".to_string()
    } else {
        update_dir.to_string()
    }
}

fn print_unknown(
    session: &mut protocol::Session,
    dir: &WorkingDir,
    entries: &Entries,
) -> Vec<String> {
    let unknown = unknown_files(&dir.local, entries);
    for name in &unknown {
        session.out(format!("? {}", dir::display_path(&dir.update_dir, name)));
    }
    unknown
}

/// The regular files in a folder that are not in `Entries`, sorted the way cvs prints them.
pub(crate) fn unknown_files(local: &Path, entries: &Entries) -> Vec<String> {
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
        if name == ".cvsignore" || name == "CVS" {
            continue;
        }
        if entries.file(&name).is_none() {
            unknown.push(name);
        }
    }
    unknown.sort();
    unknown
}

/// The folder key a protocol update directory names: `./` and `.` are the invocation folder.
pub fn target_key(update_dir: &str) -> String {
    let key = update_dir.trim_matches('/');
    if key == "." {
        String::new()
    } else {
        key.to_string()
    }
}

/// `d/two.txt` → (`d`, `two.txt`); `two.txt` → ("", `two.txt`).
pub fn split_local(path: &str) -> (String, String) {
    let path = path.replace('\\', "/");
    let path = path.trim_start_matches("./");
    match path.rsplit_once('/') {
        Some((dir, name)) => (dir.trim_matches('/').to_string(), name.to_string()),
        None => (String::new(), path.to_string()),
    }
}

fn send_file_state(session: &mut protocol::Session, dir: &WorkingDir, entry: &Entry) -> Result<()> {
    session.send(&format!("Entry {}", protocol_entry(entry)))?;
    let local = dir.local.join(&entry.name);

    if entry.is_removed() {
        // The file is recorded as removed and may already be gone; the entry alone says it.
        return Ok(());
    }

    let metadata = match std::fs::metadata(&local) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A missing file is not "Questionable" (that word is for files CVS has never
            // heard of). The entry alone is enough; the server answers "Needs Checkout".
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let changed = entry.is_added()
        || entry.is_conflict()
        || entry.is_unfinished()
        || !matches_recorded_time(&local, &entry.date);

    if !changed {
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

/// A name `remove` was given that has no entry: the reference sends its contents as
/// `Modified` without an `Entry` line, or nothing when the file is already gone. There is no
/// recorded mode or timestamp to consult, so the file's own read-only bit decides the mode and
/// the bytes are treated as text.
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

/// The entry as the protocol carries it. The recorded mtime is not sent; CVS sends only the
/// conflict marker `+=`. The `Needs Patch` date shape is ambiguous in the slash-separated
/// index, so this client sends it empty and lets the `Modified` state carry the news.
pub fn protocol_entry(entry: &Entry) -> String {
    let date = if entry.is_conflict() {
        "+=".to_string()
    } else {
        String::new()
    };
    format!(
        "/{}/{}/{}/{}/{}",
        entry.name, entry.revision, date, entry.options, entry.tag
    )
}

/// Does the file still have the time CVS recorded? Both sides are UTC, as CVSNT writes them.
fn matches_recorded_time(local: &Path, recorded: &str) -> bool {
    if recorded.trim().is_empty() {
        return false;
    }
    let Some(modified) = codec::file_mtime(local) else {
        return false;
    };
    codec::timestamps_equal(&codec::asctime(modified), recorded)
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

    #[test]
    fn a_normal_entry_is_sent_without_its_time() {
        let entry = Entry::new("a.txt", "1.2", "Tue Nov 24 09:52:53 2026", "", "TAG");
        assert_eq!(protocol_entry(&entry), "/a.txt/1.2///TAG");
    }

    #[test]
    fn a_conflict_is_sent_as_the_plus_marker() {
        let entry = Entry::new(
            "a.txt",
            "1.2",
            "Result of merge+Tue Nov 24 09:52:53 2026",
            "",
            "",
        );
        assert_eq!(protocol_entry(&entry), "/a.txt/1.2/+=//");
    }

    #[test]
    fn an_unfinished_update_is_still_sent_as_modified_without_a_bad_index() {
        let entry = Entry::new("a.txt", "1.2", "/patched", "", "");
        assert_eq!(protocol_entry(&entry), "/a.txt/1.2///");
    }
}
