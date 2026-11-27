//! `cvs update`: bring the working copy in line with the repository.
//!
//! The request is the one the reference client sends: `-u` (only when the server offers
//! `update-patches`), the command's options in the reference order, `Argument --`, the folder
//! blocks and file states, then the file names and `update`. The server answers with the same
//! file shapes `checkout` uses, plus `Copy-file` (save the working file before a merge),
//! `Merged` (the merged contents), `Removed`/`Remove-entry` and the sticky-tag changes.
//!
//! This client does not advertise `Rcs-diff`, so the server sends whole files instead of RCS
//! diffs. That is a legitimate CVS configuration (it is what a client without patch support
//! gets) and it removes the need for an external patch program.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::client::commands::targets::{self, Target};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::ignore::Ignore;
use crate::client::protocol::{self, FileKind, Response};
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::dir;
use crate::client::work::entries::{Entries, Entry};
use crate::client::work::writer::WorkWriter;

#[derive(Debug, Default)]
struct Options {
    recursive: bool,
    /// The global `-n`: report, but do not change a disk. CVSNT's own `update` refuses `-n`,
    /// but the desktop program asks for it, so it is accepted here and behaves like the
    /// global option.
    no_execute: bool,
    /// `-d`: create folders the repository has but the working copy lacks.
    build_dirs: bool,
    /// `-p`: send the file bodies to standard output and change nothing.
    pipeout: bool,
    /// `-f`: fall back to the head revision when a tag/date does not match.
    force: bool,
    /// `-A`: reset sticky tags, dates and `-k` options.
    reset_sticky: bool,
    /// `-C`: replace locally modified files with clean repository copies, saving the old file.
    toss_local: bool,
    /// `-P`: remove folders that became empty.
    prune: bool,
    /// `-k kopt`, already shaped as the server's argument (`-kb`, `-kk`, …).
    kopt: Option<String>,
    /// `-r rev`.
    revision: Option<String>,
    /// `-D date`.
    date: Option<String>,
    /// Every `-j rev`, in order.
    joins: Vec<String>,
    /// Every `-I ign`. These stay local: the protocol does not carry ignore patterns.
    ignores: Vec<String>,
    /// Every `-W spec`. Wrapper lines go to the server as `Argument -W` + the line.
    wrappers: Vec<String>,
    names: Vec<String>,
}

/// Expand clustered short options (`-Pd` → `-P -d`), keeping the value of an option that
/// takes one (`-r1.1` → `-r 1.1`, `-dout` is not a cluster because `d` takes no value).
fn expand_short_options(args: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut names_only = false;
    while i < args.len() {
        let arg = &args[i];
        if names_only || !arg.starts_with('-') || arg == "-" {
            out.push(arg.clone());
        } else if arg == "--" {
            names_only = true;
            out.push(arg.clone());
        } else {
            let letters: Vec<char> = arg[1..].chars().collect();
            let mut j = 0;
            while j < letters.len() {
                let letter = letters[j];
                if update_option_takes_value(letter) {
                    out.push(format!("-{}", letter));
                    let rest: String = letters[j + 1..].iter().collect();
                    if rest.is_empty() {
                        i += 1;
                        let value = args.get(i).ok_or_else(|| {
                            Error::plain(
                                "update",
                                format!("option requires an argument -- '{}'", letter),
                            )
                        })?;
                        out.push(value.clone());
                    } else {
                        out.push(rest);
                    }
                    break;
                }
                if !update_option_is_known(letter) {
                    return Err(Error::plain(
                        "update",
                        format!("invalid option -- '{}'", letter),
                    ));
                }
                out.push(format!("-{}", letter));
                j += 1;
            }
        }
        i += 1;
    }
    Ok(out)
}

fn update_option_is_known(letter: char) -> bool {
    matches!(
        letter,
        'A' | 'p'
            | 'C'
            | 'P'
            | 'f'
            | 'l'
            | 'R'
            | 'Q'
            | 'q'
            | 'd'
            | 'u'
            | 'n'
            | 'k'
            | 'r'
            | 'D'
            | 'j'
            | 'I'
            | 'W'
    )
}

fn update_option_takes_value(letter: char) -> bool {
    matches!(letter, 'k' | 'r' | 'D' | 'j' | 'I' | 'W')
}

