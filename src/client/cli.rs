//! The `cvs` command line: global options, command dispatch, and the help text.
//!
//! Every command in this build's list has an implementation. A name that is not in the list
//! is reported as an unknown command, as the reference does.

use std::io::{self, Write};
use std::path::Path;

use crate::client::engine::{self, CancelToken, Request, Sink};
use crate::client::error::{Error, Result};

pub const VERSION: &str = "1.11.1.3";
pub const CLIENT_NAME: &str = "cvsdesk";

/// What the program prints for `--version`.
pub fn banner() -> String {
    format!(
        "Concurrent Versions System (CVS) {} (client/server)\n{} {} compatible client",
        VERSION,
        CLIENT_NAME,
        env!("CARGO_PKG_VERSION")
    )
}

/// One accepted command line.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Version,
    Help,
    HelpCommands,
    HelpOptions,
    HelpSynonyms,
    HelpCommand(String),
    Run { name: String, args: Vec<String> },
    None,
}

/// The global options that come before the command name.
#[derive(Debug)]
pub struct Options {
    pub(crate) root: Option<String>,
    pub(crate) quiet: u32,
    pub(crate) no_execute: bool,
    pub(crate) readonly: bool,
    pub(crate) trace: bool,
    pub(crate) compression: u32,
    /// `-e`: the editor for log messages.
    pub(crate) editor: Option<String>,
    pub(crate) use_cvsrc: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            root: None,
            quiet: 0,
            no_execute: false,
            readonly: false,
            trace: false,
            compression: 0,
            editor: None,
            use_cvsrc: true,
        }
    }
}

/// Parse the arguments after the program name. Shared with [`crate::client::engine`].
///
/// `~/.cvsrc` is read first: its `cvs` line goes in front of the typed global options, and
/// the line for the command in front of the typed command arguments, as the reference does.
pub fn parse_args(args: &[String]) -> Result<(Options, Invocation)> {
    parse_args_with(args, crate::client::cvsrc::Cvsrc::read().as_ref())
}

/// The parser with the `~/.cvsrc` contents already decided, so tests need no home folder.
fn parse_args_with(
    args: &[String],
    cvsrc: Option<&crate::client::cvsrc::Cvsrc>,
) -> Result<(Options, Invocation)> {
    let (opts, invocation) = parse_without_cvsrc(args)?;
    let Some(file) = cvsrc else {
        return Ok((opts, invocation));
    };
    if !opts.use_cvsrc {
        return Ok((opts, invocation));
    }

    let mut with_globals = Vec::with_capacity(args.len() + 8);
    if let Some(options) = file.options_for("cvs") {
        with_globals.extend(options.iter().cloned());
    }
    with_globals.extend(args.iter().cloned());
    let (opts, invocation) = parse_without_cvsrc(&with_globals)?;
    // A `-f` in the file's own `cvs` line turns the command line off too.
    if !opts.use_cvsrc {
        return Ok((opts, invocation));
    }

    let (name, args) = match invocation {
        Invocation::Run { name, args } => (name, args),
        other => return Ok((opts, other)),
    };
    let Some(options) = file.options_for(&canonical(&name)) else {
        return Ok((opts, Invocation::Run { name, args }));
    };
    let mut combined = options.to_vec();
    combined.extend(args);
    Ok((
        opts,
        Invocation::Run {
            name,
            args: combined,
        },
    ))
}

fn parse_without_cvsrc(args: &[String]) -> Result<(Options, Invocation)> {
    // getopt accepts clustered short options (`-Qq`, `-fz9`). This client's global parser
    // matches whole words, so clusters are split before parsing. Command options keep their
    // text: their parsers split their own clusters.
    let expanded = expand_global_clusters(args);
    let args = expanded.as_slice();
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--version" | "-v" => return Ok((opts, Invocation::Version)),
            "--help" => return Ok((opts, Invocation::Help)),
            "--help-commands" => return Ok((opts, Invocation::HelpCommands)),
            "--help-options" => return Ok((opts, Invocation::HelpOptions)),
            "--help-synonyms" => return Ok((opts, Invocation::HelpSynonyms)),
            "-H" => {
                let command = args.get(i + 1).cloned().unwrap_or_default();
                return Ok((opts, Invocation::HelpCommand(command)));
            }
            "--" => {
                if let Some(name) = args.get(i + 1) {
                    return Ok((opts, command_invocation(name, &args[i + 2..])));
                }
                return Ok((opts, Invocation::None));
            }
            "-d" => {
                i += 1;
                opts.root = Some(require_arg(args, i, "-d")?);
            }
            "-q" => opts.quiet = opts.quiet.max(1),
            "-Q" => opts.quiet = 2,
            "-f" => opts.use_cvsrc = false,
            "-n" => opts.no_execute = true,
            "-r" => opts.readonly = true,
            "-w" => opts.readonly = false,
            "-t" => opts.trace = true,
            // The obsolete RCS directory option: accepted and ignored, as the reference does.
            "-b" => {}
            "-z" => {
                i += 1;
                opts.compression = parse_compression(&require_arg(args, i, "-z")?)?;
            }
            "-e" => {
                i += 1;
                opts.editor = Some(require_arg(args, i, "-e")?);
            }
            "-T" | "-s" => {
                // Accepted for command-line compatibility; they affect temporary files and
                // user variables, neither of which this client uses yet.
                i += 1;
                require_arg(args, i, arg)?;
            }
            "-x" | "-a" => {}
            other if other.starts_with("-d") && other.len() > 2 => {
                opts.root = Some(other[2..].to_string());
            }
            other if other.starts_with("-z") && other.len() > 2 => {
                opts.compression = parse_compression(&other[2..])?;
            }
            other if other.starts_with("-e") && other.len() > 2 => {
                opts.editor = Some(other[2..].to_string());
            }
            other if other.starts_with("-H") && other.len() > 2 => {
                return Ok((opts, Invocation::HelpCommand(other[2..].to_string())));
            }
            other if other.starts_with('-') => {
                return Err(Error::new(format!(
                    "cvs: invalid option -- '{}'",
                    other.trim_start_matches('-')
                )));
            }
            other => return Ok((opts, command_invocation(other, &args[i + 1..]))),
        }
        i += 1;
    }
    Ok((opts, Invocation::None))
}

