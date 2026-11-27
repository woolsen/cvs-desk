//! `cvs commit`: send the working files' changes to the repository.
//!
//! Only the files that changed — modified, added or removed — are sent, as the reference
//! client does: an `Entry` line plus `Modified` (with the contents) for a changed file, and
//! an `Entry` line alone for a removed or lost one. That way the server's up-to-date check
//! covers exactly the files this commit touches. Unknown files in a fully named folder are
//! sent as `Questionable` (or printed as `? name` by a server that cannot take them).
//!
//! The log message comes from `-m`, `-F`, or an editor (`-e`, `$CVSEDITOR`, `$VISUAL`,
//! `$EDITOR`, Notepad/vi). The editor runs only after the server accepted the connection and
//! only when there is something to commit. `-m` and `-F` cannot be combined; an empty message
//! is sent as `*** empty log message ***`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::client::commands::targets;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, FileKind, Response};
use crate::client::root::Root;
use crate::client::work::base;
use crate::client::work::codec;
use crate::client::work::dir;
use crate::client::work::entries::{Entry, EntryLine};
use crate::client::work::WorkWriter;

#[derive(Debug, Default)]
struct Options {
    recursive: bool,
    message: Option<String>,
    /// `-m` was given (even with an empty value), so `-F` must be refused.
    message_option: bool,
    /// The file `-F` named; read after the options, as the reference client does.
    logfile: Option<String>,
    force: bool,
    revision: Option<String>,
    merge: Option<String>,
    /// The obsolete `-n`, accepted with a warning like 1.11.23.
    obsolete_n: bool,
    names: Vec<String>,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let args = expand_clusters(args)?;
    let mut opts = Options {
        recursive: true,
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--" => {
                opts.names.extend(args[i + 1..].iter().cloned());
                break;
            }
            "-l" => opts.recursive = false,
            "-R" => opts.recursive = true,
            // The command-level `-q`/`-Q` are not in 1.11.23's commit options; they are
            // accepted for compatibility. The global forms are sent by the session itself.
            "-q" | "-Q" => {}
            "-f" => {
                opts.force = true;
                // `-f` also implies `-l`; a later `-R` turns recursion back on.
                opts.recursive = false;
            }
            "-n" => opts.obsolete_n = true,
            "-m" => {
                i += 1;
                opts.message = Some(require(&args, i, "-m")?);
                opts.message_option = true;
            }
            "-F" => {
                i += 1;
                opts.logfile = Some(require(&args, i, "-F")?);
            }
            "-r" => {
                i += 1;
                opts.revision = Some(normalize_revision(&require(&args, i, "-r")?));
            }
            "-j" => {
                i += 1;
                opts.merge = Some(require(&args, i, "-j")?);
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(Error::plain(
                    "commit",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.names.push(other.to_string()),
        }
        i += 1;
    }
    if opts.message_option && opts.logfile.is_some() {
        return Err(Error::plain(
            "commit",
            "cannot specify both a message and a log file",
        ));
    }
    if !opts.message_option {
        if let Some(path) = &opts.logfile {
            let bytes = std::fs::read(path).map_err(|e| {
                Error::aborted(
                    "commit",
                    format!("cannot read the message file {}: {}", path, e),
                )
            })?;
            // A log file written by an editor is in the user's code page, not necessarily
            // UTF-8. Decode the same way a control file is decoded.
            opts.message = Some(codec::detect(&bytes).decode(&bytes));
        }
    }
    Ok(opts)
}

/// Split getopt-style clusters: `-lf` is `-l -f`, `-mfix` is `-m fix`, `-r1.2.` is
/// `-r 1.2.`. An option that takes a value swallows the rest of its cluster.
fn expand_clusters(args: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(args.len());
    let mut literal = false;
    for arg in args {
        if literal {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            out.push(arg.clone());
            literal = true;
            continue;
        }
        if arg == "-" || !arg.starts_with('-') || arg.starts_with("--") {
            out.push(arg.clone());
            continue;
        }
        let mut chars = arg[1..].chars();
        while let Some(ch) = chars.next() {
            match ch {
                'l' | 'R' | 'q' | 'Q' | 'f' | 'n' => out.push(format!("-{}", ch)),
                'm' | 'F' | 'r' | 'j' => {
                    let rest: String = chars.collect();
                    out.push(format!("-{}", ch));
                    if !rest.is_empty() {
                        out.push(rest);
                    }
                    break;
                }
                other => {
                    return Err(Error::plain(
                        "commit",
                        format!("invalid option -- '{}'", other),
                    ))
                }
            }
        }
    }
    Ok(out)
}

fn require(args: &[String], i: usize, option: &str) -> Result<String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| Error::aborted("commit", format!("option {} requires an argument", option)))
}

