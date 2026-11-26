//! `cvs diff`: compare the working files with the repository.
//!
//! The server runs the comparison and sends the diff text; this client supplies the folder
//! blocks, the file states, and the options the user asked for. Like `diff` itself, the
//! command exits 1 when there is a difference and 0 when there is none.

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
    /// Arguments the server must see, in the user's order.
    server: Vec<String>,
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
            "-u" | "-c" | "-w" | "-b" | "-B" | "-i" | "-a" | "-N" | "-t" | "-f" | "-p"
            | "--brief" | "--text" | "--binary" | "--unified" | "--normal" => {
                opts.server.push(arg.clone());
            }
            "-r" | "-D" | "-k" | "-I" | "-F" | "-C" | "-U" | "-W" | "-L" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    Error::aborted("diff", format!("option {} requires an argument", arg))
                })?;
                opts.server.push(format!("{}{}", arg, value));
            }
            other
                if (other.starts_with("-r")
                    || other.starts_with("-D")
                    || other.starts_with("-k")
                    || other.starts_with("-I")
                    || other.starts_with("-F")
                    || other.starts_with("-C")
                    || other.starts_with("-U")
                    || other.starts_with("-W")
                    || other.starts_with("-L"))
                    && other.len() > 2 =>
            {
                opts.server.push(other.to_string());
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "diff",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.names.push(other.to_string()),
        }
        i += 1;
    }
    Ok(opts)
}

/// Run `cvs diff`. The exit code is 1 when the server printed a difference, 0 otherwise.
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
            "diff",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("diff", "no files to examine"));
    }

    let mut session = protocol::open(root, "diff", cancel, sink)?;
    if opts.quiet == 1 {
        session.send("Global_option -q")?;
    } else if opts.quiet >= 2 {
        session.send("Global_option -Q")?;
    }
    for option in &opts.server {
        session.argument(option)?;
    }

    targets::send_blocks(&mut session, &targets, false)?;
    targets::finish_to_top(&mut session, cwd, &opts.names, "diff")?;
    let differences = targets::read_text(&mut session, false)?;
    session.close();
    Ok(if differences { 1 } else { 0 })
}