/// Split global short-option clusters the way getopt does: `-Qq` becomes `-Q -q`, and
/// `-fz9` becomes `-f -z9`. An argument containing an unknown letter is left untouched, so
/// `-Pd` reaches the command parser as written.
fn expand_global_clusters(args: &[String]) -> Vec<String> {
    const NO_ARG: &str = "Qqrwtnvbxaf";
    const WITH_ARG: &str = "dzeTsH";

    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        let bytes = arg.as_bytes();
        if bytes.len() < 3 || bytes[0] != b'-' || bytes[1] == b'-' {
            out.push(arg.clone());
            continue;
        }
        let text = &arg[1..];
        // Only pure ASCII flags can cluster; anything else is a name or a value.
        if !text.is_ascii() {
            out.push(arg.clone());
            continue;
        }
        let mut parts = Vec::new();
        let mut rest = text;
        let mut valid = true;
        while let Some(ch) = rest.chars().next() {
            let flag = &rest[..ch.len_utf8()];
            rest = &rest[ch.len_utf8()..];
            if NO_ARG.contains(ch) {
                parts.push(format!("-{flag}"));
            } else if WITH_ARG.contains(ch) {
                parts.push(format!("-{flag}{rest}"));
                rest = "";
            } else {
                valid = false;
                break;
            }
        }
        if valid {
            out.extend(parts);
        } else {
            out.push(arg.clone());
        }
    }
    out
}

fn command_invocation(name: &str, rest: &[String]) -> Invocation {
    Invocation::Run {
        name: name.to_string(),
        args: rest.to_vec(),
    }
}

fn require_arg(args: &[String], i: usize, option: &str) -> Result<String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| Error::new(format!("cvs: option requires an argument -- '{}'", option)))
}

fn parse_compression(text: &str) -> Result<u32> {
    // The reference refuses anything outside 0..=9, before it looks for a repository.
    match text.parse::<u32>() {
        Ok(level) if level <= 9 => Ok(level),
        _ => Err(Error::new(
            "cvs: gzip compression level must be between 0 and 9",
        )),
    }
}

/// Where the program's own output goes: the console, one line at a time.
pub struct StdoutSink;

impl Sink for StdoutSink {
    fn out(&mut self, line: String) {
        println!("{}", line);
    }

    fn err(&mut self, line: String) {
        eprintln!("{}", line);
    }

    fn prompt(&mut self, text: String) {
        print!("{}", text);
        let _ = io::stdout().flush();
    }

    fn ask(&mut self, text: String) -> Option<String> {
        self.prompt(text);
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
            Err(_) => None,
        }
    }

    fn secret(&mut self, text: String) -> Option<String> {
        // An empty answer, including a closed input, is still a password to try: the
        // reference lets the server refuse it instead of skipping the connection.
        Some(self.ask(text).unwrap_or_default())
    }

    fn interactive(&self) -> bool {
        true
    }
}

/// The program entry point. Returns the exit code, never panics on a bad command line.
pub fn run() -> i32 {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let mut sink = StdoutSink;
    let request = Request {
        cwd,
        args,
        password: None,
    };
    match engine::run(request, &mut sink, &CancelToken::default()) {
        Ok(code) => code,
        Err(error) => {
            sink.err(error.to_string());
            1
        }
    }
}

pub(crate) fn canonical(name: &str) -> String {
    match name {
        "co" | "get" => "checkout",
        "adm" | "rcs" => "admin",
        "ci" | "com" => "commit",
        "up" | "upd" => "update",
        "st" | "stat" => "status",
        "ad" | "new" => "add",
        "rm" | "delete" => "remove",
        "di" | "dif" => "diff",
        "exp" | "ex" => "export",
        "hist" | "hi" | "his" => "history",
        "im" | "imp" => "import",
        "rl" => "rlog",
        "lo" => "log",
        "rann" | "ra" => "rannotate",
        "ann" | "blame" => "annotate",
        "rt" | "rfreeze" => "rtag",
        "patch" | "pa" => "rdiff",
        "re" | "rel" => "release",
        "ta" | "freeze" => "tag",
        "logon" | "lgn" => "login",
        "ve" | "ver" => "version",
        other => other,
    }
    .to_string()
}

