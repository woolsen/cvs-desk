//! `cvs unedit`: say that a watched file is no longer being edited.
//!
//! Like `edit` it has no request of its own. The client names the folder, sends one `Notify`
//! per file with a `U` line, and finishes with `noop`; the file itself is made read-only
//! again, which is what marks it unedited for the next checkout.
//!
//! A file whose recorded time no longer matches was changed while it was writable. The
//! reference asks whether to keep the changes before it reverts: the answer comes from the
//! sink, and the file is kept as it is unless the answer starts with `y`. A declined file is
//! left writable and is not reported to the server.

use std::path::Path;

use crate::client::commands::notify;
use crate::client::commands::targets::{self, Target};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::dir;
use crate::client::work::entries::Entry;

#[derive(Debug)]
struct Options {
    recursive: bool,
    names: Vec<String>,
}

/// Like `add`, `remove` and `edit`, `unedit` stops taking options at the first name.
fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options {
        recursive: true,
        names: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            opts.names.extend(args[i + 1..].iter().cloned());
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            opts.names.extend(args[i..].iter().cloned());
            break;
        }
        match arg.as_str() {
            "-l" => opts.recursive = false,
            "-R" => opts.recursive = true,
            other => {
                return Err(Error::plain(
                    "unedit",
                    format!("invalid option -- '{}'", other),
                ))
            }
        }
        i += 1;
    }
    Ok(opts)
}

/// Run `cvs unedit`.
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
            "unedit",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    let targets = targets::collect(cwd, opts.recursive, &opts.names)?;
    let mut wanted = Vec::new();
    for (target, entry) in notify::selected(&targets) {
        if target.dir.local.join(&entry.name).is_file() {
            wanted.push((target, entry));
        }
    }
    if wanted.is_empty() {
        return Err(Error::aborted("unedit", "no files to unedit"));
    }

    let mut session = protocol::open(root, "unedit", cancel, sink)?;
    let mut notices = Vec::new();
    for (target, entry) in wanted {
        let local = target.dir.local.join(&entry.name);
        if is_modified(target, entry) {
            if !session.confirm(format!(
                "{} has been modified; revert changes? ",
                dir::display_path(&target.dir.update_dir, &entry.name)
            )) {
                continue;
            }
        }
        // `edit` saved the file; put that copy back before making it read-only again.
        crate::client::work::base::restore(&target.dir.local, &entry.name)?;
        dir::set_readonly(&local, true)?;
        notices.push(notify::Notice {
            dir: target.dir.update_dir.clone(),
            name: entry.name.clone(),
            kind: 'U',
            watches: String::new(),
        });
    }
    notify::send(&mut session, &targets, &notices, cwd)?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

/// The same check `tag -c` makes: a file waiting for a commit (added, conflicted, half
/// updated) counts as modified, and so does one whose recorded time no longer matches. A
/// missing file cannot be modified; there is nothing to compare.
fn is_modified(target: &Target, entry: &Entry) -> bool {
    if entry.is_added() || entry.is_conflict() || entry.is_unfinished() {
        return true;
    }
    let local = target.dir.local.join(&entry.name);
    let Some(modified) = codec::file_mtime(&local) else {
        return false;
    };
    !codec::timestamps_equal(&codec::asctime(modified), &entry.date)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn the_recursion_flags_are_read_and_r_is_the_default_again() {
        let opts = parse(&["-l", "-R", "a.txt"]).unwrap();
        assert!(opts.recursive);
        assert_eq!(opts.names, vec!["a.txt"]);
    }

    #[test]
    fn a_name_then_a_dash_word_is_a_name() {
        let opts = parse(&["a.txt", "-l"]).unwrap();
        assert!(opts.recursive);
        assert_eq!(opts.names, vec!["a.txt", "-l"]);
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let error = parse(&["-c"]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = parse(&["--", "-l"]).unwrap();
        assert_eq!(opts.names, vec!["-l"]);
    }
}
