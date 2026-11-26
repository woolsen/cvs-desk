//! `cvs import`: put a folder that is not a working copy into the repository.
//!
//! This is the one command that sends file contents *up* without a `CVS/` folder to read
//! them from. The client names the repository, the vendor tag and the release tags, sends a
//! `Directory` block whenever the folder changes, and one `Modified` block per file. The
//! `import` request itself comes last, after all the blocks, which is how the reference
//! orders the conversation.
//!
//! Ignored files never leave the machine: the default list, `~/.cvsignore`, `CVSIGNORE`,
//! `-I`, and each folder's own `.cvsignore` decide. `-k b` marks every file binary, and `-d`
//! asks the server to record each file's own modification time (`Checkin-time`).
//!
//! There is no editor here, so `-m` is required; the reference would open one. When a
//! conflicting import produces an `importmergecmd` group of `MT` responses, the suggested
//! merge command is printed after the report.

use std::path::Path;

use crate::client::cli;
use crate::client::commands::date;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::ignore::Ignore;
use crate::client::protocol::{self, Response};
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::dir;

/// The branch `cvs import` uses when `-b` is not given.
const DEFAULT_BRANCH: &str = "1.1.1";

#[derive(Debug)]
struct Options {
    branch: String,
    branch_given: bool,
    dated: bool,
    ignore: Vec<String>,
    /// `-W` wrappers: accepted, but this client has no wrapper rules to apply.
    wrappers: bool,
    keyword: Option<String>,
    message: Option<String>,
    names: Vec<String>,
}

enum Parsed {
    Options(Options),
    Usage,
}

fn take(args: &[String], i: &mut usize, rest: &str, option: &str) -> Result<String> {
    if !rest.is_empty() {
        return Ok(rest.to_string());
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| Error::plain("import", format!("option {} requires an argument", option)))
}

fn parse_args(args: &[String]) -> Result<Parsed> {
    let mut opts = Options {
        branch: DEFAULT_BRANCH.to_string(),
        branch_given: false,
        dated: false,
        ignore: Vec::new(),
        wrappers: false,
        keyword: None,
        message: None,
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
        let letter = arg.as_bytes()[1] as char;
        let rest = &arg[2..];
        match letter {
            'Q' | 'q' if rest.is_empty() => {
                return Err(Error::plain(
                    "import",
                    "-q or -Q must be specified before \"import\"",
                ));
            }
            'd' if rest.is_empty() => opts.dated = true,
            'b' => {
                opts.branch = take(args, &mut i, rest, "-b")?;
                opts.branch_given = true;
            }
            'm' => opts.message = Some(take(args, &mut i, rest, "-m")?),
            'k' => opts.keyword = Some(take(args, &mut i, rest, "-k")?),
            'I' => opts.ignore.push(take(args, &mut i, rest, "-I")?),
            'W' => {
                let _ = take(args, &mut i, rest, "-W")?;
                opts.wrappers = true;
            }
            _ => return Ok(Parsed::Usage),
        }
        i += 1;
    }
    Ok(Parsed::Options(opts))
}

fn usage(sink: &mut dyn Sink) {
    if let Some(text) = cli::command_help("import") {
        for line in text.lines() {
            sink.err(line.to_string());
        }
    }
}

/// Everything an import sends, kept together so the recursive walk reads as the file tree.
struct Upload<'a, 'b> {
    session: &'a mut protocol::Session<'b>,
    /// The last `Directory` block sent, so consecutive files in one folder name it once.
    last: Option<(String, String)>,
    options: &'a Options,
    warned_keyword: bool,
    warned_date: bool,
}

impl Upload<'_, '_> {
    fn directory(&mut self, update_dir: &str, repository: &str) -> Result<()> {
        let key = (update_dir.to_string(), repository.to_string());
        if self.last.as_ref() == Some(&key) {
            return Ok(());
        }
        let local = if update_dir.is_empty() {
            "."
        } else {
            update_dir
        };
        self.session.send(&format!("Directory {}", local))?;
        self.session.send(repository)?;
        self.last = Some(key);
        Ok(())
    }

