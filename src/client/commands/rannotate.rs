//! `cvs rannotate`: line-by-line revision history for a module, with no working copy needed.
//!
//! The options go out in the reference's order (`-l`, `-f`, `-r rev`, `-D date`). `-R` is
//! client-side only: it re-enables recursion, which is the default, so it sends nothing.
//! Unlike `rlog`, `-r` and `-D` take their value as a separate `Argument` line.

use crate::client::commands::{date, targets};
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;

#[derive(Debug, Default)]
struct Options {
    /// True after `-l` and false after `-R`; recursion is on by default.
    local: bool,
    force: bool,
    revision: Option<String>,
    date: Option<String>,
    names: Vec<String>,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--" => {
                opts.names.extend(args[i + 1..].iter().cloned());
                break;
            }
            "-l" => opts.local = true,
            "-R" => opts.local = false,
            "-f" => opts.force = true,
            "-r" | "-D" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    Error::aborted("rannotate", format!("option {} requires an argument", arg))
                })?;
                if arg == "-r" {
                    opts.revision = Some(value.clone());
                } else {
                    opts.date = Some(value.clone());
                }
            }
            other if other.starts_with("-r") && other.len() > 2 => {
                opts.revision = Some(other[2..].to_string());
            }
            other if other.starts_with("-D") && other.len() > 2 => {
                opts.date = Some(other[2..].to_string());
            }
            other if other.starts_with('-') => {
                return Err(Error::plain(
                    "rannotate",
                    format!("invalid option -- '{}'", other),
                ))
            }
            other => opts.names.push(other.to_string()),
        }
        i += 1;
    }
    Ok(opts)
}

/// Run `cvs rannotate`.
pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    let opts = parse_args(args)?;

    let mut session = protocol::open(root, "rannotate", cancel, sink)?;
    if opts.local {
        session.argument("-l")?;
    }
    if opts.force {
        session.argument("-f")?;
    }
    if let Some(revision) = &opts.revision {
        session.argument("-r")?;
        session.argument(revision)?;
    }
    if let Some(date) = &opts.date {
        session.argument("-D")?;
        session.argument(&date::normalize(date))?;
    }
    for name in &opts.names {
        session.argument(name)?;
    }
    session.send("rannotate")?;
    session.flush()?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn r_takes_its_value_next_or_attached() {
        let opts = parse(&["-r", "1.1", "mod"]).unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
        assert_eq!(opts.names, vec!["mod".to_string()]);

        let opts = parse(&["-r1.1", "mod"]).unwrap();
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
        assert_eq!(opts.names, vec!["mod".to_string()]);
    }

    #[test]
    fn d_takes_its_value_next_or_attached() {
        let opts = parse(&["-D", "2020-01-01", "mod"]).unwrap();
        assert_eq!(opts.date.as_deref(), Some("2020-01-01"));

        let opts = parse(&["-D2020-01-01", "mod"]).unwrap();
        assert_eq!(opts.date.as_deref(), Some("2020-01-01"));
    }

    #[test]
    fn r_and_d_require_a_value() {
        for option in ["-r", "-D"] {
            let error = parse(&[option]).unwrap_err();
            assert!(
                error
                    .text()
                    .contains(&format!("option {} requires an argument", option)),
                "{}",
                error
            );
        }
    }

    #[test]
    fn r_re_enables_recursion_and_the_last_one_wins() {
        let opts = parse(&["-l", "-R", "mod"]).unwrap();
        assert!(!opts.local);

        let opts = parse(&["-R", "-l", "mod"]).unwrap();
        assert!(opts.local);
    }

    #[test]
    fn an_unknown_option_is_refused_and_a_double_dash_ends_the_options() {
        let error = parse(&["-q", "mod"]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);

        let opts = parse(&["-f", "--", "-mod"]).unwrap();
        assert!(opts.force);
        assert_eq!(opts.names, vec!["-mod".to_string()]);
    }
}
