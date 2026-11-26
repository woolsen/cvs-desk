//! `cvs rdiff`: a patch between two revisions of a module, with no working copy needed.
//!
//! The reference names every value separately (`-r` then `1.1`), and sends the flags in a
//! fixed order: `-l`, `-f`, `-t`, `-s`, `-u`, then the first revision or date, the second,
//! the keyword option, and the modules. Two revisions or dates at most; `-t` (the last two
//! diffs) cannot be combined with either or with `-s`.

use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

const USAGE: &[&str] = &[
    "Usage: cvs rdiff [-flR] [-c|-u] [-s|-t] [-V %d] [-k kopt]",
    "\t-r rev|-D date [-r rev2 | -D date2] modules...",
];

#[derive(Debug, Default)]
struct Options {
    local: bool,
    force_tag_match: bool,
    top_two: bool,
    short: bool,
    unidiff: bool,
    rev1: Option<String>,
    date1: Option<String>,
    rev2: Option<String>,
    date2: Option<String>,
    kflag: Option<String>,
    modules: Vec<String>,
}

impl Options {
    /// `-r` and `-D` fill the first free slot, then the second; a third is an error.
    fn revision(&mut self, value: String) -> Result<()> {
        if self.rev2.is_some() || self.date2.is_some() {
            return Err(Error::plain(
                "rdiff",
                "no more than two revisions/dates can be specified",
            ));
        }
        if self.rev1.is_some() || self.date1.is_some() {
            self.rev2 = Some(value);
        } else {
            self.rev1 = Some(value);
        }
        Ok(())
    }

    fn dated(&mut self, value: String) -> Result<()> {
        if self.rev2.is_some() || self.date2.is_some() {
            return Err(Error::plain(
                "rdiff",
                "no more than two revisions/dates can be specified",
            ));
        }
        if self.rev1.is_some() || self.date1.is_some() {
            self.date2 = Some(value);
        } else {
            self.date1 = Some(value);
        }
        Ok(())
    }
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg.len() > 2 {
            if let Some(letter) = arg.strip_prefix('-').and_then(|rest| {
                rest.starts_with(|c: char| "DrkV".contains(c))
                    .then(|| rest.chars().next().unwrap())
            }) {
                let value = arg[2..].to_string();
                match letter {
                    'r' => opts.revision(value)?,
                    'D' => opts.dated(value)?,
                    'k' => opts.kflag = Some(value),
                    _ => {
                        return Err(Error::plain(
                            "rdiff",
                            "the -V option is obsolete and should not be used",
                        ))
                    }
                }
                i += 1;
                continue;
            }
        }
        match arg.as_str() {
            "--" => {
                opts.modules.extend(args[i + 1..].iter().cloned());
                break;
            }
            "-f" => opts.force_tag_match = true,
            "-l" => opts.local = true,
            "-R" => opts.local = false,
            "-t" => opts.top_two = true,
            "-s" => opts.short = true,
            "-u" => opts.unidiff = true,
            "-c" => opts.unidiff = false,
            "-q" | "-Q" => {
                return Err(Error::plain(
                    "rdiff",
                    "-q or -Q must be specified before \"rdiff\"",
                ))
            }
            "-r" | "-D" | "-k" | "-V" => {
                i += 1;
                let value = args.get(i).cloned().ok_or_else(|| {
                    Error::plain("rdiff", format!("option {} requires an argument", arg))
                })?;
                match arg.as_str() {
                    "-r" => opts.revision(value)?,
                    "-D" => opts.dated(value)?,
                    "-k" => opts.kflag = Some(value),
                    _ => {
                        return Err(Error::plain(
                            "rdiff",
                            "the -V option is obsolete and should not be used",
                        ))
                    }
                }
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "rdiff",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.modules.push(other.to_string()),
        }
        i += 1;
    }
    Ok(opts)
}

fn usage(sink: &mut dyn Sink) {
    for line in USAGE {
        sink.err(line.to_string());
    }
    sink.err("(Specify the --help global option for a list of other help options)".to_string());
}

/// Run `cvs rdiff`.
pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    let opts = parse_args(args)?;
    if opts.modules.is_empty() {
        usage(sink);
        return Ok(1);
    }
    if opts.top_two && opts.short {
        return Err(Error::plain(
            "rdiff",
            "-t and -s options are mutually exclusive",
        ));
    }
    let any_revision =
        opts.rev1.is_some() || opts.date1.is_some() || opts.rev2.is_some() || opts.date2.is_some();
    if opts.top_two && any_revision {
        return Err(Error::plain(
            "rdiff",
            "must not specify revisions/dates with -t option!",
        ));
    }
    if !opts.top_two && !any_revision {
        return Err(Error::plain(
            "rdiff",
            "must specify at least one revision/date!",
        ));
    }

    let mut session = protocol::open(root, "rdiff", cancel, sink)?;
    if opts.local {
        session.argument("-l")?;
    }
    if opts.force_tag_match {
        session.argument("-f")?;
    }
    if opts.top_two {
        session.argument("-t")?;
    }
    if opts.short {
        session.argument("-s")?;
    }
    if opts.unidiff {
        session.argument("-u")?;
    }
    if let Some(rev) = &opts.rev1 {
        session.argument("-r")?;
        session.argument(rev)?;
    }
    if let Some(date) = &opts.date1 {
        session.argument("-D")?;
        session.argument(&date::normalize(date))?;
    }
    if let Some(rev) = &opts.rev2 {
        session.argument("-r")?;
        session.argument(rev)?;
    }
    if let Some(date) = &opts.date2 {
        session.argument("-D")?;
        session.argument(&date::normalize(date))?;
    }
    if let Some(kflag) = &opts.kflag {
        session.argument(&format!("-k{}", kflag))?;
    }
    for module in &opts.modules {
        session.argument(module)?;
    }
    session.send("rdiff")?;
    session.flush()?;
    targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}