struct CommandInfo {
    /// The command's own name, as `--help-commands` shows it.
    names: &'static str,
    /// Other names, space separated; empty when the command has only one.
    aliases: &'static str,
    summary: &'static str,
    usage: &'static str,
}

const COMMANDS: &[CommandInfo] = &[
    CommandInfo {
        names: "add",
        aliases: "ad new",
        summary: "Add a new file/directory to the repository",
        usage: "Usage: cvs add [-k rcs-kflag] [-m message] files...\n\
                \t-k rcs-kflag\tUse \"rcs-kflag\" to add the file with the specified\n\
                \t\t\tkflag.\n\
                \t-m message\tUse \"message\" for the creation log.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "admin",
        aliases: "adm rcs",
        summary: "Administration front end for rcs",
        usage: "Usage: cvs admin [options] files...\n\
                \t-a users   Append (comma-separated) user names to access list.\n\
                \t-A file    Append another file's access list.\n\
                \t-b[rev]    Set default branch (highest branch on trunk if omitted).\n\
                \t-c string  Set comment leader.\n\
                \t-e[users]  Remove (comma-separated) user names from access list\n\
                \t           (all names if omitted).\n\
                \t-I         Run interactively.\n\
                \t-k subst   Set keyword substitution mode:\n\
                \t   kv   (Default) Substitute keyword and value.\n\
                \t   kvl  Substitute keyword, value, and locker (if any).\n\
                \t   k    Substitute keyword only.\n\
                \t   o    Preserve original string.\n\
                \t   b    Like o, but mark file as binary.\n\
                \t   v    Substitute value only.\n\
                \t-l[rev]    Lock revision (latest revision on branch,\n\
                \t           latest revision on trunk if omitted).\n\
                \t-L         Set strict locking.\n\
                \t-m rev:msg  Replace revision's log message.\n\
                \t-n tag[:[rev]]  Tag branch or revision.  If :rev is omitted,\n\
                \t                delete the tag; if rev is omitted, tag the latest\n\
                \t                revision on the default branch.\n\
                \t-N tag[:[rev]]  Same as -n except override existing tag.\n\
                \t-o range   Delete (outdate) specified range of revisions:\n\
                \t   rev1:rev2   Between rev1 and rev2, including rev1 and rev2.\n\
                \t   rev1::rev2  Between rev1 and rev2, excluding rev1 and rev2.\n\
                \t   rev:        rev and following revisions on the same branch.\n\
                \t   rev::       After rev on the same branch.\n\
                \t   :rev        rev and previous revisions on the same branch.\n\
                \t   ::rev       Before rev on the same branch.\n\
                \t   rev         Just rev.\n\
                \t-q         Run quietly.\n\
                \t-s state[:rev]  Set revision state (latest revision on branch,\n\
                \t                latest revision on trunk if omitted).\n\
                \t-t[file]   Get descriptive text from file (stdin if omitted).\n\
                \t-t-string  Set descriptive text.\n\
                \t-u[rev]    Unlock the revision (latest revision on branch,\n\
                \t           latest revision on trunk if omitted).\n\
                \t-U         Unset strict locking.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "annotate",
        aliases: "ann blame",
        summary: "Show last revision where each line was modified",
        usage: "Usage: cvs annotate [-lRfF] [-r rev] [-D date] [files...]\n\
                \t-l\tLocal directory only, no recursion.\n\
                \t-R\tProcess directories recursively.\n\
                \t-f\tUse head revision if tag/date not found.\n\
                \t-F\tAnnotate binary files.\n\
                \t-r rev\tAnnotate file as of specified revision/tag.\n\
                \t-D date\tAnnotate file as of specified date.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "checkout",
        aliases: "co get",
        summary: "Checkout sources for editing",
        usage: "Usage:\n\
                \x20\x20cvs checkout [-ANPRcflnps] [-r rev] [-D date] [-d dir]\n\
                \x20\x20\x20\x20[-j rev1] [-j rev2] [-k kopt] modules...\n\
                \t-A\tReset any sticky tags/date/kopts.\n\
                \t-N\tDon't shorten module paths if -d specified.\n\
                \t-P\tPrune empty directories.\n\
                \t-R\tProcess directories recursively.\n\
                \t-c\t\"cat\" the module database.\n\
                \t-f\tForce a head revision match if tag/date not found.\n\
                \t-l\tLocal directory only, not recursive\n\
                \t-n\tDo not run module program (if any).\n\
                \t-p\tCheck out files to standard output (avoids stickiness).\n\
                \t-s\tLike -c, but include module status.\n\
                \t-r rev\tCheck out revision or tag. (implies -P) (is sticky)\n\
                \t-D date\tCheck out revisions as of date. (implies -P) (is sticky)\n\
                \t-d dir\tCheck out into dir instead of module name.\n\
                \t-k kopt\tUse RCS kopt -k option on checkout. (is sticky)\n\
                \t-j rev\tMerge in changes made between current revision and rev.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "commit",
        aliases: "ci com",
        summary: "Check files into the repository",
        usage: "Usage: cvs commit [-Rlf] [-m msg | -F logfile] [-r rev] files...\n\
                \x20\x20\x20\x20-R          Process directories recursively.\n\
                \x20\x20\x20\x20-l          Local directory only (not recursive).\n\
                \x20\x20\x20\x20-f          Force the file to be committed; disables recursion.\n\
                \x20\x20\x20\x20-F logfile  Read the log message from file.\n\
                \x20\x20\x20\x20-m msg      Log message.\n\
                \x20\x20\x20\x20-r rev      Commit to this branch or trunk revision.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "diff",
        aliases: "",
        summary: "Show differences between revisions",
        usage: "Usage: cvs diff [-lR] [-k kopt] [format_options]\n\
                \x20\x20\x20\x20[[-r rev1 | -D date1] [-r rev2 | -D date2]] [files...]\x20\n\
                \t-l\tLocal directory only, not recursive\n\
                \t-R\tProcess directories recursively.\n\
                \t-k kopt\tSpecify keyword expansion mode.\n\
                \t-D d1\tDiff revision for date against working file.\n\
                \t-D d2\tDiff rev1/date1 against date2.\n\
                \t-r rev1\tDiff revision for rev1 against working file.\n\
                \t-r rev2\tDiff rev1/date1 against rev2.\n\
                \n\
                format_options:\n\
                \x20\x20-i  --ignore-case  Consider upper- and lower-case to be the same.\n\
                \x20\x20-w  --ignore-all-space  Ignore all white space.\n\
                \x20\x20-b  --ignore-space-change  Ignore changes in the amount of white space.\n\
                \x20\x20-B  --ignore-blank-lines  Ignore changes whose lines are all blank.\n\
                \x20\x20-I RE  --ignore-matching-lines=RE  Ignore changes whose lines all match RE.\n\
                \x20\x20--binary  Read and write data in binary mode.\n\
                \x20\x20-a  --text  Treat all files as text.\n\
                \n\
                \x20\x20-c  -C NUM  --context[=NUM]  Output NUM (default 2) lines of copied context.\n\
                \x20\x20-u  -U NUM  --unified[=NUM]  Output NUM (default 2) lines of unified context.\n\
                \x20\x20\x20\x20-NUM  Use NUM context lines.\n\
                \x20\x20\x20\x20-L LABEL  --label LABEL  Use LABEL instead of file name.\n\
                \x20\x20\x20\x20-p  --show-c-function  Show which C function each change is in.\n\
                \x20\x20\x20\x20-F RE  --show-function-line=RE  Show the most recent line matching RE.\n\
                \x20\x20--brief  Output only whether files differ.\n\
                \x20\x20-e  --ed  Output an ed script.\n\
                \x20\x20-f  --forward-ed  Output something like an ed script in forward order.\n\
                \x20\x20-n  --rcs  Output an RCS format diff.\n\
                \x20\x20-y  --side-by-side  Output in two columns.\n\
                \x20\x20\x20\x20-W NUM  --width=NUM  Output at most NUM (default 130) characters per line.\n\
                \x20\x20\x20\x20--left-column  Output only the left column of common lines.\n\
                \x20\x20\x20\x20--suppress-common-lines  Do not output common lines.\n\
                \x20\x20--ifdef=NAME  Output merged file to show `#ifdef NAME' diffs.\n\
                \x20\x20--GTYPE-group-format=GFMT  Similar, but format GTYPE input groups with GFMT.\n\
                \x20\x20--line-format=LFMT  Similar, but format all input lines with LFMT.\n\
                \x20\x20--LTYPE-line-format=LFMT  Similar, but format LTYPE input lines with LFMT.\n\
                \x20\x20\x20\x20LTYPE is `old', `new', or `unchanged'.  GTYPE is LTYPE or `changed'.\n\
                \x20\x20\x20\x20GFMT may contain:\n\
                \x20\x20\x20\x20\x20\x20%<  lines from FILE1\n\
                \x20\x20\x20\x20\x20\x20%>  lines from FILE2\n\
                \x20\x20\x20\x20\x20\x20%=  lines common to FILE1 and FILE2\n\
                \x20\x20\x20\x20\x20\x20%[-][WIDTH][.[PREC]]{doxX}LETTER  printf-style spec for LETTER\n\
                \x20\x20\x20\x20\x20\x20\x20\x20LETTERs are as follows for new group, lower case for old group:\n\
                \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20F  first line number\n\
                \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20L  last line number\n\
                \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20N  number of lines = L-F+1\n\
                \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20E  F-1\n\
                \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20M  L+1\n\
                \x20\x20\x20\x20LFMT may contain:\n\
                \x20\x20\x20\x20\x20\x20%L  contents of line\n\
                \x20\x20\x20\x20\x20\x20%l  contents of line, excluding any trailing newline\n\
                \x20\x20\x20\x20\x20\x20%[-][WIDTH][.[PREC]]{doxX}n  printf-style spec for input line number\n\
                \x20\x20\x20\x20Either GFMT or LFMT may contain:\n\
                \x20\x20\x20\x20\x20\x20%%  %\n\
                \x20\x20\x20\x20\x20\x20%c'C'  the single character C\n\
                \x20\x20\x20\x20\x20\x20%c'\\OOO'  the character with octal code OOO\n\
                \n\
                \x20\x20-t  --expand-tabs  Expand tabs to spaces in output.\n\
                \x20\x20-T  --initial-tab  Make tabs line up by prepending a tab.\n\
                \n\
                \x20\x20-N  --new-file  Treat absent files as empty.\n\
                \x20\x20-s  --report-identical-files  Report when two files are the same.\n\
                \x20\x20--horizon-lines=NUM  Keep NUM lines of the common prefix and suffix.\n\
                \x20\x20-d  --minimal  Try hard to find a smaller set of changes.\n\
                \x20\x20-H  --speed-large-files  Assume large files and many scattered small changes.\n\
                \n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "edit",
        aliases: "",
        summary: "Get ready to edit a watched file",
        usage: "Usage: cvs edit [-lR] [-a <action>]... [<file>]...\n\
                -l\tLocal directory only, not recursive.\n\
                -R\tProcess directories recursively (default).\n\
                -a\tSpecify action to register for temporary watch, one of:\n\
                \x20\x20\t`edit', `unedit', `commit', `all', or `none' (defaults to `all').\n\
                (Specify the --help global option for a list of other help options.)",
    },
    CommandInfo {
        names: "editors",
        aliases: "",
        summary: "See who is editing a watched file",
        usage: "Usage: cvs editors [-lR] [<file>]...\n\
                -l\tProcess this directory only (not recursive).\n\
                -R\tProcess directories recursively (default).\n\
                (Specify the --help global option for a list of other help options.)",
    },
    CommandInfo {
        names: "export",
        aliases: "exp ex",
        summary: "Export sources from CVS, similar to checkout",
        usage: "Usage: cvs export [-NRfln] [-r tag] [-D date] [-d dir] [-k kopt] module...\n\
                \t-N\tDon't shorten module paths if -d specified.\n\
                \t-f\tForce a head revision match if tag/date not found.\n\
                \t-l\tLocal directory only, not recursive\n\
                \t-R\tProcess directories recursively (default).\n\
                \t-n\tDo not run module program (if any).\n\
                \t-r tag\tExport tagged revisions.\n\
                \t-D date\tExport revisions as of date.\n\
                \t-d dir\tExport into dir instead of module name.\n\
                \t-k kopt\tUse RCS kopt -k option on checkout.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "history",
        aliases: "hi his",
        summary: "Show repository access history",
        usage: "Usage: cvs history [-report] [-flags] [-options args] [files...]\n\
                \n\
                \x20\x20\x20Reports:\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-T              Produce report on all TAGs\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-c              Committed (Modified) files\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-o              Checked out modules\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-m <module>     Look for specified module (repeatable)\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-x [TOEFWUPCGMAR] Extract by record type\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-e              Everything (same as -x, but all record types)\n\
                \x20\x20\x20Flags:\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-a              All users (Default is self)\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-l              Last modified (committed or modified report)\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-w              Working directory must match\n\
                \x20\x20\x20Options:\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-D <date>       Since date (Many formats)\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-b <str>        Back to record with str in module/file/repos field\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-f <file>       Specified file (same as command line) (repeatable)\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-n <modulename> In module (repeatable)\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-p <repos>      In repository (repeatable)\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-r <rev/tag>    Since rev or tag (looks inside RCS files!)\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-t <tag>        Since tag record placed in history file (by anyone).\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-u <user>       For user name (repeatable)\n\
                \x20\x20\x20\x20\x20\x20\x20\x20-z <tz>         Output for time zone <tz> (e.g. -z -0700)",
    },
    CommandInfo {
        names: "import",
        aliases: "im imp",
        summary: "Import sources into CVS, using vendor branches",
        usage: "Usage: cvs import [-d] [-k subst] [-I ign] [-m msg] [-b branch]\n\
                \x20\x20\x20\x20[-W spec] repository vendor-tag release-tags...\n\
                \t-d\tUse the file's modification time as the time of import.\n\
                \t-k sub\tSet default RCS keyword substitution mode.\n\
                \t-I ign\tMore files to ignore (! to reset).\n\
                \t-b bra\tVendor branch id.\n\
                \t-m msg\tLog message.\n\
                \t-W spec\tWrappers specification line.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "init",
        aliases: "",
        summary: "Create a CVS repository if it doesn't exist",
        usage: "Usage: cvs init\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "log",
        aliases: "lo",
        summary: "Print out history information for files",
        usage: "Usage: cvs log [-lRhtNb] [-r[revisions]] [-d dates] [-s states]\n\
                \x20\x20\x20\x20[-w[logins]] [files...]\n\
                \t-l\tLocal directory only, no recursion.\n\
                \t-b\tList revisions on the default branch.\n\
                \t-h\tOnly print header.\n\
                \t-R\tOnly print name of RCS file.\n\
                \t-t\tOnly print header and descriptive text.\n\
                \t-N\tDo not list tags.\n\
                \t-n\tList tags (default).\n\
                \t-S\tDo not print name/header if no revisions selected.  -d, -r,\n\
                \t\t-s, & -w have little effect in conjunction with -b, -h, -R, and\n\
                \t\t-t without this option.\n\
                \t-r[revisions]\tA comma-separated list of revisions to print:\n\
                \t   rev1:rev2   Between rev1 and rev2, including rev1 and rev2.\n\
                \t   rev1::rev2  Between rev1 and rev2, excluding rev1.\n\
                \t   rev:        rev and following revisions on the same branch.\n\
                \t   rev::       After rev on the same branch.\n\
                \t   :rev        rev and previous revisions on the same branch.\n\
                \t   ::rev       rev and previous revisions on the same branch.\n\
                \t   rev         Just rev.\n\
                \t   branch      All revisions on the branch.\n\
                \t   branch.     The last revision on the branch.\n\
                \t-d dates\tA semicolon-separated list of dates\n\
                \t        \t(D1<D2 for range, D for latest before).\n\
                \t-s states\tOnly list revisions with specified states.\n\
                \t-w[logins]\tOnly list revisions checked in by specified logins.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "login",
        aliases: "logon lgn",
        summary: "Prompt for password for authenticating server",
        usage: "Usage: cvs login\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "logout",
        aliases: "",
        summary: "Removes entry in .cvspass for remote repository",
        usage: "Usage: cvs logout\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "rannotate",
        aliases: "rann ra",
        summary: "Show last revision where each line of module was modified",
        usage: "Usage: cvs rannotate [-lRfF] [-r rev] [-D date] [files...]\n\
                \t-l\tLocal directory only, no recursion.\n\
                \t-R\tProcess directories recursively.\n\
                \t-f\tUse head revision if tag/date not found.\n\
                \t-F\tAnnotate binary files.\n\
                \t-r rev\tAnnotate file as of specified revision/tag.\n\
                \t-D date\tAnnotate file as of specified date.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "rdiff",
        aliases: "patch pa",
        summary: "Create 'patch' format diffs between releases",
        usage: "Usage: cvs rdiff [-flR] [-c|-u] [-s|-t] [-V %d] [-k kopt]\n\
                \x20\x20\x20\x20-r rev|-D date [-r rev2 | -D date2] modules...\n\
                \t-f\tForce a head revision match if tag/date not found.\n\
                \t-l\tLocal directory only, not recursive\n\
                \t-R\tProcess directories recursively.\n\
                \t-c\tContext diffs (default)\n\
                \t-u\tUnidiff format.\n\
                \t-s\tShort patch - one liner per file.\n\
                \t-t\tTop two diffs - last change made to the file.\n\
                \t-V vers\tUse RCS Version \"vers\" for keyword expansion.\n\
                \t-k kopt\tSpecify keyword expansion mode.\n\
                \t-D date\tDate.\n\
                \t-r rev\tRevision - symbolic or numeric.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "release",
        aliases: "re rel",
        summary: "Indicate that a Module is no longer in use",
        usage: "Usage: cvs release [-d] directories...\n\
                \t-d\tDelete the given directory.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "remove",
        aliases: "rm delete",
        summary: "Remove an entry from the repository",
        usage: "Usage: cvs remove [-flR] [files...]\n\
                \t-f\tDelete the file before removing it.\n\
                \t-l\tProcess this directory only (not recursive).\n\
                \t-R\tProcess directories recursively.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "rlog",
        aliases: "rl",
        summary: "Print out history information for a module",
        usage: "Usage: cvs rlog [-lRhtNb] [-r[revisions]] [-d dates] [-s states]\n\
                \x20\x20\x20\x20[-w[logins]] [files...]\n\
                \t-l\tLocal directory only, no recursion.\n\
                \t-b\tList revisions on the default branch.\n\
                \t-h\tOnly print header.\n\
                \t-R\tOnly print name of RCS file.\n\
                \t-t\tOnly print header and descriptive text.\n\
                \t-N\tDo not list tags.\n\
                \t-n\tList tags (default).\n\
                \t-S\tDo not print name/header if no revisions selected.  -d, -r,\n\
                \t\t-s, & -w have little effect in conjunction with -b, -h, -R, and\n\
                \t\t-t without this option.\n\
                \t-r[revisions]\tA comma-separated list of revisions to print:\n\
                \t   rev1:rev2   Between rev1 and rev2, including rev1 and rev2.\n\
                \t   rev1::rev2  Between rev1 and rev2, excluding rev1.\n\
                \t   rev:        rev and following revisions on the same branch.\n\
                \t   rev::       After rev on the same branch.\n\
                \t   :rev        rev and previous revisions on the same branch.\n\
                \t   ::rev       rev and previous revisions on the same branch.\n\
                \t   rev         Just rev.\n\
                \t   branch      All revisions on the branch.\n\
                \t   branch.     The last revision on the branch.\n\
                \t-d dates\tA semicolon-separated list of dates\n\
                \t        \t(D1<D2 for range, D for latest before).\n\
                \t-s states\tOnly list revisions with specified states.\n\
                \t-w[logins]\tOnly list revisions checked in by specified logins.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "rtag",
        aliases: "rt rfreeze",
        summary: "Add a symbolic tag to a module",
        usage: "Usage: cvs rtag [-abdFflnR] [-r rev|-D date] tag modules...\n\
                \t-a\tClear tag from removed files that would not otherwise be tagged.\n\
                \t-b\tMake the tag a \"branch\" tag, allowing concurrent development.\n\
                \t-B\tAllows -F and -d to disturb branch tags.  Use with extreme care.\n\
                \t-d\tDelete the given tag.\n\
                \t-F\tMove tag if it already exists.\n\
                \t-f\tForce a head revision match if tag/date not found.\n\
                \t-l\tLocal directory only, not recursive.\n\
                \t-n\tNo execution of 'tag program'.\n\
                \t-R\tProcess directories recursively.\n\
                \t-r rev\tExisting revision/tag.\n\
                \t-D\tExisting date.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "status",
        aliases: "st stat",
        summary: "Display status information on checked out files",
        usage: "Usage: cvs status [-vlR] [files...]\n\
                \t-v\tVerbose format; includes tag information for the file\n\
                \t-l\tProcess this directory only (not recursive).\n\
                \t-R\tProcess directories recursively.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "tag",
        aliases: "ta freeze",
        summary: "Add a symbolic tag to checked out version of files",
        usage: "Usage: cvs tag [-bcdFflR] [-r rev|-D date] tag [files...]\n\
                \t-b\tMake the tag a \"branch\" tag, allowing concurrent development.\n\
                \t-B\tAllows -F and -d to disturb branch tags.  Use with extreme care.\n\
                \t-c\tCheck that working files are unmodified.\n\
                \t-d\tDelete the given tag.\n\
                \t-F\tMove tag if it already exists.\n\
                \t-f\tForce a head revision match if tag/date not found.\n\
                \t-l\tLocal directory only, not recursive.\n\
                \t-R\tProcess directories recursively.\n\
                \t-r rev\tExisting revision/tag.\n\
                \t-D\tExisting date.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "unedit",
        aliases: "",
        summary: "Undo an edit command",
        usage: "Usage: cvs unedit [-lR] [<file>]...\n\
                -l\tLocal directory only, not recursive.\n\
                -R\tProcess directories recursively (default).\n\
                (Specify the --help global option for a list of other help options.)",
    },
    CommandInfo {
        names: "update",
        aliases: "up upd",
        summary: "Bring work tree in sync with repository",
        usage: "Usage: cvs update [-APCdflRp] [-k kopt] [-r rev] [-D date] [-j rev]\n\
                \x20\x20\x20\x20[-I ign] [-W spec] [files...]\n\
                \t-A\tReset any sticky tags/date/kopts.\n\
                \t-P\tPrune empty directories.\n\
                \t-C\tOverwrite locally modified files with clean repository copies.\n\
                \t-d\tBuild directories, like checkout does.\n\
                \t-f\tForce a head revision match if tag/date not found.\n\
                \t-l\tLocal directory only, no recursion.\n\
                \t-R\tProcess directories recursively.\n\
                \t-p\tSend updates to standard output (avoids stickiness).\n\
                \t-k kopt\tUse RCS kopt -k option on checkout. (is sticky)\n\
                \t-r rev\tUpdate using specified revision/tag (is sticky).\n\
                \t-D date\tSet date to update from (is sticky).\n\
                \t-j rev\tMerge in changes made between current revision and rev.\n\
                \t-I ign\tMore files to ignore (! to reset).\n\
                \t-W spec\tWrappers specification line.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "version",
        aliases: "ve ver",
        summary: "Show current CVS version(s)",
        usage: "Usage: cvs version",
    },
    CommandInfo {
        names: "watch",
        aliases: "",
        summary: "Set watches",
        usage: "Usage: cvs watch {on|off|add|remove} [-lR] [-a <action>]... [<path>]...\n\
                on/off: Turn on/off read-only checkouts of files.\n\
                add/remove: Add or remove notification on actions.\n\
                -l (on/off/add/remove): Local directory only, not recursive.\n\
                -R (on/off/add/remove): Process directories recursively (default).\n\
                -a (add/remove): Specify what actions, one of: `edit', `unedit',\n\
                \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20`commit', `all', or `none' (defaults to `all').\n\
                (Specify the --help global option for a list of other help options.)",
    },
    CommandInfo {
        names: "watchers",
        aliases: "",
        summary: "See who is watching a file",
        usage: "Usage: cvs watchers [-lR] [<file>]...\n\
                -l\tProcess this directory only (not recursive).\n\
                -R\tProcess directories recursively (default).\n\
                (Specify the --help global option for a list of other help options.)",
    },
    CommandInfo {
        names: "noop",
        aliases: "",
        summary: "Do nothing; used to test a connection",
        usage: "Usage: cvs noop",
    },
];