    fn file(
        &mut self,
        folder: &Path,
        name: &str,
        repository: &str,
        update_dir: &str,
    ) -> Result<()> {
        let path = folder.join(name);
        let metadata = std::fs::metadata(&path)?;
        let binary = self.options.keyword.as_deref() == Some("b");

        self.directory(update_dir, repository)?;
        if binary {
            if self.session.supports("Kopt") {
                self.session.send("Kopt -kb")?;
            } else if !self.warned_keyword {
                self.warned_keyword = true;
                self.session
                    .err("cvs import: warning: ignoring -k options due to server limitations");
            }
        }
        if self.options.dated {
            if self.session.supports("Checkin-time") {
                let time = metadata.modified()?;
                self.session.send(&format!(
                    "Checkin-time {}",
                    date::internet_from_system(time)
                ))?;
            } else if !self.warned_date {
                self.warned_date = true;
                self.session
                    .err("cvs import: warning: ignoring -d option due to server limitations");
            }
        }

        let content = std::fs::read(&path)?;
        let content = if binary {
            content
        } else {
            codec::to_repository_newlines(&content)
        };
        let mode = if metadata.permissions().readonly() {
            "u=r,g=r,o=r"
        } else {
            "u=rw,g=rw,o=rw"
        };
        self.session.send(&format!("Modified {}", name))?;
        self.session.send(mode)?;
        self.session.send(&content.len().to_string())?;
        self.session.send_bytes(&content)?;
        Ok(())
    }
}

/// Walk one folder: files first, then subfolders, each in name order.
fn walk(
    upload: &mut Upload<'_, '_>,
    folder: &Path,
    repository: &str,
    update_dir: &str,
    inherited: &Ignore,
) -> Result<()> {
    let ignore = inherited.enter(folder);
    let mut files = Vec::new();
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "CVS" || ignore.is_ignored(&name) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            folders.push(name);
        } else if kind.is_file() {
            files.push(name);
        }
    }
    files.sort();
    folders.sort();

    for name in files {
        upload.file(folder, &name, repository, update_dir)?;
    }
    for name in folders {
        let child_update = if update_dir.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", update_dir, name)
        };
        let child_repository = format!("{}/{}", repository.trim_end_matches('/'), name);
        walk(
            upload,
            &folder.join(&name),
            &child_repository,
            &child_update,
            &ignore,
        )?;
    }
    Ok(())
}

/// Run `cvs import`. `cwd` is the folder whose contents are imported.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = match parse_args(args)? {
        Parsed::Options(opts) => opts,
        Parsed::Usage => {
            usage(sink);
            return Ok(1);
        }
    };
    if opts.names.len() < 3 {
        usage(sink);
        return Ok(1);
    }
    let module = opts.names[0].clone();
    let vendor = opts.names[1].clone();
    let releases = opts.names[2..].to_vec();

    check_module(&module)?;
    check_tags(&vendor, &releases)?;
    check_branch(&opts.branch)?;

    let Some(message) = opts.message.as_deref().filter(|text| !text.is_empty()) else {
        return Err(Error::aborted(
            "import",
            "no log message; use -m \"message\"",
        ));
    };
    if opts.wrappers {
        sink.err(
            "cvs import: warning: -W wrappers are not supported; the line was ignored".to_string(),
        );
    }

    let ignore = Ignore::new(&opts.ignore);
    let mut session = protocol::open(root, "import", cancel, sink)?;

    if opts.branch_given {
        session.argument("-b")?;
        session.argument(&opts.branch)?;
    }
    // The reference makes sure the message ends with a newline.
    let mut message = message.to_string();
    if !message.ends_with('\n') {
        message.push('\n');
    }
    session.argument("-m")?;
    session.argument(&message)?;
    if let Some(keyword) = &opts.keyword {
        session.argument("-k")?;
        session.argument(keyword)?;
    }
    if ignore.inhibits_server() {
        session.argument("-I")?;
        session.argument("!")?;
    }
    session.argument(&module)?;
    session.argument(&vendor)?;
    for release in &releases {
        session.argument(release)?;
    }

    let top = dir::absolute_repository(&session.root.protocol_root(), &module);
    let mut upload = Upload {
        session: &mut session,
        last: None,
        options: &opts,
        warned_keyword: false,
        warned_date: false,
    };
    upload.directory("", &top)?;
    walk(&mut upload, cwd, &top, "", &ignore)?;
    // `client_import_done`: the top folder is named again unless it was the last one.
    upload.directory("", &top)?;
    drop(upload);

    session.send("import")?;
    session.flush()?;
    read_import_answer(&mut session)?;
    session.close();
    Ok(0)
}

