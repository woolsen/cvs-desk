//! `cvs log`: the revision history of files, formatted by the server.
//!
//! Like `status`, this client only names the folders and files and prints the server's text.
//! Options that change what is printed (`-N`, `-h`, `-R`, `-r`, `-d`, `-s`, `-w`, `-b`,
//! `-t`, `-S`) are passed as protocol arguments; `-l` and quietness are handled locally.

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
    /// Arguments the server must see, in the order the user wrote them.
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
            "-q" => opts.quiet = opts.quiet.max(1),
            "-Q" => opts.quiet = 2,
            "-N" | "-h" | "-R" | "-t" | "-S" | "-b" | "-w" => {
                opts.server.push(arg.clone());
            }
            "-r" | "-d" | "-s" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    Error::aborted("log", format!("option {} requires an argument", arg))
                })?;
                opts.server.push(format!("{}{}", arg, value));
            }
            other
                if (other.starts_with("-r")
                    || other.starts_with("-d")
                    || other.starts_with("-s"))
                    && other.len() > 2 =>
            {
                opts.server.push(other.to_string());
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "log",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.names.push(other.to_string()),
        }
        i += 1;
    }
    Ok(opts)
}

/// Run `cvs log`.
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
            "log",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("log", "no files to examine"));
    }

    let mut session = protocol::open(root, "log", cancel, sink)?;
    if opts.quiet == 1 {
        session.send("Global_option -q")?;
    } else if opts.quiet >= 2 {
        session.send("Global_option -Q")?;
    }
    for option in &opts.server {
        session.argument(option)?;
    }

    targets::send_blocks(&mut session, &targets, false)?;
    targets::finish_to_top(&mut session, cwd, &opts.names, "log")?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}