pub(crate) fn command_help(name: &str) -> Option<&'static str> {
    let name = canonical(name);
    COMMANDS
        .iter()
        .find(|command| command.names == name)
        .map(|command| command.usage)
}

/// The text of `--help-commands`: one line per command.
pub(crate) fn help_commands() -> String {
    let mut text = String::from("CVS commands are:");
    for command in COMMANDS {
        text.push_str(&format!(
            "\n        {:<13}{}",
            command.names, command.summary
        ));
    }
    text.push_str("\n(Specify the --help option for a list of other help options)");
    text
}

/// The text of `--help-synonyms`: only the commands that have more than one name.
pub(crate) fn help_synonyms() -> String {
    let mut text = String::from("CVS command synonyms are:");
    for command in COMMANDS {
        if command.aliases.is_empty() {
            continue;
        }
        // The reference prints both nickname fields with `%s %s`, so a command with
        // only one synonym ends its line with a space.
        let mut aliases = command.aliases.to_string();
        if !aliases.contains(' ') {
            aliases.push(' ');
        }
        text.push_str(&format!("\n        {:<12}  {}", command.names, aliases));
    }
    text.push_str("\n(Specify the --help option for a list of other help options)");
    text
}

pub(crate) fn general_help() -> String {
    let mut text = String::new();
    text.push_str("Usage: cvs [cvs-options] command [command-options-and-arguments]\n");
    text.push_str("where cvs-options are specified first, and command is one of:\n\n");
    for command in COMMANDS {
        text.push_str(&format!("  {:<13}{}\n", command.names, command.summary));
    }
    text.push_str("\n(Specify the --help option for a list of other help options)");
    text
}