/// A purely numeric revision loses trailing dots and leading zeros, as 1.11.23 does before
/// sending it (`1.2.` and `01.2` both mean `1.2`).
fn normalize_revision(revision: &str) -> String {
    if revision.is_empty() || !revision.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        return revision.to_string();
    }
    let mut text = revision.trim_end_matches('.').to_string();
    if text.is_empty() {
        return revision.to_string();
    }
    while text.len() > 1 && text.starts_with('0') && text.as_bytes()[1].is_ascii_digit() {
        text.remove(0);
    }
    text
}

/// One folder the commit will report as holding unknown files.
#[derive(Debug)]
struct UnknownDir {
    update_dir: String,
    repository: String,
    local: PathBuf,
    names: Vec<String>,
}

/// What the commit will send, worked out before the connection (and before the editor).
#[derive(Debug, Default)]
struct ChangePlan {
    /// Every changed file, relative to the invocation folder: the `Argument` list before `ci`.
    names: Vec<String>,
    modified: Vec<String>,
    added: Vec<String>,
    removed: Vec<String>,
    /// Complaints the reference prints before sending anything.
    warnings: Vec<String>,
    unknown: Vec<UnknownDir>,
}

/// Keep only the changed files in each folder. Unchanged entries stay out of the request so
/// the server checks up-to-date state for the files being committed, nothing else.
fn plan_changes(
    targets: &mut BTreeMap<String, targets::Target>,
    force: bool,
) -> Result<ChangePlan> {
    let mut plan = ChangePlan::default();
    let keys: Vec<String> = targets.keys().cloned().collect();
    for key in keys {
        let target = targets.get_mut(&key).expect("the key came from this map");
        if target.selected {
            let unknown = targets::unknown_files(&target.dir.local, &target.entries);
            if !unknown.is_empty() {
                plan.unknown.push(UnknownDir {
                    update_dir: target.dir.update_dir.clone(),
                    repository: target.dir.repository.clone(),
                    local: target.dir.local.clone(),
                    names: unknown,
                });
            }
        }
        let mut kept = Vec::new();
        // A forced file that is otherwise unchanged must be sent as `Modified`; clearing its
        // recorded time makes the block writer see a change without touching the disk.
        let mut clear_dates = Vec::new();
        for entry in target.entries.files() {
            if !target.selected && !target.filter.iter().any(|name| name == &entry.name) {
                continue;
            }
            let full = dir::display_path(&target.dir.update_dir, &entry.name);
            let local = target.dir.local.join(&entry.name);
            if entry.is_removed() {
                if local.is_file() {
                    return Err(Error::aborted(
                        "commit",
                        format!(
                            "`{}' should be removed and is still there (or is back again)",
                            full
                        ),
                    ));
                }
                plan.removed.push(full.clone());
                plan.names.push(full);
                kept.push(entry.name.clone());
                continue;
            }
            if entry.is_added() {
                if local.is_file() {
                    plan.added.push(full.clone());
                } else {
                    // `cvs add`ed but gone again: the reference warns and lets the server
                    // drop the entry.
                    plan.warnings
                        .push(format!("warning: new-born {} has disappeared", full));
                }
                plan.names.push(full);
                kept.push(entry.name.clone());
                continue;
            }
            if !local.is_file() {
                // A lost file is not committed; the reference skips it silently.
                continue;
            }
            let recorded_match = matches_recorded_time(&local, &entry.date);
            let already_changed = entry.is_conflict() || entry.is_unfinished() || !recorded_match;
            if !already_changed && !force {
                continue;
            }
            if force && !already_changed {
                clear_dates.push(entry.name.clone());
            }
            plan.modified.push(full.clone());
            plan.names.push(full);
            kept.push(entry.name.clone());
        }
        if kept.is_empty() {
            targets.remove(&key);
            continue;
        }
        for name in clear_dates {
            if let Some(EntryLine::File(entry)) = target
                .entries
                .lines
                .iter_mut()
                .find(|line| matches!(line, EntryLine::File(entry) if entry.name == name))
            {
                entry.date.clear();
            }
        }
        target.selected = false;
        target.filter = kept;
        target.loose.clear();
    }
    Ok(plan)
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

/// Run `cvs commit`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    readonly: bool,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = parse_args(args)?;
    if opts.obsolete_n {
        sink.err("cvs commit: the `-n' option is obsolete".to_string());
    }
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "commit",
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    let no_execute = root.options.no_execute;
    let editor = root.options.editor.clone();

    let mut targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    // `-r` makes the reference pretend every file with a recorded time is modified too.
    let force = opts.force || opts.revision.is_some();
    let plan = plan_changes(&mut targets, force)?;
    for warning in &plan.warnings {
        sink.err(warning.clone());
    }
    if plan.names.is_empty() {
        // Nothing changed: exit successfully without contacting the server, as CVS does.
        return Ok(0);
    }

    let mut session = protocol::open(root, "commit", cancel, sink)?;

    // The editor runs only after the connection succeeded and only when files are going out;
    // `-m`/`-F` already supplied a message, and `-n` never edits.
    let (outgoing, editor_warning) = resolve_message(
        cwd,
        &plan,
        opts.message.clone(),
        editor.as_deref(),
        no_execute,
        &mut session,
    )?;
    if let Some(warning) = editor_warning {
        session.err(warning);
    }
    let saved_message = opts.message.is_none() && outgoing.is_some() && !no_execute;
    let message = effective_message(outgoing.as_deref());

    session.argument("-m")?;
    session.argument(&message)?;
    send_unknown_files(&mut session, &plan.unknown)?;
    if !opts.recursive {
        session.argument("-l")?;
    }
    if opts.force {
        session.argument("-f")?;
    }
    if let Some(revision) = &opts.revision {
        session.argument("-r")?;
        session.argument(revision)?;
    }
    if let Some(merge) = &opts.merge {
        session.argument("-j")?;
        session.argument(merge)?;
    }

    targets::begin_files(&mut session, &opts.names)?;
    targets::send_blocks(&mut session, &targets, false)?;
    targets::finish_to_top(&mut session, cwd, &plan.names, "ci")?;

    let mut writer = WorkWriter::new(cwd, session.root.clone())
        .readonly(readonly)
        .dry_run(no_execute);
    let mut pending_mode: Option<String> = None;
    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::Mode(mode) => pending_mode = Some(mode),
            Response::CheckedIn {
                update_dir,
                repository,
                entry,
            } => {
                let parsed = Entry::parse(&entry);
                if let Some(parsed) = &parsed {
                    if !no_execute {
                        base::forget(&writer.local_for(&update_dir), &parsed.name)?;
                    }
                }
                writer.record_entry(&update_dir, &repository, &entry)?;
                if let (Some(parsed), Some(mode)) = (&parsed, pending_mode.take()) {
                    apply_mode(
                        &writer.local_for(&update_dir).join(&parsed.name),
                        &mode,
                        no_execute,
                    )?;
                } else {
                    pending_mode = None;
                }
            }
            Response::NewEntry {
                update_dir,
                repository,
                entry,
            } => {
                // Older servers change an entry without sending file contents. The entry is
                // registered with the dummy timestamp `New-entry` asks for.
                let mut parsed = Entry::parse(&entry);
                if let Some(parsed) = &mut parsed {
                    if parsed.date.is_empty() {
                        parsed.date = "dummy timestamp from new-entry".to_string();
                    }
                }
                let line = parsed.as_ref().map(Entry::line).unwrap_or(entry);
                writer.record_entry(&update_dir, &repository, &line)?;
                if let (Some(parsed), Some(mode)) = (&parsed, pending_mode.take()) {
                    apply_mode(
                        &writer.local_for(&update_dir).join(&parsed.name),
                        &mode,
                        no_execute,
                    )?;
                } else {
                    pending_mode = None;
                }
            }
            Response::File {
                kind,
                update_dir,
                repository,
                entry,
                mode,
                content,
            } => {
                let parsed = Entry::parse(&entry);
                if kind == FileKind::Patched {
                    writer.write_patch(&update_dir, &repository, &entry, &mode, &content, None)?;
                } else {
                    writer.write_file(
                        kind,
                        &update_dir,
                        &repository,
                        &entry,
                        &mode,
                        &content,
                        None,
                    )?;
                }
                if let Some(parsed) = &parsed {
                    if !no_execute {
                        base::forget(&writer.local_for(&update_dir), &parsed.name)?;
                    }
                    if let Some(over) = pending_mode.take() {
                        apply_mode(
                            &writer.local_for(&update_dir).join(&parsed.name),
                            &over,
                            no_execute,
                        )?;
                    }
                } else {
                    pending_mode = None;
                }
            }
            Response::CopyFile {
                update_dir,
                repository,
                name,
            } => writer.copy_file(&update_dir, &repository, &name)?,
            Response::Removed(path) => {
                let (folder, name) = split_local(&path);
                if !no_execute {
                    base::forget(&writer.local_for(&folder), &name)?;
                }
                writer.remove_file(&folder, &name)?;
                writer.remove_entry(&folder, &name)?;
            }
            Response::RemoveEntry(path) => {
                let (folder, name) = split_local(&path);
                if !no_execute {
                    base::forget(&writer.local_for(&folder), &name)?;
                }
                writer.remove_entry(&folder, &name)?;
            }
            Response::Template { dir, content } => writer.write_template(&dir, &content)?,
            Response::Error { message, .. } => {
                writer.finish()?;
                // Keep the prose the user wrote: the reference saves it when the server
                // rejects the commit.
                let saved = if saved_message && !no_execute {
                    save_log_message(&message).ok()
                } else {
                    None
                };
                session.close();
                if let Some(path) = saved {
                    sink.err(format!(
                        "cvs commit: saving log message in {}",
                        path.display()
                    ));
                }
                if message.trim().is_empty() {
                    return Err(Error::aborted("commit", "the server reported an error"));
                }
                return Err(Error::aborted("commit", message));
            }
            Response::F(_)
            | Response::ValidRequests(_)
            | Response::ModuleExpansion(_)
            | Response::Trace(_)
            | Response::Checksum(_)
            | Response::ModTime(_)
            | Response::SetStaticDirectory { .. }
            | Response::ClearStaticDirectory { .. }
            | Response::SetSticky { .. }
            | Response::ClearSticky { .. }
            | Response::Notified => {}
        }
    }

    writer.finish()?;
    session.close();
    Ok(0)
}