/// Read the server's report. Besides the ordinary `M` lines, the server can group `MT`
/// responses into an `importmergecmd` block that names the conflicts and the merge command
/// to suggest; the reference prints that block in one piece when it closes.
fn read_import_answer(session: &mut protocol::Session) -> Result<()> {
    let mut merge = MergeHint::default();
    loop {
        match session.read_response()? {
            Response::Ok => return Ok(()),
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::Trace(text) => {
                let closing = text.starts_with("-importmergecmd");
                if merge.feed(&text) && closing {
                    if merge.complete() {
                        print_merge_hint(session, &merge);
                    } else {
                        session.err(
                            "cvs import: warning: invalid server: incomplete importmergecmd tags"
                                .to_string(),
                        );
                    }
                    merge = MergeHint::default();
                }
            }
            Response::Notified | Response::Checksum(_) => {}
            Response::Error { message, .. } => {
                return Err(Error::aborted(
                    "import",
                    if message.trim().is_empty() {
                        "the server reported an error".to_string()
                    } else {
                        message
                    },
                ))
            }
            other => {
                return Err(Error::aborted(
                    "import",
                    format!("unexpected answer: {:?}", other),
                ))
            }
        }
    }
}

/// The fields of one `+importmergecmd` … `-importmergecmd` group.
#[derive(Default)]
struct MergeHint {
    conflicts: u32,
    tag1: String,
    tag2: String,
    repository: String,
}

impl MergeHint {
    /// Take one `MT` body. True when it belonged to the group and was consumed.
    fn feed(&mut self, text: &str) -> bool {
        let (tag, value) = match text.split_once(' ') {
            Some((tag, value)) => (tag, value),
            None => (text, ""),
        };
        match tag {
            "+importmergecmd" => *self = MergeHint::default(),
            "conflicts" => self.conflicts = value.parse().unwrap_or(0),
            "mergetag1" => self.tag1 = value.to_string(),
            "mergetag2" => self.tag2 = value.to_string(),
            "repository" => self.repository = value.to_string(),
            "-importmergecmd" => {}
            _ => return false,
        }
        true
    }

    fn complete(&self) -> bool {
        self.conflicts > 0
            && !self.tag1.is_empty()
            && !self.tag2.is_empty()
            && !self.repository.is_empty()
    }
}

fn print_merge_hint(session: &mut protocol::Session, merge: &MergeHint) {
    session.out(String::new());
    session.out(format!(
        "{} conflicts created by this import.",
        merge.conflicts
    ));
    session.out("Use the following command to help the merge:".to_string());
    session.out(String::new());
    session.out(format!(
        "\tcvs -d {} checkout -j{} -j{} {}",
        session.root.display(),
        merge.tag1,
        merge.tag2,
        merge.repository
    ));
    session.out(String::new());
}

fn check_module(module: &str) -> Result<()> {
    let reserved = module.split(['/', '\\']).any(|part| part == "CVS");
    if reserved {
        return Err(Error::aborted(
            "import",
            "The word `CVS' is reserved by CVS and may not be used as a directory in a path or as a file name.",
        ));
    }
    let inside =
        !module.starts_with(['/', '\\']) && !module.split(['/', '\\']).any(|part| part == "..");
    if !inside {
        return Err(Error::aborted(
            "import",
            format!("directory {} not relative within the repository", module),
        ));
    }
    Ok(())
}

fn check_tags(vendor: &str, releases: &[String]) -> Result<()> {
    check_tag(vendor)?;
    for (index, release) in releases.iter().enumerate() {
        check_tag(release)?;
        if release == vendor || releases[..index].iter().any(|earlier| earlier == release) {
            return Err(Error::aborted(
                "import",
                format!("tag `{}' was specified more than once", release),
            ));
        }
    }
    Ok(())
}

/// The rule RCS applies: a letter first, visible characters only, and none of `$,.:;@`.
fn check_tag(tag: &str) -> Result<()> {
    let mut chars = tag.chars();
    match chars.next() {
        Some(first) if first.is_alphabetic() => {}
        _ => {
            return Err(Error::aborted(
                "import",
                format!("tag `{}' must start with a letter", tag),
            ))
        }
    }
    for c in chars {
        if !c.is_ascii_graphic() {
            return Err(Error::aborted(
                "import",
                format!("tag `{}' has non-visible graphic characters", tag),
            ));
        }
        if "$,.:;@".contains(c) {
            return Err(Error::aborted(
                "import",
                format!("tag `{}' must not contain the characters `$,.:;@'", tag),
            ));
        }
    }
    Ok(())
}

fn check_branch(branch: &str) -> Result<()> {
    let parts: Vec<&str> = branch.split('.').collect();
    let numeric = |text: &str| {
        !text.is_empty()
            && text.chars().all(|c| c.is_ascii_digit())
            && text.starts_with(|c: char| c != '0')
    };
    if parts.len() != 3 || !parts.iter().all(|part| numeric(part)) {
        return Err(Error::aborted(
            "import",
            format!(
                "Only numeric branch specifications with two dots are\nsupported by import, not `{}'.  For example: `1.1.1'.",
                branch
            ),
        ));
    }
    Ok(())
}
