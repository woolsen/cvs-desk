//! `cvs admin`: pass RCS administration options to the server for the named files.
//!
//! The server runs `rcs admin` itself, so the client's job is to fold each option onto one
//! argument (`-m` and its message become `-m1.1:message`), end the option list with `--`, and
//! name the files. The reference sends the single-value options in a fixed order first, then
//! the repeatable ones in the order they were typed.
//!
//! `-t` is special: `-t-text` sets the description, `-tfile` reads it from a local file, and a
//! bare `-t` reads it from standard input. A description with line breaks travels as
//! `Argument` plus one `Argumentx` per extra line.

use std::io::Read;
use std::path::Path;

use crate::client::cli;
use crate::client::commands::targets;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;
use crate::client::work::codec;

#[derive(Debug, Default)]
struct Options {
    branch: Option<String>,
    comment: Option<String>,
    strict: bool,
    nonstrict: bool,
    delete_revs: Option<String>,
    description: Option<String>,
    interactive: bool,
    quiet: bool,
    kflag: Option<String>,
    /// `-a`, `-A`, `-e`, `-l`, `-u`, `-n`, `-N`, `-m`, `-s`, already folded onto one token.
    extra: Vec<String>,
    warnings: Vec<&'static str>,
    names: Vec<String>,
}

/// The reference's `usage_error` label: the diagnosis already ended, then the advice.
fn usage_error(message: impl Into<String>) -> Error {
    Error::new(format!(
        "{}\ncvs [admin aborted]: specify cvs -H admin for usage information",
        message.into()
    ))
}

fn invalid_option(letter: char) -> Error {
    usage_error(format!("cvs admin: invalid option -- '{}'", letter))
}

fn missing_argument(letter: char) -> Error {
    usage_error(format!(
        "cvs admin: option requires an argument -- '{}'",
        letter
    ))
}

/// getopt's `+` and its `::` optional arguments: a cluster is split letter by letter, and an
/// option that takes a value swallows the rest of its cluster (`-m1.1:x`, `-lq`).
fn expand_clusters(args: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(args.len());
    let mut literal = false;
    for arg in args {
        if literal {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            out.push(arg.clone());
            literal = true;
            continue;
        }
        if arg == "-" || !arg.starts_with('-') {
            // getopt's leading `+` stops at the first name; later `-x` words are names.
            literal = true;
            out.push(arg.clone());
            continue;
        }
        let mut chars = arg[1..].chars();
        while let Some(letter) = chars.next() {
            match letter {
                'i' | 'L' | 'U' | 'I' | 'q' | 'x' => out.push(format!("-{}", letter)),
                'b' | 'e' | 'l' | 'u' | 't' => {
                    let rest: String = chars.collect();
                    out.push(format!("-{}{}", letter, rest));
                    break;
                }
                'c' | 'a' | 'A' | 'n' | 'N' | 'm' | 'o' | 's' | 'V' | 'k' => {
                    let rest: String = chars.collect();
                    out.push(format!("-{}{}", letter, rest));
                    break;
                }
                other => return Err(invalid_option(other)),
            }
        }
    }
    Ok(out)
}

/// The value of one option: the rest of this token when something follows the letter, else the
/// next argument. getopt consumes a following argument even when it starts with `-`.
fn value(args: &[String], i: &mut usize, rest: &str, letter: char) -> Result<String> {
    if !rest.is_empty() {
        return Ok(rest.to_string());
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| missing_argument(letter))
}

const KFLAG_USAGE: &str = "\
cvs admin: invalid RCS keyword expansion mode
Valid expansion modes include:
   -kkv\tGenerate keywords using the default form.
   -kkvl\tLike -kkv, except locker's name inserted.
   -kk\tGenerate only keyword names in keyword strings.
   -kv\tGenerate only keyword values in keyword strings.
   -ko\tGenerate the old keyword string (no changes from checked in file).
   -kb\tGenerate binary file unmodified (merges not allowed) (RCS 5.7).
(Specify the --help global option for a list of other help options)";

