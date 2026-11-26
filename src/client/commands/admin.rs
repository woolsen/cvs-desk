//! `cvs admin`: pass RCS administration options to the server for the named files.
//!
//! The server runs `rcs admin` itself, so the client's job is to fold each option onto one
//! argument (`-m` and its message become `-m1.1:message`), end the option list with `--`, and
//! name the files. The reference sends the single-value options in a fixed order first, then
//! the repeatable ones in the order they were typed.
//!
//! `-t` is special: `-t-text` sets the description, `-t file` reads it from a local file, and
//! a description with line breaks travels as `Argument` plus one `Argumentx` per extra line.

use std::path::Path;

use crate::client::cli;
use crate::client::commands::targets;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

#[derive(Debug, Default)]
struct Options {
    branch: Option<String>,
    comment: Option<String>,
    strict: bool,
    nonstrict: bool,
    delete_revs: Option<String>,
    description: Option<String>,
    quiet: bool,
    kflag: Option<String>,
    /// `-a`, `-A`, `-e`, `-l`, `-u`, `-n`, `-N`, `-m`, `-s`, already folded onto one token.
    extra: Vec<String>,
    warnings: Vec<&'static str>,
    names: Vec<String>,
}

/// The value of one option: the rest of this token when something follows the letter, else the
/// next argument.
fn value(args: &[String], i: &mut usize, rest: &str, letter: char) -> Result<String> {
    if !rest.is_empty() {
        return Ok(rest.to_string());
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| Error::plain("admin", format!("option -{} requires an argument", letter)))
}

/// `-t-text` is the description; `-t file` reads it; a bare `-t` means no text.
fn description(cwd: &Path, rest: &str) -> Result<String> {
    if let Some(text) = rest.strip_prefix('-') {
        return Ok(text.to_string());
    }
    if rest.is_empty() {
        return Ok(String::new());
    }
    let path = if Path::new(rest).is_absolute() {
        Path::new(rest).to_path_buf()
    } else {
        cwd.join(rest)
    };
    let bytes = std::fs::read(&path).map_err(|e| {
        Error::plain(
            "admin",
            format!("cannot read the description file {}: {}", rest, e),
        )
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_args(cwd: &Path, args: &[String]) -> Result<Options> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            opts.names.extend(args[i + 1..].iter().cloned());
            break;
        }
        if arg.len() < 2 || !arg.starts_with('-') {
            opts.names.push(arg.clone());
            i += 1;
            continue;
        }
        let letter = arg.as_bytes()[1] as char;
        let rest = &arg[2..];
        match letter {
            'L' if rest.is_empty() => opts.strict = true,
            'U' if rest.is_empty() => opts.nonstrict = true,
            'I' if rest.is_empty() => {
                return Err(Error::plain(
                    "admin",
                    "-I option not useful with client/server",
                ))
            }
            'q' if rest.is_empty() => opts.quiet = true,
            'b' => {
                if opts.branch.is_some() {
                    return Err(Error::plain("admin", "duplicate 'b' option"));
                }
                opts.branch = Some(rest.to_string());
            }
            'c' => {
                if opts.comment.is_some() {
                    return Err(Error::plain("admin", "duplicate 'c' option"));
                }
                opts.comment = Some(value(args, &mut i, rest, letter)?);
            }
            'k' => {
                if opts.kflag.is_some() {
                    return Err(Error::plain("admin", "duplicate '-k' option"));
                }
                opts.kflag = Some(value(args, &mut i, rest, letter)?);
            }
            'o' => {
                if opts.delete_revs.is_some() {
                    return Err(Error::plain("admin", "duplicate '-o' option"));
                }
                opts.delete_revs = Some(value(args, &mut i, rest, letter)?);
            }
            't' => {
                if opts.description.is_some() {
                    return Err(Error::plain("admin", "duplicate 't' option"));
                }
                opts.description = Some(description(cwd, rest)?);
            }
            'a' | 'A' | 'e' | 'l' | 'u' | 'n' | 'N' | 'm' | 's' => {
                // `-e`, `-l` and `-u` take their value only when it is attached.
                let attached_only = matches!(letter, 'e' | 'l' | 'u');
                let rest = if attached_only {
                    rest.to_string()
                } else {
                    value(args, &mut i, rest, letter)?
                };
                opts.extra.push(format!("-{}{}", letter, rest));
            }
            'V' => {
                i += 1;
                opts.warnings.push("the `-V' option is obsolete");
            }
            _ => {
                return Err(Error::plain(
                    "admin",
                    "specify cvs -H admin for usage information",
                ))
            }
        }
        i += 1;
    }
    Ok(opts)
}

fn usage(sink: &mut dyn Sink) {
    if let Some(text) = cli::command_help("admin") {
        for line in text.lines() {
            sink.err(line.to_string());
        }
    }
}

/// Run `cvs admin`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    if args.is_empty() {
        usage(sink);
        return Ok(1);
    }
    let opts = parse_args(cwd, args)?;
    for warning in &opts.warnings {
        sink.err(format!("cvs admin: {}", warning));
    }
    if opts.strict && opts.nonstrict {
        return Err(Error::plain("admin", "-U and -L are incompatible"));
    }
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "admin",
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    let targets = targets::collect(cwd, true, &opts.names)?;
    if targets.is_empty() {
        return Err(Error::aborted("admin", "no files to administer"));
    }

    let mut session = protocol::open(root, "admin", cancel, sink)?;
    if let Some(branch) = &opts.branch {
        session.argument(&format!("-b{}", branch))?;
    }
    if let Some(comment) = &opts.comment {
        session.argument(&format!("-c{}", comment))?;
    }
    if opts.strict {
        session.argument("-L")?;
    }
    if opts.nonstrict {
        session.argument("-U")?;
    }
    if let Some(revisions) = &opts.delete_revs {
        session.argument(&format!("-o{}", revisions))?;
    }
    if let Some(text) = &opts.description {
        session.argument(&format!("-t-{}", text))?;
    }
    if opts.quiet {
        session.argument("-q")?;
    }
    if let Some(kflag) = &opts.kflag {
        session.argument(&format!("-k{}", kflag))?;
    }
    for option in &opts.extra {
        session.argument(option)?;
    }
    session.argument("--")?;

    let last = targets::send_blocks_watch(&mut session, &targets)?;
    targets::finish_from_last(&mut session, cwd, &opts.names, "admin", last.as_deref())?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}