/// The message the server gets: what the user wrote, or the marker RCS uses for an empty one.
fn effective_message(message: Option<&str>) -> String {
    match message {
        Some(text) if !text.trim().is_empty() => text.to_string(),
        _ => "*** empty log message ***".to_string(),
    }
}

/// Pick the log message: `-m`/`-F` when present, otherwise an editor session. With `-n` the
/// reference does not start an editor at all.
fn resolve_message(
    cwd: &Path,
    plan: &ChangePlan,
    message: Option<String>,
    editor: Option<&str>,
    no_execute: bool,
    session: &mut protocol::Session,
) -> Result<(Option<String>, Option<String>)> {
    if message.is_some() {
        return Ok((message, None));
    }
    if no_execute {
        return Ok((None, None));
    }
    edit_message(cwd, plan, editor, session)
}

/// Tell the server about files it does not know. A server without the `Questionable`
/// request gets the local `? name` lines instead, as the reference client does.
fn send_unknown_files(session: &mut protocol::Session, unknown: &[UnknownDir]) -> Result<()> {
    for folder in unknown {
        if !session.supports("Questionable") {
            for name in &folder.names {
                session.out(format!("? {}", dir::display_path(&folder.update_dir, name)));
            }
            continue;
        }
        let absolute = dir::absolute_repository(session.root.protocol_root(), &folder.repository);
        session.directory(
            &targets::display_dir(&folder.update_dir),
            &absolute,
            &folder.local,
        )?;
        for name in &folder.names {
            session.send(&format!("Questionable {}", name))?;
        }
    }
    Ok(())
}