fn parse_args(args: &[String]) -> Result<Options> {
    let expanded = expand_short_options(args)?;
    let mut opts = Options {
        recursive: true,
        ..Default::default()
    };
    let mut i = 0;
    while i < expanded.len() {
        let arg = expanded[i].as_str();
        match arg {
            "--" => {
                opts.names.extend(expanded[i + 1..].iter().cloned());
                break;
            }
            "-l" => opts.recursive = false,
            "-R" => opts.recursive = true,
            "-n" => opts.no_execute = true,
            "-d" => opts.build_dirs = true,
            "-p" => opts.pipeout = true,
            "-f" => opts.force = true,
            "-A" => opts.reset_sticky = true,
            "-C" => opts.toss_local = true,
            "-P" => opts.prune = true,
            // The reference accepts these only in server mode; on the command line they are
            // an error because the global options belong before the command name.
            "-q" | "-Q" => {
                return Err(Error::plain(
                    "update",
                    "-q or -Q must be specified before \"update\"",
                ))
            }
            "-k" => {
                let kopt = take_value(&expanded, &mut i, "-k")?;
                opts.kopt = Some(format!("-k{}", kopt));
            }
            "-r" => opts.revision = Some(take_value(&expanded, &mut i, "-r")?),
            "-D" => opts.date = Some(take_value(&expanded, &mut i, "-D")?),
            "-j" => {
                let rev = take_value(&expanded, &mut i, "-j")?;
                if opts.joins.len() >= 2 {
                    return Err(Error::plain(
                        "update",
                        "only two -j options can be specified",
                    ));
                }
                opts.joins.push(rev);
            }
            "-I" => {
                let pattern = take_value(&expanded, &mut i, "-I")?;
                opts.ignores.push(pattern);
            }
            "-W" => {
                let spec = take_value(&expanded, &mut i, "-W")?;
                opts.wrappers.push(spec);
            }
            "-u" => {
                return Err(Error::plain(
                    "update",
                    "the -u option is only for server mode",
                ))
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "update",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.names.push(other.to_string()),
        }
        i += 1;
    }
    Ok(opts)
}

fn take_value(expanded: &[String], i: &mut usize, option: &str) -> Result<String> {
    *i += 1;
    expanded
        .get(*i)
        .cloned()
        .ok_or_else(|| Error::plain("update", format!("option {} requires an argument", option)))
}

/// A wrapper line from `~/.cvswrappers`, `$CVSWRAPPERS` or `-W`, reduced to what the protocol
/// can carry: the file name pattern and its `-k` substitution. Filters (`-t`/`-f`) and a
/// `-m COPY` merge method have no remote effect; the reference warns about each.
#[derive(Debug, Default, PartialEq, Eq)]
struct Wrapper {
    pattern: String,
    kopt: Option<String>,
    copy: bool,
    filter: bool,
}

impl Wrapper {
    /// The line `wrap_send` puts on the wire: `<pattern> -k '<kopt>'`.
    fn protocol_line(&self) -> String {
        format!(
            "{} -k '{}'",
            self.pattern,
            self.kopt.as_deref().unwrap_or("kv")
        )
    }
}

fn parse_wrapper(line: &str) -> Option<Wrapper> {
    let text = line.trim();
    if text.is_empty() || text.starts_with('#') {
        return None;
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let mut wrapper = Wrapper {
        pattern: text[..i].to_string(),
        ..Default::default()
    };
    while i < bytes.len() {
        if bytes[i] != b'-' {
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            break;
        }
        let option = bytes[i] as char;
        i += 1;
        while i < bytes.len() && bytes[i] != b'\'' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        i += 1;
        let mut value = String::new();
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if i + 1 < bytes.len() => {
                    i += 1;
                    value.push(bytes[i] as char);
                    i += 1;
                }
                b'\'' => {
                    i += 1;
                    break;
                }
                byte => {
                    value.push(byte as char);
                    i += 1;
                }
            }
        }
        match option {
            'k' => {
                wrapper.kopt = if value == "kv" { None } else { Some(value) };
            }
            'm' => wrapper.copy = value.starts_with(['C', 'c']),
            't' | 'f' => wrapper.filter = true,
            _ => {}
        }
    }
    Some(wrapper)
}

