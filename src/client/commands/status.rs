//! `cvs status`: what the server knows about each file, and where the working copy differs.
//!
//! The server formats the answer: this client's job is to say which directories and files to
//! look at, and to tell it which files are unchanged, modified or absent. Sending a file's
//! contents is how CVS lets the server say "locally modified" or "had conflicts"; unchanged
//! files cost only one `Unchanged` line each.

use std::path::Path;

use crate::client::commands::targets;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

#[derive(Debug, Default)]
struct Options {
    recursive: bool,
    quiet: u32,
    names: Vec<String>,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options {
        recursive: true,
        ..Default::default()
    };
    for arg in args {
        match arg.as_str() {
            "-l" => opts.recursive = false,
            "-R" => opts.recursive = true,
            "-q" => opts.quiet = opts.quiet.max(1),
            "-Q" => opts.quiet = 2,
            "-v" => {}
            "--" => {}
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "status",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.names.push(other.to_string()),
        }
    }
    Ok(opts)
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
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "status",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("status", "no files to examine"));
    }

    let mut session = protocol::open(root, "status", cancel, sink)?;
    if opts.quiet == 1 {
        session.send("Global_option -q")?;
    } else if opts.quiet >= 2 {
        session.send("Global_option -Q")?;
    }

    targets::send_blocks(&mut session, &targets, true)?;
    targets::finish_to_top(&mut session, cwd, &opts.names, "status")?;
    let _ = targets::read_text(&mut session, true)?;
    session.close();
    Ok(0)
}