/// `Mode` changes the local permission of the file the following response carries.
fn apply_mode(path: &Path, mode: &str, no_execute: bool) -> Result<()> {
    if no_execute || mode.trim().is_empty() || !path.is_file() {
        return Ok(());
    }
    dir::set_readonly(path, dir::mode_is_readonly(mode))
}

/// The text the editor starts with: the folder's `CVS/Template` (when the repository set
/// one), the instruction lines, `Committing in .` and the files this commit covers.
fn initial_text(template: Option<&str>, plan: &ChangePlan) -> String {
    let mut text = String::new();
    if let Some(template) = template {
        if !template.is_empty() {
            text.push_str(template);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
    }
    text.push_str("CVS: ----------------------------------------------------------------------\n");
    text.push_str("CVS: Enter Log.  Lines beginning with `CVS:' are removed automatically\n");
    text.push_str("CVS: \n");
    text.push_str("CVS: Committing in .\n");
    text.push_str("CVS: \n");
    text.push_str(&file_group("Modified Files:", &plan.modified));
    text.push_str(&file_group("Added Files:", &plan.added));
    text.push_str(&file_group("Removed Files:", &plan.removed));
    text.push_str("CVS: ----------------------------------------------------------------------\n");
    text
}

/// The `Modified Files:`/`Added Files:`/`Removed Files:` block, wrapped the way cvs wraps it.
fn file_group(title: &str, files: &[String]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut text = format!("CVS: {}\n", title);
    let mut col = 0usize;
    for file in files {
        if col == 0 {
            text.push_str("CVS: \t");
            col = 8;
        } else if col + file.chars().count() > 70 {
            text.push_str("\nCVS: \t");
            col = 8;
        }
        text.push_str(file);
        text.push(' ');
        col += file.chars().count() + 1;
    }
    text.push('\n');
    text
}

/// What the user typed, with the `CVS: ` instruction lines taken out. The last line may
/// have no newline; an editor on Windows may have written CRLF.
fn message_from_edited(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut message = String::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with("CVS: ") {
            continue;
        }
        message.push_str(line);
    }
    message
}