/// Read the wrapper lists the reference reads for a remote `update`: `~/.cvswrappers`, then
/// `$CVSWRAPPERS`, then the `-W` lines the user typed. `CVSROOT/cvswrappers` lives on the
/// server and is not readable here, so the server's own list still applies there.
fn wrapper_lines(cwd: &Path, explicit: &[String]) -> Vec<Wrapper> {
    let _ = cwd;
    let mut lines = Vec::new();
    if let Some(home) = home_dir() {
        if let Ok(text) = std::fs::read_to_string(home.join(".cvswrappers")) {
            lines.extend(text.lines().map(|line| line.to_string()));
        }
    }
    if let Ok(text) = std::env::var("CVSWRAPPERS") {
        lines.extend(text.lines().map(|line| line.to_string()));
    }
    lines.extend(explicit.iter().cloned());
    lines
        .iter()
        .filter_map(|line| parse_wrapper(line))
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Run `cvs update`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    readonly: bool,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = parse_args(args)?;
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "update",
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("update", "no files to update"));
    }

    let mut root = root;
    // The desktop program's `update -n` is an alias for the global option; let the session
    // send it once as `Global_option -n`.
    root.options.no_execute |= opts.no_execute;
    let no_write = root.options.no_execute || opts.pipeout;
    // A preview is a real `-n` update, not `-p`'s internal "change nothing".
    let preview = root.options.no_execute && !opts.pipeout;

    let wrappers = wrapper_lines(cwd, &opts.wrappers);
    for wrapper in &wrappers {
        if wrapper.filter {
            sink.err(
                "cvs update: -t and -f wrapper options are not supported remotely; ignored"
                    .to_string(),
            );
        }
        if wrapper.copy {
            sink.err(
                "cvs update: -m wrapper option is not supported remotely; ignored".to_string(),
            );
        }
    }

    let ignore = Ignore::new(&opts.ignores);
    let mut session = protocol::open(root, "update", cancel, sink)?;

    // `-u` is the ordinary "update the working file" mode; the reference sends it only when
    // the server advertised `update-patches`.
    if session.supports("update-patches") {
        session.argument("-u")?;
    }
    send_options(&mut session, &opts, &wrappers)?;
    // `--` must come before any folder block, and after the command's own options.
    targets::begin_files(&mut session, &opts.names)?;
    send_update_blocks(&mut session, &targets, &opts, &ignore, no_write)?;
    targets::finish_to_top(&mut session, cwd, &opts.names, "update")?;

    let mut writer = WorkWriter::new(cwd, session.root.clone())
        .readonly(readonly)
        .dry_run(no_write);
    let mut pending_mtime = None;
    let mut changed = false;
    let mut conflicts = false;
    let mut saw_report = false;
    let mut seen_dirs: BTreeSet<String> = BTreeSet::new();
    // (folder, file) → did the merge conflict? `Merged` entries are `Result of merge` once
    // the response loop is over; a later response for the same file wins.
    let mut merges: BTreeMap<(String, String), bool> = BTreeMap::new();
    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => {
                // U/M/C/R/P/? all mean the working copy differs from the repository.
                // With `-p` the server sends the file bodies themselves through `M`; they
                // are output, not a report of a change.
                let report = preview && text.len() > 1;
                session.out(text);
                saw_report = true;
                if report {
                    changed = true;
                }
            }
            Response::ModTime(text) => pending_mtime = codec::parse_mod_time(&text),
            Response::File {
                kind,
                update_dir,
                repository,
                entry,
                mode,
                content,
            } => {
                let key = normalize_update_dir(&update_dir);
                seen_dirs.insert(key.clone());
                let parsed = Entry::parse(&entry);
                if let Some(parsed) = &parsed {
                    let path_key = (key.clone(), parsed.name.clone());
                    match kind {
                        FileKind::Merged => {
                            let conflict = is_conflict_entry(parsed);
                            merges.insert(path_key, conflict);
                            if conflict {
                                conflicts = true;
                            }
                        }
                        _ => {
                            merges.remove(&path_key);
                        }
                    }
                }
                if kind == FileKind::Created {
                    if let Some(parsed) = &parsed {
                        let local = writer.local_for(&update_dir).join(&parsed.name);
                        if local.exists() {
                            let shown = dir::display_path(&update_dir, &parsed.name);
                            session
                                .err(format!("cvs update: move away {}; it is in the way", shown));
                            conflicts = true;
                            saw_report = true;
                            pending_mtime = None;
                            continue;
                        }
                    }
                }
                if kind == FileKind::UpdateExisting {
                    if let Some(parsed) = &parsed {
                        let local = writer.local_for(&update_dir).join(&parsed.name);
                        if !local.is_file() {
                            session.err(format!(
                                "cvs update: warning: {} unexpectedly disappeared",
                                dir::display_path(&update_dir, &parsed.name)
                            ));
                        }
                    }
                }
                if kind == FileKind::Patched {
                    writer.write_patch(
                        &update_dir,
                        &repository,
                        &entry,
                        &mode,
                        &content,
                        pending_mtime,
                    )?;
                } else {
                    writer.write_file(
                        kind,
                        &update_dir,
                        &repository,
                        &entry,
                        &mode,
                        &content,
                        pending_mtime,
                    )?;
                }
                pending_mtime = None;
                changed = true;
                saw_report = true;
            }
            Response::CopyFile {
                update_dir,
                repository,
                name,
            } => {
                seen_dirs.insert(normalize_update_dir(&update_dir));
                writer.copy_file(&update_dir, &repository, &name)?;
                changed = true;
                saw_report = true;
            }
            Response::Removed(path) => {
                let (dir_name, name) = split_local(&path);
                seen_dirs.insert(dir_name.clone());
                writer.remove_file(&dir_name, &name)?;
                // `Removed` deletes the file *and* its `Entries` line; `Remove-entry` only
                // drops the line (client.c:2411-2426).
                writer.remove_entry(&dir_name, &name)?;
                changed = true;
                saw_report = true;
            }
            Response::RemoveEntry(path) => {
                let (dir_name, name) = split_local(&path);
                seen_dirs.insert(dir_name.clone());
                writer.remove_entry(&dir_name, &name)?;
                changed = true;
                saw_report = true;
            }
            Response::Template { dir, content } => {
                writer.write_template(&dir, &content)?;
            }
            Response::SetStaticDirectory { dir, repository } => {
                seen_dirs.insert(normalize_update_dir(&dir));
                writer.set_static_directory(&dir, &repository)?;
            }
            Response::ClearStaticDirectory { dir, repository } => {
                seen_dirs.insert(normalize_update_dir(&dir));
                writer.clear_static_directory(&dir, &repository)?;
            }
            Response::SetSticky {
                dir,
                repository,
                tag,
            } => {
                seen_dirs.insert(normalize_update_dir(&dir));
                writer.set_sticky(&dir, &repository, &tag)?;
            }
            Response::ClearSticky { dir, repository } => {
                seen_dirs.insert(normalize_update_dir(&dir));
                writer.clear_sticky(&dir, &repository)?;
            }
            Response::Error { message, .. } => {
                // CVSNT ends a `Patched` update with an empty `error` instead of `ok`. The
                // file has already been written and the entry recorded, so this is a
                // successful end, not a failure.
                let empty_end = message.trim().is_empty() && (changed || saw_report);
                writer.finish()?;
                finish_working_copy(cwd, &targets, &seen_dirs, &merges, &opts, no_write)?;
                session.close();
                if empty_end {
                    return Ok(if conflicts || (preview && changed) {
                        1
                    } else {
                        0
                    });
                }
                return Err(Error::aborted(
                    "update",
                    if message.trim().is_empty() {
                        "the server reported an error".to_string()
                    } else {
                        message
                    },
                ));
            }
            Response::CheckedIn {
                update_dir,
                repository,
                entry,
            } => {
                seen_dirs.insert(normalize_update_dir(&update_dir));
                writer.record_entry(&update_dir, &repository, &entry)?;
            }
            Response::NewEntry {
                update_dir,
                repository,
                entry,
            } => {
                seen_dirs.insert(normalize_update_dir(&update_dir));
                // `update -A`/`-r` changes a file's sticky tag without moving its contents.
                // The server's line carries no date; the reference marks the file with a
                // dummy timestamp so `commit` still sees it as locally modified.
                writer.record_entry(&update_dir, &repository, &new_entry_line(&entry))?;
            }
            Response::Mode(_)
            | Response::F(_)
            | Response::ValidRequests(_)
            | Response::ModuleExpansion(_)
            | Response::Notified
            | Response::Trace(_)
            | Response::Checksum(_) => {}
        }
    }

    writer.finish()?;
    finish_working_copy(cwd, &targets, &seen_dirs, &merges, &opts, no_write)?;
    session.close();
    // `cvs -n update` exits 1 when there would have been something to do; a conflict also
    // makes the update fail, as it does in the reference client.
    Ok(if conflicts || (preview && changed) {
        1
    } else {
        0
    })
}

