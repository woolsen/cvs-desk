//! `cvs update`: bring the working copy in line with the repository.
//!
//! The request is the one the reference client sends: the folder blocks and file states, then
//! `Directory .` and `update`. The server answers with the same file shapes `checkout` uses,
//! plus `Copy-file` (save the working file before a merge), `Merged` (the merged contents),
//! `Removed`/`Remove-entry` and the sticky-tag changes.
//!
//! This client does not advertise `Rcs-diff`, so the server sends whole files instead of RCS
//! patches. That is a legitimate CVS configuration (it is what a client without patch support
//! gets) and it removes the need for an external patch program.

use std::path::Path;

use crate::client::commands::targets;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, FileKind, Response};
use crate::client::root::Root;
use crate::client::work::{codec, WorkWriter};

#[derive(Debug, Default)]
struct Options {
    recursive: bool,
    quiet: u32,
    /// The global `-n`: report, but do not change a disk. CVSNT's own `update` refuses `-n`,
    /// but the desktop program asks for it, so it is accepted here and behaves like the
    /// global option.
    no_execute: bool,
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
            "-n" => opts.no_execute = true,
            "-P" | "-A" | "-C" | "-f" | "-d" => opts.server.push(arg.clone()),
            "-p" => {
                return Err(Error::plain(
                    "update",
                    "sending updates to standard output (-p) is not supported by this cvs yet",
                ))
            }
            "-k" | "-r" | "-D" | "-j" | "-I" | "-W" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    Error::aborted("update", format!("option {} requires an argument", arg))
                })?;
                opts.server.push(format!("{}{}", arg, value));
            }
            other
                if (other.starts_with("-k")
                    || other.starts_with("-r")
                    || other.starts_with("-D")
                    || other.starts_with("-j")
                    || other.starts_with("-I")
                    || other.starts_with("-W"))
                    && other.len() > 2 =>
            {
                opts.server.push(other.to_string())
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

    let mut session = protocol::open(root, "update", cancel, sink)?;
    if opts.quiet == 1 {
        session.send("Global_option -q")?;
    } else if opts.quiet >= 2 {
        session.send("Global_option -Q")?;
    }
    if opts.no_execute {
        // The reference sends both: -n is "do not change the disk", -l is "no history".
        session.send("Global_option -n")?;
        session.send("Global_option -l")?;
    }
    // `-u` is the ordinary "update the working file" mode (the alternative is -p).
    session.argument("-u")?;
    for option in &opts.server {
        session.argument(option)?;
    }

    targets::send_blocks(&mut session, &targets, false)?;
    targets::finish_to_top(&mut session, cwd, &opts.names, "update")?;

    let mut writer = WorkWriter::new(cwd, session.root.clone())
        .readonly(readonly)
        .dry_run(opts.no_execute);
    let mut pending_mtime = None;
    let mut changed = false;
    let mut conflicts = false;
    let mut saw_report = false;
    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => {
                // U/M/C/R/P/? all mean the working copy differs from the repository.
                let report = opts.no_execute && text.len() > 1;
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
                if kind == FileKind::Merged && entry.contains("+=/") {
                    conflicts = true;
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
                writer.copy_file(&update_dir, &repository, &name)?;
                changed = true;
                saw_report = true;
            }
            Response::Removed(path) => {
                let (dir, name) = split_local(&path);
                writer.remove_file(&dir, &name)?;
                changed = true;
                saw_report = true;
            }
            Response::RemoveEntry(path) => {
                let (dir, name) = split_local(&path);
                writer.remove_entry(&dir, &name)?;
                changed = true;
                saw_report = true;
            }
            Response::Template { dir, content } => {
                writer.write_template(&dir, &content)?;
            }
            Response::SetStaticDirectory { dir, repository } => {
                writer.set_static_directory(&dir, &repository)?;
            }
            Response::ClearStaticDirectory { dir, repository } => {
                writer.clear_static_directory(&dir, &repository)?;
            }
            Response::SetSticky {
                dir,
                repository,
                tag,
            } => {
                writer.set_sticky(&dir, &repository, &tag)?;
            }
            Response::ClearSticky { dir, repository } => {
                writer.clear_sticky(&dir, &repository)?;
            }
            Response::Error { message, .. } => {
                // CVSNT ends a `Patched` update with an empty `error` instead of `ok`. The
                // file has already been written and the entry recorded, so this is a
                // successful end, not a failure.
                let empty_end = message.trim().is_empty() && (changed || saw_report);
                writer.finish()?;
                session.close();
                if empty_end {
                    return Ok(if conflicts || opts.no_execute && changed {
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
                writer.record_entry(&update_dir, &repository, &entry)?;
            }
            Response::NewEntry(_)
            | Response::Mode(_)
            | Response::F(_)
            | Response::ValidRequests(_)
            | Response::ModuleExpansion(_)
            | Response::Notified
            | Response::Trace(_)
            | Response::Checksum(_) => {}
        }
    }

    writer.finish()?;
    session.close();
    // `cvs -n update` exits 1 when there would have been something to do; a conflict also
    // makes the update fail, as it does in the reference client.
    Ok(if conflicts || opts.no_execute && changed {
        1
    } else {
        0
    })
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
    fn a_removed_path_splits_into_folder_and_name() {
        assert_eq!(split_local("d/two.txt"), ("d".into(), "two.txt".into()));
        assert_eq!(split_local("two.txt"), ("".into(), "two.txt".into()));
        assert_eq!(split_local("./d/two.txt"), ("d".into(), "two.txt".into()));
    }

    #[test]
    fn the_command_accepts_the_desktop_programs_n() {
        let opts = parse_args(&["-n".to_string(), "-P".to_string()]).unwrap();
        assert!(opts.no_execute);
        assert!(opts.server.iter().any(|s| s == "-P"));
    }
}