/// The editor the reference picks: `-e`, then `CVSEDITOR`, `VISUAL`, `EDITOR`, then the
/// platform's plain editor.
fn editor_command(option: Option<&str>) -> String {
    if let Some(editor) = option {
        if !editor.trim().is_empty() {
            return editor.to_string();
        }
    }
    for name in ["CVSEDITOR", "VISUAL", "EDITOR"] {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return value;
            }
        }
    }
    if cfg!(windows) {
        "notepad".to_string()
    } else {
        "vi".to_string()
    }
}

/// Run the editor on a temporary message file and return what the user wrote.
///
/// If the file did not change (or the message is empty), ask the same a/c/e/! question the
/// reference asks; an answer that cannot be read counts as `c)ontinue`.
fn edit_message(
    cwd: &Path,
    plan: &ChangePlan,
    editor_option: Option<&str>,
    session: &mut protocol::Session,
) -> Result<(Option<String>, Option<String>)> {
    let template = std::fs::read(cwd.join("CVS").join("Template"))
        .ok()
        .map(|bytes| codec::detect(&bytes).decode(&bytes));
    let initial = initial_text(template.as_deref(), plan);
    let path = message_temp_path();
    loop {
        write_initial_message(&path, &initial)?;
        let before = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        let editor = editor_command(editor_option);
        let mut parts = editor.split_whitespace();
        let program = parts.next().unwrap_or("");
        if program.is_empty() {
            let _ = std::fs::remove_file(&path);
            return Err(Error::aborted(
                "commit",
                "no editor defined, must use -e or -m",
            ));
        }
        let mut warning = None;
        match std::process::Command::new(program)
            .args(parts)
            .arg(&path)
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(_) => warning = Some("cvs commit: warning: editor session failed".to_string()),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(Error::aborted(
                    "commit",
                    format!("cannot run editor {}: {}", program, e),
                ));
            }
        }

        let bytes = std::fs::read(&path)?;
        let after = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let text = message_from_edited(&codec::detect(&bytes).decode(&bytes));
        let message = if text.trim().is_empty() {
            None
        } else {
            Some(text)
        };
        if before != after && message.is_some() {
            let _ = std::fs::remove_file(&path);
            return Ok((message, warning));
        }
        match ask_action(session) {
            EditAction::Abort => {
                let _ = std::fs::remove_file(&path);
                return Err(Error::aborted("commit", "aborted by user"));
            }
            EditAction::Edit => continue,
            EditAction::Continue | EditAction::Reuse => {
                let _ = std::fs::remove_file(&path);
                return Ok((message, warning));
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum EditAction {
    Abort,
    Continue,
    Edit,
    Reuse,
}

/// The question the reference asks when the editor left the message unchanged. It goes
/// through the sink: the console reads the answer, and a window answers with its default
/// at once instead of waiting on a standard input nobody can type into.
fn ask_action(session: &mut protocol::Session) -> EditAction {
    let answer = session.ask(
        "\nLog message unchanged or not specified\n\
         a)bort, c)ontinue, e)dit, !)reuse this message unchanged for remaining dirs\n\
         Action: (continue) "
            .to_string(),
    );
    match answer {
        // End of input counts as the default, as the reference's `getline` failure does.
        None => EditAction::Continue,
        Some(answer) => action_for(&answer),
    }
}

fn action_for(answer: &str) -> EditAction {
    match answer.trim_start().chars().next() {
        Some('a' | 'A') => EditAction::Abort,
        Some('e' | 'E') => EditAction::Edit,
        Some('!') => EditAction::Reuse,
        _ => EditAction::Continue,
    }
}

fn write_initial_message(path: &Path, initial: &str) -> Result<()> {
    let bytes = codec::system_ansi()
        .encode(initial)
        .unwrap_or_else(|_| initial.as_bytes().to_vec());
    std::fs::write(path, codec::to_platform_newlines(&bytes))?;
    Ok(())
}

fn message_temp_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    crate::core::temp::messages_dir().join(format!("cvsmsg-{}-{}.txt", std::process::id(), nanos))
}

fn save_log_message(message: &str) -> std::io::Result<PathBuf> {
    let path = message_temp_path();
    let bytes = codec::system_ansi()
        .encode(message)
        .unwrap_or_else(|_| message.as_bytes().to_vec());
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// `d/two.txt` → (`d`, `two.txt`); `two.txt` → ("", `two.txt`).
fn split_local(path: &str) -> (String, String) {
    let path = path.replace('\\', "/");
    let path = path.trim_start_matches("./");
    match path.rsplit_once('/') {
        Some((dir, name)) => (dir.trim_matches('/').to_string(), name.to_string()),
        None => (String::new(), path.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::work::entries::Entries;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-commit-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        dir
    }

    fn one_target(dir: &Path, entries: &str) -> BTreeMap<String, targets::Target> {
        let working = dir::WorkingDir::new(
            dir.to_path_buf(),
            String::new(),
            "mod".to_string(),
            codec::system_ansi(),
        );
        let mut map = BTreeMap::new();
        map.insert(
            String::new(),
            targets::Target {
                dir: working,
                entries: Entries::parse(entries),
                filter: Vec::new(),
                selected: true,
                loose: Vec::new(),
            },
        );
        map
    }

    #[test]
    fn the_message_file_is_read_and_the_name_is_kept() {
        let dir = scratch("message-file");
        let file = dir.join("msg.txt");
        std::fs::write(&file, "from a file\n").unwrap();
        let opts = parse_args(&args(&["-F", &file.display().to_string(), "a.txt"])).unwrap();
        assert_eq!(opts.message.as_deref(), Some("from a file\n"));
        assert_eq!(opts.names, vec!["a.txt"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn short_options_cluster_the_way_getopt_reads_them() {
        let opts = parse_args(&args(&["-lf"])).unwrap();
        assert!(!opts.recursive, "-f implies -l");
        assert!(opts.force);

        let opts = parse_args(&args(&["-fR"])).unwrap();
        assert!(opts.recursive, "a later -R undoes the -l of -f");
        assert!(opts.force);

        let opts = parse_args(&args(&["-Rf"])).unwrap();
        assert!(!opts.recursive, "-f after -R turns recursion off again");
        assert!(opts.force);

        let opts = parse_args(&args(&["-mfix", "a.txt"])).unwrap();
        assert_eq!(opts.message.as_deref(), Some("fix"));
        assert_eq!(opts.names, vec!["a.txt"]);

        let opts = parse_args(&args(&["-r1.2.", "-j1.1", "a.txt"])).unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.2"));
        assert_eq!(opts.merge.as_deref(), Some("1.1"));
    }

    #[test]
    fn a_message_and_a_log_file_are_mutually_exclusive() {
        let dir = scratch("both");
        let file = dir.join("msg.txt");
        std::fs::write(&file, "from file\n").unwrap();
        let error = parse_args(&args(&["-m", "", "-F", &file.display().to_string()])).unwrap_err();
        assert!(
            error
                .text()
                .contains("cannot specify both a message and a log file"),
            "{}",
            error
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_message_becomes_the_rcs_marker() {
        assert_eq!(effective_message(Some("")), "*** empty log message ***");
        assert_eq!(
            effective_message(Some("   \n")),
            "*** empty log message ***"
        );
        assert_eq!(effective_message(None), "*** empty log message ***");
        assert_eq!(effective_message(Some("fix")), "fix");
    }

    #[test]
    fn a_gbk_log_file_is_decoded_not_mangled() {
        let dir = scratch("gbk-message");
        let file = dir.join("msg.txt");
        let (bytes, _, _) = encoding_rs::GBK.encode("修改了中文日志");
        std::fs::write(&file, bytes.as_ref()).unwrap();
        let opts = parse_args(&args(&["-F", &file.display().to_string()])).unwrap();
        assert_eq!(opts.message.as_deref(), Some("修改了中文日志"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_changed_files_are_sent() {
        let dir = scratch("only-changed");
        let file = dir.join("a.txt");
        std::fs::write(&file, "hello\n").unwrap();
        let recorded = codec::asctime(codec::file_mtime(&file).unwrap());
        let entries = format!("/a.txt/1.1/{}//\n", recorded);

        let mut map = one_target(&dir, &entries);
        let plan = plan_changes(&mut map, false).unwrap();
        assert!(plan.names.is_empty(), "{:?}", plan.names);
        assert!(map.is_empty(), "an unchanged folder is not named");

        let mut map = one_target(&dir, &entries);
        let plan = plan_changes(&mut map, true).unwrap();
        assert_eq!(plan.names, vec!["a.txt"], "-f sends an unchanged file");
        assert_eq!(map[""].filter, vec!["a.txt"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_removed_file_that_is_back_is_refused() {
        let dir = scratch("removed-back");
        std::fs::write(dir.join("a.txt"), "back\n").unwrap();
        let mut map = one_target(&dir, "/a.txt/-1/Tue Nov 24 09:52:53 2026//\n");
        let error = plan_changes(&mut map, false).unwrap_err();
        assert!(
            error
                .text()
                .contains("should be removed and is still there"),
            "{}",
            error
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_removed_path_splits_into_folder_and_name() {
        assert_eq!(split_local("d/a.txt"), ("d".into(), "a.txt".into()));
        assert_eq!(split_local("a.txt"), ("".into(), "a.txt".into()));
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;

    #[test]
    fn the_unchanged_message_answers_are_read_like_the_reference() {
        assert_eq!(action_for("a"), EditAction::Abort);
        assert_eq!(action_for("E"), EditAction::Edit);
        assert_eq!(action_for("!"), EditAction::Reuse);
        assert_eq!(action_for("  ! reuse"), EditAction::Reuse);
        assert_eq!(action_for(""), EditAction::Continue);
        assert_eq!(action_for("whatever"), EditAction::Continue);
    }

    #[test]
    fn the_editor_text_keeps_the_template_above_the_instructions() {
        let text = initial_text(Some("bug id:\n"), &ChangePlan::default());
        assert!(text.starts_with("bug id:\n"), "{}", text);
        assert!(
            text.contains("CVS: Enter Log.  Lines beginning with `CVS:' are removed automatically"),
            "{}",
            text
        );
        assert!(text.contains("CVS: Committing in .\n"), "{}", text);
    }

    #[test]
    fn the_editor_text_lists_the_files_being_committed() {
        let mut plan = ChangePlan::default();
        plan.modified.push("a.txt".to_string());
        plan.added.push("d/new.txt".to_string());
        plan.removed.push("d/gone.txt".to_string());
        let text = initial_text(None, &plan);
        assert!(
            text.contains("CVS: Modified Files:\nCVS: \ta.txt \n"),
            "{}",
            text
        );
        assert!(
            text.contains("CVS: Added Files:\nCVS: \td/new.txt \n"),
            "{}",
            text
        );
        assert!(
            text.contains("CVS: Removed Files:\nCVS: \td/gone.txt \n"),
            "{}",
            text
        );
    }

    #[test]
    fn cvs_instruction_lines_are_dropped_from_the_edited_message() {
        let text = "my message\n\
                    CVS: ----------------------------------------------------------------------\n\
                    CVS: Enter Log.  Lines beginning with `CVS:' are removed automatically\n";
        assert_eq!(message_from_edited(text), "my message\n");
        assert_eq!(message_from_edited("CVS: only instructions\n"), "");
        assert_eq!(
            message_from_edited("CVS:no space is user text\n"),
            "CVS:no space is user text\n"
        );
    }
}