/// `RCS_check_kflag`: only these modes pass the client; anything else is its keyword usage
/// text, not a protocol error.
fn check_kflag(argument: &str) -> Result<String> {
    if matches!(argument, "kv" | "kvl" | "k" | "v" | "o" | "b") {
        Ok(argument.to_string())
    } else {
        Err(Error::new(KFLAG_USAGE))
    }
}

/// Decode a description the way the working copy is encoded; text mode also drops the CR of
/// CRLF, as the reference's `fopen (..., "r")` does on Windows.
fn decode_description(encoding: codec::Codec, bytes: &[u8]) -> String {
    encoding.decode(&codec::to_repository_newlines(bytes))
}

/// A description with `-t-` is the text after the dash; `-tfile` reads the file in the working
/// copy's encoding; a bare `-t` reads standard input.
fn description(cwd: &Path, rest: &str) -> Result<String> {
    if let Some(text) = rest.strip_prefix('-') {
        return Ok(text.to_string());
    }
    let encoding = codec::for_dir(cwd);
    let bytes = if rest.is_empty() {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes).map_err(|e| {
            Error::plain(
                "admin",
                format!("cannot read the description from stdin: {}", e),
            )
        })?;
        bytes
    } else {
        let path = if Path::new(rest).is_absolute() {
            Path::new(rest).to_path_buf()
        } else {
            cwd.join(rest)
        };
        std::fs::read(&path).map_err(|e| {
            Error::plain(
                "admin",
                format!("cannot read the description file {}: {}", rest, e),
            )
        })?
    };
    Ok(decode_description(encoding, &bytes))
}

fn parse_args(cwd: &Path, args: &[String]) -> Result<Options> {
    let expanded = expand_clusters(args)?;
    let args = expanded.as_slice();
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            opts.names.extend(args[i + 1..].iter().cloned());
            break;
        }
        // getopt's leading `+`: the first name ends the option list, so a later `-kb` is a
        // file name and not an option.
        if arg.len() < 2 || !arg.starts_with('-') {
            opts.names.extend(args[i..].iter().cloned());
            break;
        }
        let mut chars = arg[1..].chars();
        let letter = chars.next().expect("the argument has a letter");
        let rest = chars.as_str();
        match letter {
            'i' => {
                return Err(usage_error(
                    "cvs admin: the -i option to admin is not supported\n\
                     cvs admin: run add or import to create an RCS file",
                ))
            }
            'L' => {
                if opts.nonstrict {
                    return Err(usage_error("cvs admin: -U and -L are incompatible"));
                }
                opts.strict = true;
            }
            'U' => {
                if opts.strict {
                    return Err(usage_error("cvs admin: -U and -L are incompatible"));
                }
                opts.nonstrict = true;
            }
            'I' => opts.interactive = true,
            'q' => opts.quiet = true,
            'b' => {
                if opts.branch.is_some() {
                    return Err(usage_error("cvs admin: duplicate 'b' option"));
                }
                opts.branch = Some(rest.to_string());
            }
            'c' => {
                if opts.comment.is_some() {
                    return Err(usage_error("cvs admin: duplicate 'c' option"));
                }
                opts.comment = Some(value(args, &mut i, rest, letter)?);
            }
            'k' => {
                if opts.kflag.is_some() {
                    return Err(usage_error("cvs admin: duplicate '-k' option"));
                }
                opts.kflag = Some(check_kflag(&value(args, &mut i, rest, letter)?)?);
            }
            'o' => {
                if opts.delete_revs.is_some() {
                    return Err(usage_error("cvs admin: duplicate '-o' option"));
                }
                opts.delete_revs = Some(value(args, &mut i, rest, letter)?);
            }
            't' => {
                if opts.description.is_some() {
                    return Err(usage_error("cvs admin: duplicate 't' option"));
                }
                opts.description = Some(description(cwd, rest)?);
            }
            'a' | 'A' | 'e' | 'l' | 'u' | 'n' | 'N' | 'm' | 's' => {
                // `-e`, `-l` and `-u` take their value only when it is attached.
                let attached_only = matches!(letter, 'e' | 'l' | 'u');
                let argument = if attached_only {
                    rest.to_string()
                } else {
                    value(args, &mut i, rest, letter)?
                };
                opts.extra.push(format!("-{}{}", letter, argument));
            }
            'V' => {
                // `V:` is still a required argument; only the version is ignored.
                let _ = value(args, &mut i, rest, letter)?;
                opts.warnings.push("the `-V' option is obsolete");
            }
            'x' => {
                return Err(usage_error(
                    "cvs admin: the -x option has never done anything useful\n\
                     cvs admin: RCS files in CVS always end in ,v",
                ))
            }
            other => return Err(invalid_option(other)),
        }
        i += 1;
    }
    Ok(opts)
}