pub(crate) fn global_options_help() -> &'static str {
    "    -H           Displays usage information for command.\n\
     \x20   -Q           Cause CVS to be really quiet.\n\
     \x20   -q           Cause CVS to be somewhat quiet.\n\
     \x20   -r           Make checked-out files read-only.\n\
     \x20   -w           Make checked-out files read-write (default).\n\
     \x20   -n           Do not execute anything that will change the disk.\n\
     \x20   -t           Show trace of program execution -- try with -n.\n\
     \x20   -v           CVS version and copyright.\n\
     \x20   -T tmpdir    Use 'tmpdir' for temporary files.\n\
     \x20   -e editor    Use 'editor' for editing log information.\n\
     \x20   -d CVS_root  Overrides $CVSROOT as the root of the CVS tree.\n\
     \x20   -f           Do not use the ~/.cvsrc file.\n\
     \x20   -z #         Use compression level '#' for net traffic.\n\
     \x20   -a           Authenticate all net traffic.\n\
     \x20   -s VAR=VAL   Set CVS user variable.\n\
     (Specify the --help option for a list of other help options)"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::cvsrc::Cvsrc;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn cvsrc_global_options_come_before_the_typed_ones() {
        let file = Cvsrc::parse("cvs -q -z9\n");
        let (opts, _) = parse_args_with(&args(&["noop"]), Some(&file)).unwrap();
        assert_eq!(opts.quiet, 1);
        assert_eq!(opts.compression, 9);
    }

    #[test]
    fn cvsrc_command_options_come_before_the_typed_arguments() {
        let file = Cvsrc::parse("rdiff -u\n");
        let (_, invocation) =
            parse_args_with(&args(&["patch", "-r", "1.1", "mod"]), Some(&file)).unwrap();
        let Invocation::Run { name, args } = invocation else {
            panic!("expected a command");
        };
        assert_eq!(name, "patch");
        assert_eq!(args, ["-u", "-r", "1.1", "mod"]);
    }

    #[test]
    fn f_turns_the_file_off() {
        let file = Cvsrc::parse("cvs -q\nrdiff -u\n");
        let (opts, invocation) =
            parse_args_with(&args(&["-f", "rdiff", "-r", "1.1", "mod"]), Some(&file)).unwrap();
        assert_eq!(opts.quiet, 0);
        let Invocation::Run { args, .. } = invocation else {
            panic!("expected a command");
        };
        assert_eq!(args, ["-r", "1.1", "mod"]);
    }

    #[test]
    fn help_takes_the_global_line_but_no_command_options() {
        let file = Cvsrc::parse("cvs -q\nrdiff -u\n");
        let (opts, invocation) = parse_args_with(&args(&["-H", "rdiff"]), Some(&file)).unwrap();
        assert_eq!(opts.quiet, 1);
        assert!(matches!(invocation, Invocation::HelpCommand(_)));
    }
}
