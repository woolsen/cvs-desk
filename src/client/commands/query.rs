//! Read-only queries the server answers with text: `annotate`, `editors`, `watchers`.
//!
//! They share `status`'s folder blocks: each folder, then each file's state, then the command
//! from the invocation folder. `annotate` additionally takes the revision options; `editors`
//! and `watchers` make no sense without a file name and say nothing when given none, exactly
//! as the reference client does.

use std::path::Path;

use crate::client::commands::targets;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

#[derive(Debug, Default)]
struct AnnotateOptions {
    recursive: bool,
    quiet: u32,
    names: Vec<String>,
    server: Vec<String>,
}

fn parse_annotate(args: &[String]) -> Result<AnnotateOptions> {
    let mut opts = AnnotateOptions {
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
            "-f" => opts.server.push(arg.clone()),
            "-r" | "-D" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    Error::aborted("annotate", format!("option {} requires an argument", arg))
                })?;
                opts.server.push(format!("{}{}", arg, value));
            }
            other if (other.starts_with("-r") || other.starts_with("-D")) && other.len() > 2 => {
                opts.server.push(other.to_string())
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "annotate",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.names.push(other.to_string()),
        }
        i += 1;
    }
    Ok(opts)
}

/// Run `cvs annotate`.
pub fn annotate(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = parse_annotate(args)?;
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "annotate",
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("annotate", "no files to annotate"));
    }
    let mut session = protocol::open(root, "annotate", cancel, sink)?;
    if opts.quiet == 1 {
        session.send("Global_option -q")?;
    } else if opts.quiet >= 2 {
        session.send("Global_option -Q")?;
    }
    for option in &opts.server {
        session.argument(option)?;
    }
    targets::send_blocks(&mut session, &targets, false)?;
    targets::finish_to_top(&mut session, cwd, &opts.names, "annotate")?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

/// Run `cvs editors` or `cvs watchers`. With no file names the reference sends nothing and
/// exits 1, so this does the same without opening a connection.
pub fn file_query(
    cwd: &Path,
    root: Root,
    command: &str,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let mut names = Vec::new();
    for arg in args {
        match arg.as_str() {
            "-l" | "-R" => {}
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    command,
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => names.push(other.to_string()),
        }
    }
    if names.is_empty() {
        return Ok(1);
    }
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            command,
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    let targets = targets::collect(cwd, false, &names)?;
    if targets.is_empty() {
        return Ok(1);
    }
    let mut session = protocol::open(root, command, cancel, sink)?;
    targets::send_blocks(&mut session, &targets, false)?;
    targets::finish_to_top(&mut session, cwd, &names, command)?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}
