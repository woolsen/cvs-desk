//! `cvs commit`: send the working files' changes to the repository.
//!
//! The client sends the same folder blocks and file states as `update`, with the log message
//! first, then `ci` (the protocol's name for commit). The server answers with `Checked-in`
//! for each file it accepted, which is where the new revision and sticky tag come from.
//!
//! Without `-m` or `-F` the message comes from an editor: `$CVSEDITOR`, `$VISUAL`, `$EDITOR`,
//! or Notepad on Windows. The file starts with `CVS/Template` when the repository sent one
//! during checkout or update, then `CVS: ` instruction lines, which are removed on the way
//! back. This client has no `-e`, so there is no way to pick the editor on the command line.

use std::path::Path;

use crate::client::commands::targets;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, Response};
use crate::client::root::Root;
use crate::client::work::base;
use crate::client::work::codec;
use crate::client::work::entries::Entry;
use crate::client::work::WorkWriter;

#[derive(Debug, Default)]
struct Options {
    recursive: bool,
    quiet: u32,
    message: Option<String>,
    force: bool,
    server: Vec<String>,
    names: Vec<String>,
}

fn parse_args(args: &[String]) -> Result<Options> {
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
            "-q" => opts.quiet = opts.quiet.max(1),
            "-Q" => opts.quiet = 2,
            "-f" => opts.force = true,
            "-m" => {
                i += 1;
                opts.message = Some(require(args, i, "-m")?);
            }
            "-F" => {
                i += 1;
                let path = require(args, i, "-F")?;
                let bytes = std::fs::read(&path).map_err(|e| {
                    Error::aborted(
                        "commit",
                        format!("cannot read the message file {}: {}", path, e),
                    )
                })?;
                opts.message = Some(String::from_utf8_lossy(&bytes).into_owned());
            }
            "-r" | "-j" => {
                i += 1;
                let value = require(args, i, arg)?;
                opts.server.push(format!("{}{}", arg, value));
            }
            other if (other.starts_with("-r") || other.starts_with("-j")) && other.len() > 2 => {
                opts.server.push(other.to_string())
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "commit",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.names.push(other.to_string()),
        }
        i += 1;
    }
    Ok(opts)
}

fn require(args: &[String], i: usize, option: &str) -> Result<String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| Error::aborted("commit", format!("option {} requires an argument", option)))
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
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "commit",
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    let message = match opts.message {
        Some(message) if !message.trim().is_empty() => message,
        Some(_) => return Err(Error::aborted("commit", "empty log message")),
        None => edit_message(cwd, sink)?,
    };

    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("commit", "no files to commit"));
    }

    let mut session = protocol::open(root, "commit", cancel, sink)?;
    if opts.quiet == 1 {
        session.send("Global_option -q")?;
    } else if opts.quiet >= 2 {
        session.send("Global_option -Q")?;
    }
    session.argument("-m")?;
    session.argument(&message)?;
    for option in &opts.server {
        session.argument(option)?;
    }

    targets::send_blocks(&mut session, &targets, false)?;
    targets::finish_to_top(&mut session, cwd, &opts.names, "ci")?;

    let mut writer = WorkWriter::new(cwd, session.root.clone()).readonly(readonly);
    let mut saw_checked_in = false;
    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::CheckedIn {
                update_dir,
                repository,
                entry,
            } => {
                if let Some(parsed) = Entry::parse(&entry) {
                    base::forget(&writer.local_for(&update_dir), &parsed.name)?;
                }
                writer.record_entry(&update_dir, &repository, &entry)?;
                saw_checked_in = true;
            }
            Response::Removed(path) => {
                let (dir, name) = split_local(&path);
                base::forget(&writer.local_for(&dir), &name)?;
                writer.remove_file(&dir, &name)?;
                writer.remove_entry(&dir, &name)?;
            }
            Response::RemoveEntry(path) => {
                let (dir, name) = split_local(&path);
                base::forget(&writer.local_for(&dir), &name)?;
                writer.remove_entry(&dir, &name)?;
            }
            Response::Error { message, .. } => {
                writer.finish()?;
                session.close();
                if message.trim().is_empty() && saw_checked_in {
                    return Ok(0);
                }
                return Err(Error::aborted(
                    "commit",
                    if message.trim().is_empty() {
                        "the server reported an error".to_string()
                    } else {
                        message
                    },
                ));
            }
            Response::Mode(_)
            | Response::NewEntry(_)
            | Response::F(_)
            | Response::ValidRequests(_)
            | Response::ModuleExpansion(_)
            | Response::Trace(_)
            | Response::Checksum(_)
            | Response::Template { .. }
            | Response::Notified
            | Response::File { .. }
            | Response::CopyFile { .. }
            | Response::ModTime(_)
            | Response::SetStaticDirectory { .. }
            | Response::ClearStaticDirectory { .. }
            | Response::SetSticky { .. }
            | Response::ClearSticky { .. } => {}
        }
    }

    writer.finish()?;
    session.close();
    Ok(0)
}

/// The text the editor starts with: the folder's `CVS/Template` (when the repository set
/// one), then the instruction lines that are removed again on the way back.
fn initial_text(template: Option<&str>) -> String {
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
    text.push_str("CVS: ----------------------------------------------------------------------\n");
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

/// The editor the reference picks: `CVSEDITOR`, then `VISUAL`, then `EDITOR`, then the
/// platform's plain editor.
fn editor_command() -> String {
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
fn edit_message(cwd: &Path, sink: &mut dyn Sink) -> Result<String> {
    let template = std::fs::read(cwd.join("CVS").join("Template"))
        .ok()
        .map(|bytes| codec::detect(&bytes).decode(&bytes));
    let initial = initial_text(template.as_deref());

    let path = std::env::temp_dir().join(format!(
        "cvsmsg-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, codec::to_platform_newlines(initial.as_bytes()))?;

    let editor = editor_command();
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("");
    if program.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Err(Error::aborted(
            "commit",
            "no editor defined; use -m \"message\" or -F file",
        ));
    }
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status();
    match status {
        Ok(status) if status.success() => {}
        Ok(_) => sink.err("cvs commit: warning: editor session failed".to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(Error::aborted(
                "commit",
                format!("cannot run editor {}: {}", program, e),
            ));
        }
    }

    let bytes = std::fs::read(&path)?;
    let _ = std::fs::remove_file(&path);
    let message = message_from_edited(&String::from_utf8_lossy(&bytes));
    if message.trim().is_empty() {
        return Err(Error::aborted("commit", "empty log message"));
    }
    Ok(message)
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

    #[test]
    fn the_message_file_is_read_and_the_name_is_kept() {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-commit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("msg.txt");
        std::fs::write(&file, "from a file\n").unwrap();
        let opts = parse_args(&[
            "-F".to_string(),
            file.display().to_string(),
            "a.txt".to_string(),
        ])
        .unwrap();
        assert_eq!(opts.message.as_deref(), Some("from a file\n"));
        assert_eq!(opts.names, vec!["a.txt"]);
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
    fn the_editor_text_keeps_the_template_above_the_instructions() {
        let text = initial_text(Some("bug id:\n"));
        assert!(text.starts_with("bug id:\n"), "{}", text);
        assert!(
            text.contains("CVS: Enter Log.  Lines beginning with `CVS:' are removed automatically"),
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