/// The command's options, in the order the reference client puts them on the wire.
fn send_options(
    session: &mut protocol::Session,
    opts: &Options,
    wrappers: &[Wrapper],
) -> Result<()> {
    if !opts.recursive {
        session.argument("-l")?;
    }
    if opts.build_dirs {
        session.argument("-d")?;
    }
    if opts.pipeout {
        session.argument("-p")?;
    }
    if opts.force {
        session.argument("-f")?;
    }
    if opts.reset_sticky {
        session.argument("-A")?;
    }
    if opts.toss_local {
        session.argument("-C")?;
    }
    if opts.prune {
        session.argument("-P")?;
    }
    if let Some(revision) = &opts.revision {
        session.argument("-r")?;
        session.argument(revision)?;
    }
    if let Some(kopt) = &opts.kopt {
        session.argument(kopt)?;
    }
    if let Some(date) = &opts.date {
        session.argument("-D")?;
        session.argument(date)?;
    }
    for join in &opts.joins {
        session.argument("-j")?;
        session.argument(join)?;
    }
    for wrapper in wrappers {
        session.argument("-W")?;
        session.argument(&wrapper.protocol_line())?;
    }
    Ok(())
}

/// Send the folder blocks and each file's state, the way `send_files` does.
///
/// Unknown files in a folder the user selected as a whole are sent as `Questionable` (the
/// server answers `? file`), unless the local ignore list cannot be expressed to the server
/// or the server has no `Questionable` request; then the `?` line is printed here. `-I`,
/// `$CVSIGNORE`, `~/.cvsignore` and each folder's `.cvsignore` decide what counts as unknown.
fn send_update_blocks(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
    opts: &Options,
    ignore: &Ignore,
    no_write: bool,
) -> Result<()> {
    for target in targets.values() {
        let absolute =
            dir::absolute_repository(session.root.protocol_root(), &target.dir.repository);
        let local = targets::display_dir(&target.dir.update_dir);
        session.directory(&local, &absolute, &target.dir.local)?;
        let folder_ignore = ignore.enter(&target.dir.local);
        let unknown = if target.selected {
            unknown_files(&target.dir.local, &target.entries)
        } else {
            Vec::new()
        };
        let mut questionable = Vec::new();
        for name in unknown {
            if folder_ignore.is_ignored(&name) {
                continue;
            }
            questionable.push(name);
        }
        for entry in target.entries.files() {
            if !target.selected && !target.filter.iter().any(|name| name == &entry.name) {
                continue;
            }
            send_file_state(session, &target.dir, entry, opts, no_write)?;
        }
        let can_question = session.supports("Questionable") && !folder_ignore.inhibits_server();
        for name in questionable {
            if can_question {
                session.send(&format!("Questionable {}", name))?;
            } else {
                session.out(format!(
                    "? {}",
                    dir::display_path(&target.dir.update_dir, &name)
                ));
            }
        }
    }
    Ok(())
}