/// `check_numeric`: a numeric revision only makes sense for one name, and not for a folder
/// that stands for more than one file.
fn check_numeric(revision: &str, names: &[String], cwd: &Path) -> Result<()> {
    if !revision
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit())
    {
        return Ok(());
    }
    if names.len() != 1 || cwd.join(&names[0]).is_dir() {
        return Err(Error::new(
            "cvs admin: while processing more than one file:\n\
             cvs [admin aborted]: attempt to specify a numeric revision",
        ));
    }
    Ok(())
}

/// The reference's numeric checks: `-b`, `-o` (both sides of its colon), and each `-m`, `-l`
/// and `-u` value.
fn check_revisions(opts: &Options, cwd: &Path) -> Result<()> {
    for option in &opts.extra {
        if option
            .as_bytes()
            .get(1)
            .is_some_and(|&letter| matches!(letter, b'm' | b'l' | b'u'))
        {
            check_numeric(&option[2..], &opts.names, cwd)?;
        }
    }
    if let Some(branch) = &opts.branch {
        check_numeric(branch, &opts.names, cwd)?;
    }
    if let Some(revisions) = &opts.delete_revs {
        check_numeric(revisions, &opts.names, cwd)?;
        if let Some(colon) = revisions.find(':') {
            let after = &revisions[colon + 1..];
            let bytes = after.as_bytes();
            if bytes.first().is_some_and(|byte| byte.is_ascii_digit()) {
                check_numeric(after, &opts.names, cwd)?;
            } else if bytes.first() == Some(&b':')
                && bytes.get(1).is_some_and(|byte| byte.is_ascii_digit())
            {
                check_numeric(&after[1..], &opts.names, cwd)?;
            }
        }
    }
    Ok(())
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
    check_revisions(&opts, cwd)?;
    if opts.interactive {
        return Err(Error::aborted(
            "admin",
            "-I option not useful with client/server",
        ));
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

    // `-Q` sets really_quiet, and admin forwards either quiet flag as `-q` (admin.c:499-503).
    let really_quiet = root.options.quiet >= 2;
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
    if opts.quiet || really_quiet {
        session.argument("-q")?;
    }
    if let Some(kflag) = &opts.kflag {
        session.argument(&format!("-k{}", kflag))?;
    }
    for option in &opts.extra {
        session.argument(option)?;
    }
    // `Argument --`, then `Max-dotdot`, then the folder blocks; the names and the command go
    // last (admin.c:510-513, client.c:5606-5616).
    targets::begin_files(&mut session, &opts.names)?;

    let last = targets::send_blocks_watch(&mut session, &targets)?;
    targets::finish_from_last(&mut session, cwd, &opts.names, "admin", last.as_deref())?;
    let _ = targets::read_text(&mut session, false)?;
    session.close();
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn clusters_split_like_getopt() {
        let opts = parse_args(Path::new("."), &args(&["-qL", "f"])).unwrap();
        assert!(opts.quiet);
        assert!(opts.strict);
        assert_eq!(opts.names, vec!["f".to_string()]);

        let opts = parse_args(Path::new("."), &args(&["-Lu1.2", "f"])).unwrap();
        assert!(opts.strict);
        assert_eq!(opts.extra, vec!["-u1.2".to_string()]);

        // An optional argument swallows the rest of its cluster, so this is `-l q`.
        let opts = parse_args(Path::new("."), &args(&["-lq", "f"])).unwrap();
        assert_eq!(opts.extra, vec!["-lq".to_string()]);
    }

    #[test]
    fn a_glued_version_does_not_swallow_the_next_option() {
        let opts = parse_args(Path::new("."), &args(&["-V1.1", "-kb", "f"])).unwrap();
        assert_eq!(opts.warnings, ["the `-V' option is obsolete"]);
        assert_eq!(opts.kflag.as_deref(), Some("b"));
        assert_eq!(opts.names, vec!["f".to_string()]);
    }

    #[test]
    fn a_bare_version_needs_its_argument() {
        let error = parse_args(Path::new("."), &args(&["-V"])).unwrap_err();
        assert!(
            error.text().contains("option requires an argument -- 'V'"),
            "{}",
            error
        );
    }

    #[test]
    fn the_first_name_ends_the_option_list() {
        let opts = parse_args(Path::new("."), &args(&["f", "-kb", "-Z"])).unwrap();
        assert!(opts.kflag.is_none());
        assert_eq!(
            opts.names,
            vec!["f".to_string(), "-kb".to_string(), "-Z".to_string()]
        );
    }

    #[test]
    fn a_double_dash_makes_everything_after_it_a_name() {
        let opts = parse_args(Path::new("."), &args(&["-q", "--", "-kb"])).unwrap();
        assert!(opts.quiet);
        assert_eq!(opts.names, vec!["-kb".to_string()]);
    }

    #[test]
    fn unsupported_rcs_options_get_their_own_diagnosis() {
        let error = parse_args(Path::new("."), &args(&["-i", "f"])).unwrap_err();
        assert!(
            error
                .text()
                .contains("the -i option to admin is not supported"),
            "{}",
            error
        );
        assert!(error.text().contains("run add or import"), "{}", error);

        let error = parse_args(Path::new("."), &args(&["-x", "f"])).unwrap_err();
        assert!(
            error
                .text()
                .contains("the -x option has never done anything useful"),
            "{}",
            error
        );
    }

    #[test]
    fn only_the_reference_keyword_modes_are_accepted() {
        let opts = parse_args(Path::new("."), &args(&["-kkv", "f"])).unwrap();
        assert_eq!(opts.kflag.as_deref(), Some("kv"));

        let error = parse_args(Path::new("."), &args(&["-k", "u", "f"])).unwrap_err();
        assert!(
            error.text().contains("invalid RCS keyword expansion mode"),
            "{}",
            error
        );
    }

    #[test]
    fn a_description_is_decoded_with_the_working_copy_codec() {
        // 中文 in GBK, followed by the CRLF a Windows text file would carry.
        let bytes = [0xd6, 0xd0, 0xce, 0xc4, b'\r', b'\n'];
        assert_eq!(decode_description(codec::Codec::Gbk, &bytes), "中文\n");
        assert_eq!(
            decode_description(codec::Codec::Utf8, b"plain\r\ntext"),
            "plain\ntext"
        );
    }

    #[test]
    fn a_numeric_revision_needs_one_file() {
        let error = check_numeric(
            "1.2",
            &["a.txt".to_string(), "b.txt".to_string()],
            Path::new("."),
        )
        .unwrap_err();
        assert!(
            error.text().contains("while processing more than one file"),
            "{}",
            error
        );
        assert!(
            error
                .text()
                .contains("attempt to specify a numeric revision"),
            "{}",
            error
        );

        assert!(check_numeric("1.2", &["a.txt".to_string()], Path::new(".")).is_ok());
        assert!(check_numeric(
            "TAG",
            &["a.txt".to_string(), "b.txt".to_string()],
            Path::new(".")
        )
        .is_ok());
    }

    #[test]
    fn a_numeric_revision_does_not_stand_for_a_folder() {
        let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
        let error = check_numeric("1.2", &["src".to_string()], cwd).unwrap_err();
        assert!(
            error
                .text()
                .contains("attempt to specify a numeric revision"),
            "{}",
            error
        );
    }
}
