//! `cvs checkout`: bring a module out of the repository into a working folder.
//!
//! The server drives this command: after `expand-modules` it names each folder and sends each
//! file as a `Created`/`Updated` block. This client creates the folders the server names,
//! writes their `CVS/Root`, `CVS/Repository` and `CVS/Entries`, and records each file's own
//! time in its entry — the same rule the desktop program checks.
//!
//! The request order is the one the reference CVSNT client sends (captured from a real
//! session): `expand-modules` first, then `-N` and the module for `co`. `export` uses the
//! same transfer with its own request name and without the `CVS` folders, so both live here.

use std::path::Path;

use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, FileKind, Response};
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::WorkWriter;

#[derive(Debug, Default, Clone)]
pub struct CheckoutOptions {
    pub destination: Option<String>,
    pub revision: Option<String>,
    pub date: Option<String>,
    pub prune: bool,
    /// `-N`: do not create the CVS control folders (the way export works).
    pub no_cvs: bool,
    pub force: bool,
    pub reset_sticky: bool,
    pub local_only: bool,
    pub keyword: Option<String>,
    pub readonly: bool,
}

/// Parse the command's own options and return them with the module names.
pub fn parse_args(command: &str, args: &[String]) -> Result<(CheckoutOptions, Vec<String>)> {
    let mut opts = CheckoutOptions::default();
    let mut modules = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--" => {
                modules.extend(args[i + 1..].iter().cloned());
                break;
            }
            "-d" => {
                i += 1;
                opts.destination = Some(arg_value(command, args, i, "-d")?);
            }
            "-r" => {
                i += 1;
                opts.revision = Some(arg_value(command, args, i, "-r")?);
            }
            "-D" => {
                i += 1;
                opts.date = Some(arg_value(command, args, i, "-D")?);
            }
            "-P" => opts.prune = true,
            "-N" => opts.no_cvs = true,
            "-f" => opts.force = true,
            "-A" => opts.reset_sticky = true,
            "-l" => opts.local_only = true,
            "-R" => opts.local_only = false,
            "-k" => {
                i += 1;
                opts.keyword = Some(arg_value(command, args, i, "-k")?);
            }
            other if other.starts_with("-r") && other.len() > 2 => {
                opts.revision = Some(other[2..].to_string());
            }
            other if other.starts_with("-D") && other.len() > 2 => {
                opts.date = Some(other[2..].to_string());
            }
            other if other.starts_with("-k") && other.len() > 2 => {
                opts.keyword = Some(other[2..].to_string());
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    command,
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => modules.push(other.to_string()),
        }
        i += 1;
    }
    Ok((opts, modules))
}

fn arg_value(command: &str, args: &[String], i: usize, option: &str) -> Result<String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| Error::aborted(command, format!("option {} requires an argument", option)))
}

/// Run `cvs checkout`. `cwd` is the folder the command was started in.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    readonly: bool,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    run_as("checkout", cwd, root, args, readonly, cancel, sink)
}

/// The transfer `checkout` and `export` share. They differ in the request name, and in that
/// export writes no `CVS` folders and never prunes: the reference sends `-N` for both.
pub fn run_as(
    command: &str,
    cwd: &Path,
    root: Root,
    args: &[String],
    readonly: bool,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let export = command == "export";
    let (mut opts, modules) = parse_args(command, args)?;
    opts.readonly |= readonly;
    if export {
        if cwd.join("CVS").is_dir() {
            return Err(Error::aborted(
                command,
                "cannot export into a working directory",
            ));
        }
        opts.no_cvs = true;
        opts.prune = false;
    }
    if modules.is_empty() {
        return Err(Error::aborted(command, "No modules specified"));
    }
    if opts.destination.is_some() && modules.len() > 1 {
        return Err(Error::aborted(
            command,
            "the -d option can only be used with a single module",
        ));
    }

    let mut session = protocol::open(root, command, cancel, sink)?;
    let protocol_root = session.root.protocol_root().to_string();

    // Ask what the modules expand to. The answer also proves the module exists before any
    // file is written.
    for module in &modules {
        session.argument(module)?;
    }
    session.directory(".", &protocol_root, cwd)?;
    session.send("expand-modules")?;
    session.flush()?;
    let mut expanded = Vec::new();
    loop {
        match session.read_response()? {
            Response::ModuleExpansion(name) => expanded.push(name),
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::Error { message, .. } => {
                return Err(Error::aborted(
                    command,
                    if message.trim().is_empty() {
                        "the server could not expand the module".to_string()
                    } else {
                        message
                    },
                ))
            }
            other => {
                return Err(Error::aborted(
                    command,
                    format!("unexpected answer to expand-modules: {:?}", other),
                ))
            }
        }
    }
    if expanded.is_empty() {
        expanded = modules.clone();
    }

    // The reference client asks for `co -N`: the server names the folders, the client writes
    // the CVS folders itself.
    session.argument("-N")?;
    if let Some(revision) = &opts.revision {
        session.argument("-r")?;
        session.argument(revision)?;
    }
    if let Some(date) = &opts.date {
        session.argument("-D")?;
        session.argument(date)?;
    }
    if opts.prune {
        session.argument("-P")?;
    }
    if opts.force {
        session.argument("-f")?;
    }
    if opts.reset_sticky {
        session.argument("-A")?;
    }
    if let Some(keyword) = &opts.keyword {
        session.argument(&format!("-k{}", keyword))?;
    }
    if opts.local_only {
        session.argument("-l")?;
    }
    for module in &modules {
        session.argument(module)?;
    }
    session.directory(".", &protocol_root, cwd)?;
    // The request is `co`; export is the one command whose request spells out its name.
    session.send(if export { "export" } else { "co" })?;
    session.flush()?;

    let mut writer = WorkWriter::new(cwd, session.root.clone())
        .with_modules(&expanded)
        .with_destination(opts.destination.clone())
        .no_cvs(opts.no_cvs)
        .readonly(opts.readonly);
    let mut pending_mtime = None;
    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::ModTime(text) => pending_mtime = codec::parse_mod_time(&text),
            Response::File {
                kind,
                update_dir,
                repository,
                entry,
                mode,
                content,
            } => {
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
                return Err(Error::aborted(
                    command,
                    if message.trim().is_empty() {
                        "the server reported an error".to_string()
                    } else {
                        message
                    },
                ))
            }
            Response::Removed(_)
            | Response::CopyFile { .. }
            | Response::CheckedIn { .. }
            | Response::NewEntry(_)
            | Response::Mode(_)
            | Response::RemoveEntry(_)
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
    Ok(0)
}