fn send_file_state(
    session: &mut protocol::Session,
    folder: &dir::WorkingDir,
    entry: &Entry,
    opts: &Options,
    no_write: bool,
) -> Result<()> {
    session.send(&format!("Entry {}", wire_entry(entry)))?;
    let local = folder.local.join(&entry.name);
    if entry.is_removed() {
        return Ok(());
    }
    let metadata = match std::fs::metadata(&local) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A missing file is not `Questionable`; the entry alone is enough.
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let changed = entry_changed(&local, entry);
    if changed && opts.toss_local {
        backup_modified_file(session, folder, entry, no_write);
    }
    if !changed {
        session.send(&format!("Unchanged {}", entry.name))?;
        return Ok(());
    }
    if opts.toss_local && session.supports("Is-modified") {
        session.send(&format!("Is-modified {}", entry.name))?;
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

/// `-C`: copy the modified working file to `.#name.rev` before the clean copy arrives and
/// say so, exactly like the reference client (`client.c:5188-5198`).
fn backup_modified_file(
    session: &mut protocol::Session,
    folder: &dir::WorkingDir,
    entry: &Entry,
    no_write: bool,
) {
    let local = folder.local.join(&entry.name);
    if !local.is_file() {
        return;
    }
    let backup_name = if entry.revision.is_empty() {
        format!(".#{}", entry.name)
    } else {
        format!(".#{}.{}", entry.name, entry.revision)
    };
    if !no_write {
        let backup = folder.local.join(&backup_name);
        let _ = dir::clear_readonly(&backup);
        if std::fs::copy(&local, &backup).is_err() {
            return;
        }
    }
    session.out(format!(
        "(Locally modified {} moved to {})",
        entry.name, backup_name
    ));
}

/// The entry as the protocol carries it. A CVS date of `Result of merge` is a *clean* merge
/// and must not be sent as the conflict marker; only a conflict (`Result of merge+time`, or
/// the server's `+=`) is `+=`.
fn wire_entry(entry: &Entry) -> String {
    let date = if is_conflict_entry(entry) {
        "+=".to_string()
    } else {
        String::new()
    };
    format!(
        "/{}/{}/{}/{}/{}",
        entry.name, entry.revision, date, entry.options, entry.tag
    )
}

fn is_conflict_entry(entry: &Entry) -> bool {
    let date = entry.date.trim();
    date.starts_with('+') || date.starts_with("Result of merge+")
}

fn entry_changed(local: &Path, entry: &Entry) -> bool {
    entry.is_added()
        || entry.is_conflict()
        || entry.is_unfinished()
        || is_conflict_entry(entry)
        || !matches_recorded_time(local, &entry.date)
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

/// The regular files in a folder that are not in `Entries`, sorted the way cvs prints them.
fn unknown_files(local: &Path, entries: &Entries) -> Vec<String> {
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

/// `d/two.txt` → (`d`, `two.txt`); `two.txt` → ("", `two.txt`).
fn split_local(path: &str) -> (String, String) {
    let path = path.replace('\\', "/");
    let path = path.trim_start_matches("./");
    match path.rsplit_once('/') {
        Some((dir_name, name)) => (dir_name.trim_matches('/').to_string(), name.to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// `./` and `.` are the invocation folder; the rest keeps its slashes.
fn normalize_update_dir(update_dir: &str) -> String {
    let key = update_dir.trim().trim_matches('/');
    if key == "." {
        String::new()
    } else {
        key.to_string()
    }
}

/// The local path of an update directory below the invocation folder.
fn local_dir(cwd: &Path, update_dir: &str) -> PathBuf {
    let mut path = cwd.to_path_buf();
    for part in update_dir.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        path.push(part);
    }
    path
}

/// A `New-entry` carries no timestamp. The reference registers it as
/// `dummy timestamp from new-entry`, which keeps a locally modified file modified.
fn new_entry_line(entry_line: &str) -> String {
    match Entry::parse(entry_line) {
        Some(mut entry) => {
            if entry.date.is_empty() {
                entry.date = "dummy timestamp from new-entry".to_string();
            }
            entry.line()
        }
        None => entry_line.to_string(),
    }
}

/// Finish the working copy after the response stream: record folders the server created
/// (`-d`), write the official clean-merge date, and prune empty folders (`-P`).
fn finish_working_copy(
    cwd: &Path,
    targets: &BTreeMap<String, Target>,
    seen_dirs: &BTreeSet<String>,
    merges: &BTreeMap<(String, String), bool>,
    opts: &Options,
    no_write: bool,
) -> Result<()> {
    if no_write {
        return Ok(());
    }
    register_directories(cwd, seen_dirs)?;
    write_merge_entries(cwd, merges)?;
    if opts.prune {
        prune_empty_dirs(cwd, targets)?;
    }
    Ok(())
}

/// `Subdir_Register`: a folder the server named and created must appear in its parent's
/// `CVS/Entries` as `D/name////`, otherwise the next recursive command never enters it.
fn register_directories(cwd: &Path, seen_dirs: &BTreeSet<String>) -> Result<()> {
    let mut changes: BTreeMap<PathBuf, (Entries, bool)> = BTreeMap::new();
    for update_dir in seen_dirs {
        let parts: Vec<&str> = update_dir
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        for depth in 1..=parts.len() {
            let parent = cwd.join(parts[..depth - 1].iter().copied().collect::<PathBuf>());
            let child = parent.join(parts[depth - 1]);
            if !child.join("CVS").is_dir() || !parent.join("CVS").is_dir() {
                continue;
            }
            if !changes.contains_key(&parent) {
                changes.insert(parent.clone(), (Entries::read(&parent)?, false));
            }
            let (entries, dirty) = changes.get_mut(&parent).unwrap();
            if !entries.directories().any(|name| name == parts[depth - 1]) {
                entries.set_directory(parts[depth - 1]);
                *dirty = true;
            }
        }
    }
    for (folder, (entries, dirty)) in changes {
        if dirty {
            entries.write(&folder, codec::for_dir(&folder))?;
        }
    }
    Ok(())
}

/// A clean merge's `Entries` date is the literal `Result of merge`; a conflict has the file's
/// time appended. `write_file` cannot tell the two apart for a clean merge (the server sends
/// no marker), so the date is written here once the response stream is over.
fn write_merge_entries(cwd: &Path, merges: &BTreeMap<(String, String), bool>) -> Result<()> {
    let mut changes: BTreeMap<PathBuf, Entries> = BTreeMap::new();
    for ((update_dir, name), conflict) in merges {
        if *conflict {
            continue;
        }
        let folder = local_dir(cwd, update_dir);
        if !folder.join("CVS").is_dir() {
            continue;
        }
        let mut entries = match changes.remove(&folder) {
            Some(entries) => entries,
            None => Entries::read(&folder)?,
        };
        if let Some(mut entry) = entries.file(name).cloned() {
            if entry.date != "Result of merge" {
                entry.date = "Result of merge".to_string();
                entries.set_file(entry);
                changes.insert(folder, entries);
            } else {
                changes.insert(folder, entries);
            }
        }
    }
    for (folder, entries) in changes {
        entries.write(&folder, codec::for_dir(&folder))?;
    }
    Ok(())
}

/// `-P`: every folder that was named to the server is a prune candidate. Deepest first, so a
/// folder that only held another folder can become empty in turn. A folder is empty when it
/// holds nothing but `CVS/` and has no removed-but-uncommitted file.
fn prune_empty_dirs(cwd: &Path, targets: &BTreeMap<String, Target>) -> Result<()> {
    let mut candidates: Vec<&String> = targets.keys().filter(|key| !key.is_empty()).collect();
    candidates.sort_by_key(|key| std::cmp::Reverse(key.split('/').count()));
    for key in candidates {
        let folder = local_dir(cwd, key);
        if !folder.is_dir() || !is_empty_dir(&folder)? {
            continue;
        }
        std::fs::remove_dir_all(&folder)?;
        let Some(parent) = folder.parent() else {
            continue;
        };
        let Some(name) = folder.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !parent.join("CVS").is_dir() {
            continue;
        }
        let mut entries = Entries::read(parent)?;
        entries.remove_directory(name);
        entries.write(parent, codec::for_dir(parent))?;
    }
    Ok(())
}

fn is_empty_dir(folder: &Path) -> Result<bool> {
    let entries = Entries::read(folder)?;
    if entries.files().any(|entry| entry.is_removed()) {
        return Ok(false);
    }
    for item in std::fs::read_dir(folder)? {
        let item = item?;
        if item.file_name() != "CVS" {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_path_splits_into_folder_and_name() {
        assert_eq!(split_local("d/two.txt"), ("d".into(), "two.txt".into()));
        assert_eq!(split_local("two.txt"), ("".into(), "two.txt".into()));
        assert_eq!(split_local("./d/two.txt"), ("d".into(), "two.txt".into()));
    }

    #[test]
    fn the_command_accepts_the_desktop_programs_n() {
        let opts = parse_args(&["-n".to_string(), "-P".to_string()]).unwrap();
        assert!(opts.no_execute);
        assert!(opts.prune);
    }

    #[test]
    fn clustered_short_options_split_into_their_letters() {
        let opts = parse_args(&["-Pd".to_string()]).unwrap();
        assert!(opts.prune);
        assert!(opts.build_dirs);
        let opts = parse_args(&["-APdf".to_string()]).unwrap();
        assert!(opts.reset_sticky);
        assert!(opts.prune);
        assert!(opts.build_dirs);
        assert!(opts.force);
        assert!(opts.recursive, "-R is the default and stays on");
    }

    #[test]
    fn a_cluster_stops_at_the_value_of_an_option() {
        assert_eq!(
            expand_short_options(&["-Pr1.2".to_string()]).unwrap(),
            vec!["-P".to_string(), "-r".to_string(), "1.2".to_string()]
        );
        assert_eq!(
            expand_short_options(&["-dPkj".to_string()]).unwrap(),
            vec![
                "-d".to_string(),
                "-P".to_string(),
                "-k".to_string(),
                "j".to_string()
            ]
        );
        assert_eq!(
            expand_short_options(&["-I".to_string(), "*.o".to_string()]).unwrap(),
            vec!["-I".to_string(), "*.o".to_string()]
        );
    }

    #[test]
    fn excluded_spellings_are_kept_out_of_the_cluster_expansion() {
        let opts = parse_args(&[
            "-I".to_string(),
            "*.tmp".to_string(),
            "--".to_string(),
            "-weird".to_string(),
        ])
        .unwrap();
        assert_eq!(opts.ignores, vec!["*.tmp"]);
        assert_eq!(opts.names, vec!["-weird"]);
    }

    #[test]
    fn a_q_on_the_command_line_is_refused_like_the_reference() {
        let error = parse_args(&["-q".to_string()]).unwrap_err();
        assert!(error.text().contains("-q or -Q must be specified before"));
    }

    #[test]
    fn a_clean_merge_is_not_sent_as_a_conflict() {
        let clean = Entry::new("a.txt", "1.2", "Result of merge", "", "");
        assert!(!is_conflict_entry(&clean));
        assert_eq!(wire_entry(&clean), "/a.txt/1.2///");
        let conflict = Entry::new(
            "a.txt",
            "1.2",
            "Result of merge+Tue Nov 24 09:52:53 2026",
            "",
            "",
        );
        assert!(is_conflict_entry(&conflict));
        assert_eq!(wire_entry(&conflict), "/a.txt/1.2/+=//");
    }

    #[test]
    fn a_wrapper_line_is_reduced_to_its_k_option() {
        let wrapper = parse_wrapper("*.nib -m 'COPY' -k 'b'").unwrap();
        assert_eq!(wrapper.pattern, "*.nib");
        assert_eq!(wrapper.kopt.as_deref(), Some("b"));
        assert!(wrapper.copy);
        assert_eq!(wrapper.protocol_line(), "*.nib -k 'b'");

        let wrapper = parse_wrapper("*.txt").unwrap();
        assert_eq!(wrapper.protocol_line(), "*.txt -k 'kv'");
        assert!(parse_wrapper("# a comment").is_none());
    }

    #[test]
    fn a_new_entry_becomes_a_dummy_timestamp() {
        assert_eq!(
            new_entry_line("/a.txt/1.2///"),
            "/a.txt/1.2/dummy timestamp from new-entry//"
        );
        assert_eq!(
            new_entry_line("/a.txt/1.2/Tue Nov 24 09:52:53 2026//"),
            "/a.txt/1.2/Tue Nov 24 09:52:53 2026//"
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-update-{}-{}-{}",
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

    fn versioned(dir: &Path) {
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        std::fs::write(dir.join("CVS").join("Repository"), "mod\n").unwrap();
        std::fs::write(dir.join("CVS").join("Entries"), "").unwrap();
    }

    #[test]
    fn a_folder_the_server_created_is_registered_in_its_parent() {
        let base = scratch("register");
        versioned(&base);
        versioned(&base.join("newdir"));
        let mut seen = BTreeSet::new();
        seen.insert("newdir".to_string());
        register_directories(&base, &seen).unwrap();
        let entries = Entries::read(&base).unwrap();
        assert_eq!(entries.directories().collect::<Vec<_>>(), vec!["newdir"]);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn an_empty_folder_is_pruned_and_its_parent_line_goes_with_it() {
        let base = scratch("prune");
        versioned(&base);
        versioned(&base.join("gone"));
        let mut entries = Entries::read(&base).unwrap();
        entries.set_directory("gone");
        entries.write(&base, codec::for_dir(&base)).unwrap();
        let target = Target {
            dir: dir::WorkingDir::new(
                base.join("gone"),
                "gone".to_string(),
                "mod/gone".to_string(),
                codec::for_dir(&base),
            ),
            entries: Entries::default(),
            filter: Vec::new(),
            selected: true,
            loose: Vec::new(),
        };
        let mut targets = BTreeMap::new();
        targets.insert("gone".to_string(), target);
        prune_empty_dirs(&base, &targets).unwrap();
        assert!(!base.join("gone").exists());
        let entries = Entries::read(&base).unwrap();
        assert_eq!(entries.directories().count(), 0);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_folder_with_a_removed_file_is_not_pruned() {
        let base = scratch("prune-removed");
        versioned(&base);
        versioned(&base.join("kept"));
        std::fs::write(
            base.join("kept").join("CVS").join("Entries"),
            "/a.txt/-1/dummy timestamp//\n",
        )
        .unwrap();
        let target = Target {
            dir: dir::WorkingDir::new(
                base.join("kept"),
                "kept".to_string(),
                "mod/kept".to_string(),
                codec::for_dir(&base),
            ),
            entries: Entries::default(),
            filter: Vec::new(),
            selected: true,
            loose: Vec::new(),
        };
        let mut targets = BTreeMap::new();
        targets.insert("kept".to_string(), target);
        prune_empty_dirs(&base, &targets).unwrap();
        assert!(
            base.join("kept").is_dir(),
            "a pending removal keeps the folder"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
