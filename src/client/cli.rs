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
    pub(crate) history_off: bool,
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
            history_off: false,
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
            "-l" => opts.history_off = true,
            "-z" => {
                i += 1;
                opts.compression = parse_compression(&require_arg(args, i, "-z")?)?;
            }
            "-T" | "-e" | "-s" => {
                // Accepted for command-line compatibility; they affect temporary files,
                // the log editor and user variables, none of which this client uses yet.
                i += 1;
                require_arg(args, i, arg)?;
            }
            "-x" | "-a" => {}
            "-D" => {
                i += 1;
                require_arg(args, i, "-D")?;
            }
            other if other.starts_with("-d") && other.len() > 2 => {
                opts.root = Some(other[2..].to_string());
            }
            other if other.starts_with("-z") && other.len() > 2 => {
                opts.compression = parse_compression(&other[2..])?;
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

    fn ask(&mut self, text: String) -> String {
        self.prompt(text);
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
        line.trim_end_matches(['\r', '\n']).to_string()
    }

    fn secret(&mut self, text: String) -> Option<String> {
        Some(self.ask(text))
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
                \t-k\tUse \"rcs-kflag\" to add the file with the specified kflag.\n\
                \t-m\tUse \"message\" for the creation log.\n\
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
                \t(Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "annotate",
        aliases: "ann blame",
        summary: "Show last revision where each line was modified",
        usage: "Usage: cvs annotate [-flRqQ] [-r rev] [-D date] files...",
    },
    CommandInfo {
        names: "checkout",
        aliases: "co get",
        summary: "Checkout sources for editing",
        usage: "Usage: cvs checkout [-ANPRcf] [-r rev] [-D date] [-d dir] [-k kopt] modules...\n\
                \x20 -A             Reset any sticky tags/date/kopts.\n\
                \x20 -N             Don't create a CVS directory (the export behaviour).\n\
                \x20 -P             Prune empty directories.\n\
                \x20 -f             Force a head revision match if tag/date not found.\n\
                \x20 -r rev         Check out revision or tag 'rev'.\n\
                \x20 -D date        Check out revisions as of 'date'.\n\
                \x20 -d dir         Use 'dir' as the working directory name.\n\
                \x20 -k kopt        Use RCS kopt -k option on checkout.",
    },
    CommandInfo {
        names: "commit",
        aliases: "ci com",
        summary: "Check files into the repository",
        usage: "Usage: cvs commit [-flRqQ] [-m message] [-F file] [-r rev] [-j rev] [files...]",
    },
    CommandInfo {
        names: "diff",
        aliases: "",
        summary: "Show differences between revisions",
        usage: "Usage: cvs diff [-lRqQ] [-u] [-c] [-w] [-b] [-B] [-i] [-a] [-N] [-r rev] [-D date] [files...]",
    },
    CommandInfo {
        names: "edit",
        aliases: "",
        summary: "Get ready to edit a watched file",
        usage: "Usage: cvs edit [-cflR] [files...]\n\
                -c: Check that working files are unedited\n\
                -f: Force edit if working files are edited (default)\n\
                -l: Local directory only, not recursive\n\
                -R: Process directories recursively (default)\n\
                -a: Specify what actions for temporary watch, one of\n\
                \x20   edit,unedit,commit,all,none\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "editors",
        aliases: "",
        summary: "See who is editing a watched file",
        usage: "Usage: cvs editors files...",
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
                \t(Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "history",
        aliases: "hi his",
        summary: "Show repository access history",
        usage: "Usage: cvs history [-report] [-flags] [-options args] [files...]\n\n\
                \x20  Reports:\n\
                \x20       -T              Produce report on all TAGs\n\
                \x20       -c              Committed (Modified) files\n\
                \x20       -o              Checked out modules\n\
                \x20       -m <module>     Look for specified module (repeatable)\n\
                \x20       -x [TOEFWUPCGMAR] Extract by record type\n\
                \x20       -e              Everything (same as -x, but all record types)\n\
                \x20  Flags:\n\
                \x20       -a              All users (Default is self)\n\
                \x20       -l              Last modified (committed or modified report)\n\
                \x20       -w              Working directory must match\n\
                \x20  Options:\n\
                \x20       -D <date>       Since date (Many formats)\n\
                \x20       -b <str>        Back to record with str in module/file/repos field\n\
                \x20       -f <file>       Specified file (same as command line) (repeatable)\n\
                \x20       -n <modulename> In module (repeatable)\n\
                \x20       -p <repos>      In repository (repeatable)\n\
                \x20       -r <rev/tag>    Since rev or tag (looks inside RCS files!)\n\
                \x20       -t <tag>        Since tag record placed in history file (by anyone).\n\
                \x20       -u <user>       For user name (repeatable)\n\
                \x20       -z <tz>         Output for time zone <tz> (e.g. -z -0700)\n\
                \x20       (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "import",
        aliases: "im imp",
        summary: "Import sources into CVS, using vendor branches",
        usage: "Usage: cvs import [-d] [-k subst] [-I ign] [-m msg] [-b branch]\n\
                \x20   [-W spec] repository vendor-tag release-tags...\n\
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
        usage: "Usage: cvs log [-lRhtNb] [-r[revisions]] [-d dates] [-s states] [files...]",
    },
    CommandInfo {
        names: "login",
        aliases: "logon lgn",
        summary: "Prompt for password for authenticating server",
        usage: "Usage: cvs login\n(Specify the --help option for a list of other help options)",
    },
    CommandInfo {
        names: "logout",
        aliases: "",
        summary: "Removes entry in .cvspass for remote repository",
        usage: "Usage: cvs logout\n(Specify the --help option for a list of other help options)",
    },
    CommandInfo {
        names: "rannotate",
        aliases: "rann ra",
        summary: "Show last revision where each line of module was modified",
        usage: "Usage: cvs rannotate [-lRf] [-r rev] [-D date] [files...]\n\
                \t-l\tLocal directory only, no recursion.\n\
                \t-R\tProcess directories recursively.\n\
                \t-f\tUse head revision if tag/date not found.\n\
                \t-r rev\tAnnotate file as of specified revision/tag.\n\
                \t-D date\tAnnotate file as of specified date.\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "rdiff",
        aliases: "patch pa",
        summary: "Create 'patch' format diffs between releases",
        usage: "Usage: cvs rdiff [-flR] [-c|-u] [-s|-t] [-V %d] [-k kopt]\n\
                \x20   -r rev|-D date [-r rev2 | -D date2] modules...\n\
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
                \t(Specify the --help global option for a list of other help options)",
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
        usage: "Usage: cvs rlog [-lRhtNb] [-r[revisions]] [-d dates] [-s states]\n    [-w[logins]] [files...]\n\
                \t-l\tLocal directory only, no recursion.\n\
                \t-R\tOnly print name of RCS file.\n\
                \t-h\tOnly print header.\n\
                \t-t\tOnly print header and descriptive text.\n\
                \t-N\tDo not list tags.\n\
                \t-b\tOnly list revisions on the default branch.\n\
                \t-r[revisions]\tSpecify revision(s)s to list.\n\
                \t   rev1:rev2   Between rev1 and rev2, including rev1 and rev2.\n\
                \t   rev1::rev2  Between rev1 and rev2, excluding rev1 and rev2.\n\
                \t   rev:        rev and following revisions on the same branch.\n\
                \t   rev::       After rev on the same branch.\n\
                \t   :rev        rev and previous revisions on the same branch.\n\
                \t   ::rev       Before rev on the same branch.\n\
                \t   rev         Just rev.\n\
                \t   branch      All revisions on the branch.\n\
                \t   branch.     The last revision on the branch.\n\
                \t-d dates\tSpecify dates (D1<D2 for range, D for latest before).\n\
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
        usage: "Usage: cvs status [-vlRqQ] [files...]\n\
                \x20 -v             Verbose: also show tags and dates.\n\
                \x20 -l             Local only; do not recurse into subfolders.\n\
                \x20 -R             Recurse (the default).\n\
                \x20 -q             Be somewhat quiet.",
    },
    CommandInfo {
        names: "tag",
        aliases: "ta freeze",
        summary: "Add a symbolic tag to checked out version of files",
        usage: "Usage: cvs tag [-bcdFflR] [-r rev|-D date] tag [files...]\n\
                \t-b\tMake the tag a \"branch\" tag, allowing concurrent development.\n\
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
        usage: "Usage: cvs unedit [-lR] [files...]\n\
                -l: Local directory only, not recursive\n\
                -R: Process directories recursively\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "update",
        aliases: "up upd",
        summary: "Bring work tree in sync with repository",
        usage: "Usage: cvs update [-APCdflRp] [-k kopt] [-r rev] [-D date] [-j rev] [-I ign] [-W spec] [files...]",
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
        usage: "Usage: cvs watch [on|off|add|remove] [-lR] [-a action] [files...]\n\
                on/off: turn on/off read-only checkouts of files\n\
                add/remove: add or remove notification on actions\n\
                -l (on/off/add/remove): Local directory only, not recursive\n\
                -R (on/off/add/remove): Process directories recursively\n\
                -a (add/remove): Specify what actions, one of\n\
                \x20   edit,unedit,commit,all,none\n\
                (Specify the --help global option for a list of other help options)",
    },
    CommandInfo {
        names: "watchers",
        aliases: "",
        summary: "See who is watching a file",
        usage: "Usage: cvs watchers files...",
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
        text.push_str(&format!(
            "\n        {:<13}{}",
            command.names, command.aliases
        ));
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
    "  -H              Displays usage information for command.\n\
     \x20 -Q              Cause CVS to be really quiet.\n\
     \x20 -q              Cause CVS to be somewhat quiet.\n\
     \x20 -r              Make checked-out files read-only.\n\
     \x20 -w              Make checked-out files read-write (default).\n\
     \x20 -l              Turn history logging off.\n\
     \x20 -n              Do not execute anything that will change the disk.\n\
     \x20 -t              Show trace of program execution -- try with -n.\n\
     \x20 -v              CVS version and copyright.\n\
     \x20 -T tmpdir       Use 'tmpdir' for temporary files.\n\
     \x20 -e editor       Use 'editor' for editing log information.\n\
     \x20 -d CVS_root     Overrides $CVSROOT as the root of the CVS tree.\n\
     \x20 -f              Do not use the ~/.cvsrc file.\n\
     \x20 -z #            Use compression level '#' for net traffic.\n\
     \x20 --version       CVS version and copyright."
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
