//! `cvs import`: put a folder that is not a working copy into the repository.
//!
//! This is the one command that sends file contents *up* without a `CVS/` folder to read
//! them from. The client names the repository, the vendor tag and the release tags, sends a
//! `Directory` block whenever the folder changes, and one `Modified` block per file. The
//! `import` request itself comes last, after all the blocks, which is how the reference
//! orders the conversation.
//!
//! Ignored files never leave the machine: the default list, `~/.cvsignore`, `CVSIGNORE`,
//! `-I`, and each folder's own `.cvsignore` decide. `-W` and each folder's `.cvswrappers`
//! choose the keyword mode of matching files, so a wrapper can keep a binary file from being
//! run through the CRLF conversion. `-k b` marks every file binary, and `-d` asks the server
//! to record each file's own modification time (`Checkin-time`).
//!
//! Without `-m` the reference opens an editor (`-e`, `CVSEDITOR`, `VISUAL`, `EDITOR`, then
//! the platform's plain editor); `-m ""` is a valid, empty message. When a conflicting import
//! produces an `importmergecmd` group of `MT` responses, the suggested merge command is
//! printed after the report, once the whole group has been read.

use std::path::{Path, PathBuf};

use crate::client::cli;
use crate::client::commands::date;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::ignore::{self, Ignore};
use crate::client::protocol::{self, Response};
use crate::client::root::{self, Root};
use crate::client::work::codec;
use crate::client::work::dir;

/// The branch `cvs import` uses when `-b` is not given.
const DEFAULT_BRANCH: &str = "1.1.1";

/// The modes `RCS_check_kflag` accepts (rcs.c:47-49).
const KEYWORDS: [&str; 6] = ["kv", "kvl", "k", "v", "o", "b"];

#[derive(Debug)]
struct Options {
    branch: String,
    dated: bool,
    ignore: Vec<String>,
    /// Each `-W` spec, kept until the permanent wrapper list is built.
    wrappers: Vec<String>,
    keyword: Option<String>,
    message: Option<String>,
    names: Vec<String>,
}

#[derive(Debug)]
enum Parsed {
    Options(Options),
    Usage,
}

fn parse_args(args: &[String]) -> Result<Parsed> {
    let mut opts = Options {
        branch: DEFAULT_BRANCH.to_string(),
        dated: false,
        ignore: Vec::new(),
        wrappers: Vec::new(),
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
        let text = &arg[1..];
        // getopt accepts clustered flags: `-dm msg` is `-d -m msg`, `-dIx` is `-d -I x`.
        // An option that takes a value consumes the rest of the cluster, if any.
        for (pos, letter) in text.char_indices() {
            match letter {
                'Q' | 'q' => {
                    // `error (1, …)` at status 1: the reference labels it an abort.
                    return Err(Error::aborted(
                        "import",
                        "-q or -Q must be specified before \"import\"",
                    ));
                }
                'd' => opts.dated = true,
                'b' | 'm' | 'I' | 'k' | 'W' => {
                    let rest = &text[pos + letter.len_utf8()..];
                    let value = if !rest.is_empty() {
                        rest.to_string()
                    } else {
                        i += 1;
                        args.get(i).cloned().ok_or_else(|| {
                            Error::plain(
                                "import",
                                format!("option -{} requires an argument", letter),
                            )
                        })?
                    };
                    match letter {
                        'b' => opts.branch = value,
                        'm' => opts.message = Some(value),
                        'I' => opts.ignore.push(value),
                        'k' => {
                            check_keyword(&value)?;
                            opts.keyword = Some(value);
                        }
                        'W' => opts.wrappers.push(value),
                        _ => unreachable!(),
                    }
                    break;
                }
                _ => return Ok(Parsed::Usage),
            }
        }
        i += 1;
    }
    Ok(Parsed::Options(opts))
}

/// `RCS_check_kflag`: the value after `-k` is one of a short fixed list. The reference
/// rejects anything else before it contacts the server.
fn check_keyword(value: &str) -> Result<()> {
    if KEYWORDS.contains(&value) {
        return Ok(());
    }
    Err(Error::plain(
        "import",
        "invalid RCS keyword expansion mode\n\
         Valid expansion modes include:\n\
         \x20  -kkv\tGenerate keywords using the default form.\n\
         \x20  -kkvl\tLike -kkv, except locker's name inserted.\n\
         \x20  -kk\tGenerate only keyword names in keyword strings.\n\
         \x20  -kv\tGenerate only keyword values in keyword strings.\n\
         \x20  -ko\tGenerate the old keyword string (no changes from checked in file).\n\
         \x20  -kb\tGenerate binary file unmodified (merges not allowed) (RCS 5.7).\n\
         (Specify the --help global option for a list of other help options)",
    ))
}

fn usage(sink: &mut dyn Sink) {
    if let Some(text) = cli::command_help("import") {
        for line in text.lines() {
            sink.err(line.to_string());
        }
    }
}

/// One `cvswrappers` line, with only the fields `import` can honour: the name pattern and
/// the `-k` mode. `-m COPY` is remembered only so the remote warning can be printed.
#[derive(Clone, Debug)]
struct Wrapper {
    pattern: String,
    /// The RCS mode without `-k`; `None` for the default (`kv`), which sends no `Kopt`.
    rcs: Option<String>,
    copy: bool,
}

impl Wrapper {
    /// `wrap_send`'s wire form: the pattern and its keyword mode, defaulting to `kv`.
    fn argument(&self) -> String {
        format!(
            "{} -k '{}'",
            self.pattern,
            self.rcs.as_deref().unwrap_or("kv")
        )
    }

    fn rcs_option(&self) -> Option<String> {
        self.rcs.as_deref().map(|mode| format!("-k{}", mode))
    }
}

/// The permanent wrappers (`~/.cvswrappers`, `$CVSWRAPPERS`, `-W`) plus the temporary ones
/// read from one folder's `.cvswrappers`. As with `.cvsignore`, a folder's file replaces the
/// parent's temporary entries rather than adding to them.
#[derive(Clone, Debug, Default)]
struct Wrappers {
    permanent: Vec<Wrapper>,
    temporary: Vec<Wrapper>,
}

impl Wrappers {
    fn new(extra: &[String]) -> Result<Wrappers> {
        let mut permanent = Vec::new();
        if let Some(home) = ignore::home_dir() {
            if let Some(text) = read_text_file(&home.join(".cvswrappers")) {
                add_wrapper_lines(&mut permanent, &text)?;
            }
        }
        if let Ok(value) = std::env::var("CVSWRAPPERS") {
            add_wrapper_line(&mut permanent, &value)?;
        }
        for spec in extra {
            add_wrapper_line(&mut permanent, spec)?;
        }
        Ok(Wrappers {
            permanent,
            temporary: Vec::new(),
        })
    }

    fn enter(&self, folder: &Path) -> Result<Wrappers> {
        let mut temporary = Vec::new();
        if let Some(text) = read_text_file(&folder.join(".cvswrappers")) {
            add_wrapper_lines(&mut temporary, &text)?;
        }
        Ok(Wrappers {
            permanent: self.permanent.clone(),
            temporary,
        })
    }

    /// `wrap_matching_entry`: the first entry whose pattern matches decides, even when it
    /// names no `-k`.
    fn matching(&self, name: &str) -> Option<&Wrapper> {
        self.permanent
            .iter()
            .chain(self.temporary.iter())
            .find(|wrapper| ignore::glob(&wrapper.pattern, name))
    }

    /// `wrap_rcsoption(name, 1)`: `-kb`, `-ko`, … or nothing at all.
    fn rcs_option(&self, name: &str) -> Option<String> {
        self.matching(name).and_then(Wrapper::rcs_option)
    }
}

fn read_text_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(codec::detect(&bytes).decode(&bytes))
}

fn add_wrapper_lines(list: &mut Vec<Wrapper>, text: &str) -> Result<()> {
    for line in text.lines() {
        add_wrapper_line(list, line)?;
    }
    Ok(())
}

/// Parse one line the way `wrap_add` does: a pattern, then `-x 'value'` pairs. A `-t` or
/// `-f` filter is fatal in this CVS version (wrapper.c:418-447).
fn add_wrapper_line(list: &mut Vec<Wrapper>, line: &str) -> Result<()> {
    if line.is_empty() || line.starts_with('#') {
        return Ok(());
    }
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    while i < len && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if start == i {
        return Ok(());
    }
    let mut wrapper = Wrapper {
        pattern: line[start..i].to_string(),
        rcs: None,
        copy: false,
    };
    while i < len {
        while i < len && bytes[i] != b'-' {
            i += 1;
        }
        if i >= len {
            break;
        }
        i += 1;
        if i >= len {
            break;
        }
        let option = bytes[i] as char;
        i += 1;
        while i < len && bytes[i] != b'\'' {
            i += 1;
        }
        if i >= len {
            break;
        }
        i += 1;
        let value_start = i;
        while i < len && !(bytes[i] == b'\'' && bytes[i - 1] != b'\\') {
            i += 1;
        }
        if i == value_start {
            // An empty value stops the line, as the reference does.
            break;
        }
        let value = &line[value_start..i];
        match option {
            't' | 'f' => {
                return Err(Error::aborted(
                    "import",
                    "-t/-f wrappers not supported by this version of CVS",
                ))
            }
            'm' => wrapper.copy = value.starts_with(['C', 'c']),
            'k' => {
                wrapper.rcs = if value == "kv" {
                    None
                } else {
                    Some(value.to_string())
                }
            }
            _ => {}
        }
        if i < len {
            i += 1;
        }
    }
    list.push(wrapper);
    Ok(())
}

/// Everything an import sends, kept together so the recursive walk reads as the file tree.
struct Upload<'a, 'b> {
    session: &'a mut protocol::Session<'b>,
    /// The last `Directory` block sent, so consecutive files in one folder name it once.
    last: Option<(String, String)>,
    options: &'a Options,
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

    /// The `I`/`L` report `add_log` prints: the path relative to the repository root. `-Q`
    /// hides it; `-q` does not.
    fn status(&mut self, mark: char, repository: &str, name: &str) {
        if self.session.root.options.quiet >= 2 {
            return;
        }
        let root = self.session.root.protocol_root().trim_end_matches('/');
        let folder = repository
            .strip_prefix(root)
            .unwrap_or(repository)
            .trim_matches('/');
        let path = if folder.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", folder, name)
        };
        self.session.out(format!("{} {}", mark, path));
    }

    fn file(
        &mut self,
        folder: &Path,
        name: &str,
        repository: &str,
        update_dir: &str,
        wrappers: &Wrappers,
    ) -> Result<()> {
        let path = folder.join(name);
        let metadata = std::fs::metadata(&path)?;
        // `client_process_import_file`: `-k b` wins; otherwise the first matching wrapper
        // decides, and only an exact `-kb` keeps the bytes from being converted.
        let keyword = if self.options.keyword.as_deref() == Some("b") {
            Some("-kb".to_string())
        } else {
            wrappers.rcs_option(name)
        };
        let binary = keyword.as_deref() == Some("-kb");

        self.directory(update_dir, repository)?;
        if let Some(mode) = &keyword {
            if self.session.supports("Kopt") {
                self.session.send(&format!("Kopt {}", mode))?;
            } else {
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
            } else {
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

/// Walk one folder: each entry is handled as the disk lists it (an ignored name is `I`, a
/// symbolic link is `L`, a regular file is uploaded at once), and subfolders are entered
/// after the folder's own entries, as `import_descend` does.
/// Returns how many entries could not be imported (symbolic links).
fn walk(
    upload: &mut Upload<'_, '_>,
    folder: &Path,
    repository: &str,
    update_dir: &str,
    inherited_ignore: &Ignore,
    inherited_wrappers: &Wrappers,
) -> Result<usize> {
    let ignore = inherited_ignore.enter(folder);
    let wrappers = inherited_wrappers.enter(folder)?;
    let mut folders = Vec::new();
    let mut errors = 0;
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type()?;
        // Ignoring is decided first, so an ignored link is `I`, not `L`.
        if ignore.is_ignored(&name) {
            upload.status('I', repository, &name);
            continue;
        }
        if kind.is_symlink() {
            upload.status('L', repository, &name);
            errors += 1;
            continue;
        }
        if kind.is_dir() {
            folders.push(name);
        } else if kind.is_file() {
            upload.file(folder, &name, repository, update_dir, &wrappers)?;
        }
    }

    for name in folders {
        let child_update = if update_dir.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", update_dir, name)
        };
        let child_repository = format!("{}/{}", repository.trim_end_matches('/'), name);
        errors += walk(
            upload,
            &folder.join(&name),
            &child_repository,
            &child_update,
            &ignore,
            &wrappers,
        )?;
    }
    Ok(errors)
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

    check_cvs_reserved(&module)?;
    check_tags(&vendor, &releases)?;
    check_module_path(&module)?;
    check_branch(&opts.branch)?;
    refuse_inherited_root(cwd, &root)?;

    let ignore = Ignore::new(&opts.ignore);
    let wrappers = Wrappers::new(&opts.wrappers)?;

    let mut session = protocol::open(root, "import", cancel, sink)?;

    // `start_server` comes before `do_editor` in the reference, so a root that cannot be
    // reached fails before the user spends time in the editor.
    let editor = session.root.options.editor.clone();
    let message = match &opts.message {
        Some(text) => text.clone(),
        None if session.no_execute() => String::new(),
        None => match edit_message(cwd, editor.as_deref(), &mut session)? {
            (text, warning) => {
                if let Some(warning) = warning {
                    session.err(warning);
                }
                text.unwrap_or_default()
            }
        },
    };
    let message = message_with_newline(&message);

    // `-b` is always sent, with its default when the user named no branch.
    session.argument("-b")?;
    session.argument(&opts.branch)?;
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
    for wrapper in &wrappers.permanent {
        if wrapper.copy {
            session.err("cvs import: -m wrapper option is not supported remotely; ignored");
        }
        session.argument("-W")?;
        session.argument(&wrapper.argument())?;
    }
    // The module and tags are the command's operands; `--` separates them from its options.
    // The reference sends the module name and the vendor/release tags directly; it does
    // not put `Argument --` before them (import.c:292-296).
    session.argument(&module)?;
    session.argument(&vendor)?;
    for release in &releases {
        session.argument(release)?;
    }

    let top = dir::absolute_repository(&session.root.protocol_root(), &module);
    let errors = {
        let mut upload = Upload {
            session: &mut session,
            last: None,
            options: &opts,
        };
        upload.directory("", &top)?;
        let errors = walk(&mut upload, cwd, &top, "", &ignore, &wrappers)?;
        // `client_import_done` names the top folder again unconditionally, even when the
        // last file was already in it (client.c:5775-5800).
        upload.last = None;
        upload.directory("", &top)?;
        errors
    };

    session.send("import")?;
    session.flush()?;
    read_import_answer(&mut session)?;
    session.close();
    Ok(if errors > 0 { 1 } else { 0 })
}

/// The message the wire carries: always ends in a newline, even when it is empty
/// (import.c:251-270).
fn message_with_newline(message: &str) -> String {
    let mut text = message.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// `CVS_CMD_IGNORE_ADMROOT`: with neither `-d` nor `$CVSROOT`, `import` must not use a
/// `CVS/Root` found above the folder. The engine resolves a root for every command, so the
/// inherited case is stopped here before anything is uploaded.
fn refuse_inherited_root(cwd: &Path, root: &Root) -> Result<()> {
    if std::env::var("CVSROOT").is_ok_and(|value| !value.trim().is_empty()) {
        return Ok(());
    }
    if root_was_named() {
        return Ok(());
    }
    if root::find_working_root(cwd).is_some_and(|(_, found)| found.key == root.key) {
        return Err(Error::new(format!(
            "cvs import: missing CVSROOT environment variable\n\n\
             cvs [import aborted]: Set it or specify the '-d' option to {}.",
            program_name()
        )));
    }
    Ok(())
}

/// Did this process name the root itself? The command line is inspected for `-d`; a caller
/// that is not the `cvs` program (the window) is left alone.
fn root_was_named() -> bool {
    match command_line_prefix() {
        Some(prefix) => prefix.iter().any(|arg| is_root_option(arg)),
        // A caller that did not spell a command (the window) passes its root in the request.
        None => true,
    }
}

/// Was the root named by `-d`? The merge hint prints `-d` only in this case; a root from
/// `$CVSROOT` or `CVS/Root` is left out (client.c:3213-3231).
fn root_from_command_line() -> bool {
    command_line_prefix().is_some_and(|prefix| prefix.iter().any(|arg| is_root_option(arg)))
}

fn is_root_option(arg: &str) -> bool {
    arg == "-d" || (arg.starts_with("-d") && arg.len() > 2)
}

/// The command line before the command name, or `None` when this is not a command line
/// invocation (the window calls the engine directly and passes its root in the request).
fn command_line_prefix() -> Option<Vec<String>> {
    // Only the `cvs` program has a root on the command line. The window's process arguments
    // say nothing about the request it is about to run.
    let program = program_name();
    if !program.eq_ignore_ascii_case("cvs") && !program.eq_ignore_ascii_case("cvs.exe") {
        return None;
    }
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    args.iter()
        .position(|arg| matches!(arg.as_str(), "import" | "im" | "imp"))
        .map(|command| args[..command].to_vec())
}

fn program_name() -> String {
    std::env::args_os()
        .next()
        .and_then(|arg| {
            Path::new(&arg)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "cvs".to_string())
}

/// The editor text: the current message, then a possible `CVS/Template`, then the
/// instruction lines. Import has no directory name and no file list to add
/// (logmsg.c:219-290).
fn initial_text(template: Option<&str>, message: Option<&str>) -> String {
    let mut text = String::new();
    if let Some(message) = message {
        if !message.is_empty() {
            text.push_str(message);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
    }
    if let Some(template) = template {
        if !template.is_empty() {
            text.push_str(template);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
    }
    text.push_str("CVS: ----------------------------------------------------------------------\n");
    text.push_str("CVS: Enter Log.  Lines beginning with `CVS:' are removed automatically\n");
    text.push_str("CVS: \n");
    text.push_str("CVS: ----------------------------------------------------------------------\n");
    text
}

/// What the user typed, with the `CVS:` instruction lines taken out. The last line may have
/// no newline; an editor on Windows may have written CRLF.
fn message_from_edited(text: &str) -> Option<String> {
    let text = text.replace("\r\n", "\n");
    let mut message = String::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with("CVS:") {
            continue;
        }
        message.push_str(line);
    }
    if message.is_empty() || message == "\n" {
        None
    } else {
        Some(message)
    }
}

/// The editor the reference picks: `-e`, then `CVSEDITOR`, `VISUAL`, `EDITOR`, then the
/// platform's plain editor. An explicitly empty `-e` is kept so it can be rejected.
fn editor_command(option: Option<&str>) -> String {
    if let Some(editor) = option {
        return editor.to_string();
    }
    for name in ["CVSEDITOR", "VISUAL", "EDITOR"] {
        if let Ok(value) = std::env::var(name) {
            return value;
        }
    }
    if cfg!(windows) {
        "notepad".to_string()
    } else {
        "vi".to_string()
    }
}

/// Run the editor on a temporary message file and return what the user wrote.
///
/// If the file did not change (or the message is empty), ask the same a/c/e/! question the
/// reference asks; an answer that cannot be read counts as `c)ontinue`.
fn edit_message(
    cwd: &Path,
    editor_option: Option<&str>,
    session: &mut protocol::Session,
) -> Result<(Option<String>, Option<String>)> {
    let template = std::fs::read(cwd.join("CVS").join("Template"))
        .ok()
        .map(|bytes| codec::detect(&bytes).decode(&bytes));
    let path = message_temp_path();
    let mut existing = None;
    loop {
        let initial = initial_text(template.as_deref(), existing.as_deref());
        write_initial_message(&path, &initial)?;
        let before = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        let editor = editor_command(editor_option);
        let mut parts = editor.split_whitespace();
        let program = parts.next().unwrap_or("");
        if program.is_empty() {
            let _ = std::fs::remove_file(&path);
            return Err(Error::aborted(
                "import",
                "no editor defined, must use -e or -m",
            ));
        }
        let mut warning = None;
        match std::process::Command::new(program)
            .args(parts)
            .arg(&path)
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(_) => warning = Some("cvs import: warning: editor session failed".to_string()),
            // `run_exec` failing is only a warning; the reference re-reads the unchanged
            // file and asks the a/c/e/! question (logmsg.c:302-306).
            Err(e) => warning = Some(format!("cvs import: warning: editor session failed: {}", e)),
        }

        let bytes = std::fs::read(&path)?;
        let after = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let message = message_from_edited(&codec::detect(&bytes).decode(&bytes));
        if before != after && message.is_some() {
            let _ = std::fs::remove_file(&path);
            return Ok((message, warning));
        }
        match ask_action(session) {
            EditAction::Abort => {
                let _ = std::fs::remove_file(&path);
                return Err(Error::aborted("import", "aborted by user"));
            }
            EditAction::Aborting => {
                let _ = std::fs::remove_file(&path);
                return Err(Error::aborted("import", "aborting"));
            }
            EditAction::Edit => {
                existing = message;
                continue;
            }
            EditAction::Continue | EditAction::Reuse => {
                let _ = std::fs::remove_file(&path);
                return Ok((message, warning));
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum EditAction {
    Abort,
    Aborting,
    Continue,
    Edit,
    Reuse,
}

/// The question the reference asks when the editor left the message unchanged. It goes
/// through the sink so a window does not wait on a standard input nobody can type into.
fn ask_action(session: &mut protocol::Session) -> EditAction {
    let answer = session.ask(
        "\nLog message unchanged or not specified\n\
         a)bort, c)ontinue, e)dit, !)reuse this message unchanged for remaining dirs\n\
         Action: (continue) "
            .to_string(),
    );
    match answer {
        // `getline` returning -1 (EOF or a read error) is fatal in the reference
        // (logmsg.c:343-351). A window's sink has no line to give, so it gives up too.
        None => EditAction::Aborting,
        Some(answer) => action_for(&answer),
    }
}

fn action_for(answer: &str) -> EditAction {
    match answer.trim_start().chars().next() {
        Some('a' | 'A') => EditAction::Abort,
        Some('e' | 'E') => EditAction::Edit,
        Some('!') => EditAction::Reuse,
        _ => EditAction::Continue,
    }
}

fn write_initial_message(path: &Path, initial: &str) -> Result<()> {
    let bytes = codec::system_ansi()
        .encode(initial)
        .unwrap_or_else(|_| initial.as_bytes().to_vec());
    std::fs::write(path, codec::to_platform_newlines(&bytes))?;
    Ok(())
}

fn message_temp_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    crate::core::temp::messages_dir().join(format!("cvsmsg-{}-{}.txt", std::process::id(), nanos))
}

/// Read the server's report. Besides the ordinary `M` lines, the server can group `MT`
/// responses into an `importmergecmd` block that names the conflicts and the merge command
/// to suggest; the reference prints that block in one piece when it closes. Inside the block
/// the `MT text`/`MT newline` parts are swallowed: the merged line is not a report.
fn read_import_answer(session: &mut protocol::Session) -> Result<()> {
    let mut merge = MergeHint::default();
    let mut in_merge = false;
    loop {
        match session.read_response()? {
            Response::Ok => return Ok(()),
            Response::E(text) => session.err(text),
            Response::M(text) => {
                if !in_merge {
                    session.out(text);
                }
            }
            Response::Trace(text) => {
                if text == "+importmergecmd" {
                    in_merge = true;
                    merge = MergeHint::default();
                } else if text == "-importmergecmd" {
                    in_merge = false;
                    if merge.complete() {
                        print_merge_hint(session, &merge);
                    } else {
                        session.err(
                            "cvs import: invalid server: incomplete importmergecmd tags"
                                .to_string(),
                        );
                    }
                    merge = MergeHint::default();
                } else if in_merge {
                    merge.feed(&text);
                }
            }
            // `F` asks the client to flush stderr; the sink has nothing buffered.
            Response::F(_) => {}
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

/// The fields of one `+importmergecmd` … `-importmergecmd` group. `None` means the server
/// did not send that tag at all, which is what makes the group incomplete.
#[derive(Default)]
struct MergeHint {
    conflicts: Option<u32>,
    tag1: Option<String>,
    tag2: Option<String>,
    repository: Option<String>,
}

impl MergeHint {
    /// Take one tagged `MT` body; unknown tags (such as `text`) are ignored.
    fn feed(&mut self, text: &str) {
        let (tag, value) = match text.split_once(' ') {
            Some((tag, value)) => (tag, value),
            None => (text, ""),
        };
        match tag {
            "conflicts" => self.conflicts = value.trim().parse().ok(),
            "mergetag1" => self.tag1 = Some(value.to_string()),
            "mergetag2" => self.tag2 = Some(value.to_string()),
            "repository" => self.repository = Some(value.to_string()),
            _ => {}
        }
    }

    fn complete(&self) -> bool {
        // `handle_mt` checks for a zero count and for NULL tags, so an empty tag is still
        // a tag (client.c:3177-3185).
        self.conflicts != Some(0)
            && self.conflicts.is_some()
            && self.tag1.is_some()
            && self.tag2.is_some()
            && self.repository.is_some()
    }
}

fn print_merge_hint(session: &mut protocol::Session, merge: &MergeHint) {
    let (Some(conflicts), Some(tag1), Some(tag2), Some(repository)) = (
        merge.conflicts,
        merge.tag1.as_deref(),
        merge.tag2.as_deref(),
        merge.repository.as_deref(),
    ) else {
        return;
    };
    // `-d` is printed only when the root came from the command line (client.c:3213-3231).
    let mut command = format!("\t{}", program_name());
    if root_from_command_line() {
        command.push_str(" -d ");
        command.push_str(session.root.display());
    }
    command.push_str(" checkout -j");
    command.push_str(tag1);
    command.push_str(" -j");
    command.push_str(tag2);
    command.push(' ');
    command.push_str(repository);

    session.out(String::new());
    session.out(format!("{} conflicts created by this import.", conflicts));
    session.out("Use the following command to help the merge:".to_string());
    session.out(String::new());
    session.out(command);
    session.out(String::new());
}

fn check_cvs_reserved(module: &str) -> Result<()> {
    let reserved = module.split(['/', '\\']).any(|part| part == "CVS");
    if reserved {
        return Err(Error::new(
            "cvs import: The word `CVS' is reserved by CVS and may not be used\n\
             cvs [import aborted]: as a directory in a path or as a file name.",
        ));
    }
    Ok(())
}

fn check_module_path(module: &str) -> Result<()> {
    let bytes = module.as_bytes();
    let drive_absolute = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\');
    let absolute = module.starts_with(['/', '\\']) || drive_absolute;
    if absolute || pathname_levels(module) > 0 {
        return Err(Error::aborted(
            "import",
            format!("directory {} not relative within the repository", module),
        ));
    }
    Ok(())
}

/// How far a path climbs above its starting point, the way `pathname_levels` counts. A path
/// such as `a/../b` never climbs, so the reference accepts it.
fn pathname_levels(name: &str) -> usize {
    let mut level: isize = 0;
    let mut deepest: isize = 0;
    for part in name.split(['/', '\\']) {
        match part {
            ".." => {
                level -= 1;
                if level < deepest {
                    deepest = level;
                }
            }
            "." | "" => {}
            _ => level += 1,
        }
    }
    (-deepest) as usize
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

/// The rule RCS applies: an ASCII letter first, visible characters only, and none of
/// `$,.:;@`.
fn check_tag(tag: &str) -> Result<()> {
    let mut chars = tag.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn options(values: &[&str]) -> Options {
        match parse_args(&args(values)).unwrap() {
            Parsed::Options(opts) => opts,
            Parsed::Usage => panic!("expected options"),
        }
    }

    #[test]
    fn clustered_flags_are_split_like_getopt() {
        let opts = options(&["-dm", "a message", "mod", "vendor", "start"]);
        assert!(opts.dated);
        assert_eq!(opts.message.as_deref(), Some("a message"));

        let opts = options(&["-dIx", "mod", "vendor", "start"]);
        assert!(opts.dated);
        assert_eq!(opts.ignore, vec!["x".to_string()]);
    }

    #[test]
    fn the_default_branch_is_one_one_one() {
        assert_eq!(options(&["mod", "vendor", "start"]).branch, DEFAULT_BRANCH);
        assert_eq!(
            options(&["-b", "1.2.3", "mod", "vendor", "start"]).branch,
            "1.2.3"
        );
    }

    #[test]
    fn an_empty_message_is_accepted() {
        let opts = options(&["-m", "", "mod", "vendor", "start"]);
        assert_eq!(opts.message.as_deref(), Some(""));
        assert_eq!(message_with_newline(""), "\n");
    }

    #[test]
    fn an_invalid_keyword_mode_is_refused_before_the_wire() {
        let error = parse_args(&args(&["-k", "nonsense", "mod", "vendor", "start"])).unwrap_err();
        assert!(
            error.text().contains("invalid RCS keyword expansion mode"),
            "{}",
            error
        );
        assert!(
            error.text().starts_with("cvs import:"),
            "the reference reports a usage error, not an abort: {}",
            error
        );
        assert!(parse_args(&args(&["-kx", "mod", "vendor", "start"])).is_err());
        for mode in KEYWORDS {
            assert!(
                parse_args(&args(&["-k", mode, "mod", "vendor", "start"])).is_ok(),
                "{} is a valid RCS mode",
                mode
            );
        }
    }

    #[test]
    fn an_attached_option_value_may_hold_non_ascii_text() {
        let opts = options(&["-m中文说明", "mod", "vendor", "start"]);
        assert_eq!(opts.message.as_deref(), Some("中文说明"));
        let opts = options(&["-m", "第一行\n第二行", "mod", "vendor", "start"]);
        assert_eq!(opts.message.as_deref(), Some("第一行\n第二行"));
    }

    #[test]
    fn a_command_level_quiet_switch_is_refused_as_aborted() {
        let error = parse_args(&args(&["-q", "mod", "vendor", "start"])).unwrap_err();
        assert!(
            error.text().starts_with("cvs [import aborted]:"),
            "the reference calls error(1), which prints `aborted`: {}",
            error
        );
        let error = parse_args(&args(&["-Qd", "mod", "vendor", "start"])).unwrap_err();
        assert!(error.text().contains("-q or -Q must be specified"));
    }

    #[test]
    fn the_reserved_cvs_word_is_reported_on_two_lines() {
        let error = check_cvs_reserved("a/CVS/b").unwrap_err();
        assert_eq!(
            error.text(),
            "cvs import: The word `CVS' is reserved by CVS and may not be used\n\
             cvs [import aborted]: as a directory in a path or as a file name."
        );
        assert!(check_cvs_reserved("a/CVSX/b").is_ok());
    }

    #[test]
    fn names_after_the_first_operand_are_not_options() {
        let opts = options(&["mod", "vendor", "start", "-d"]);
        assert!(!opts.dated);
        assert_eq!(opts.names, vec!["mod", "vendor", "start", "-d"]);
    }

    #[test]
    fn a_module_path_may_climb_back_to_its_start() {
        assert!(check_module_path("a/../b").is_ok());
        assert!(check_module_path("../a").is_err());
        assert!(check_module_path("a/../../b").is_err());
        assert!(check_module_path("/abs").is_err());
        assert!(check_module_path(r"C:\abs").is_err());
    }

    #[test]
    fn a_tag_must_start_with_an_ascii_letter() {
        assert!(check_tag("R1").is_ok());
        assert!(check_tag("é1").is_err());
        assert!(check_tag("1abc").is_err());
    }

    #[test]
    fn a_wrapper_spec_is_sent_with_its_keyword_mode() {
        let mut list = Vec::new();
        add_wrapper_line(&mut list, "*.bin -k 'b'").unwrap();
        add_wrapper_line(&mut list, "*.txt -k 'kv'").unwrap();
        add_wrapper_line(&mut list, "*.log").unwrap();
        assert_eq!(list[0].argument(), "*.bin -k 'b'");
        assert_eq!(list[0].rcs_option().as_deref(), Some("-kb"));
        assert_eq!(list[1].argument(), "*.txt -k 'kv'");
        assert_eq!(list[1].rcs_option(), None);
        assert_eq!(list[2].argument(), "*.log -k 'kv'");
    }

    #[test]
    fn a_filter_wrapper_is_refused_like_the_reference() {
        let mut list = Vec::new();
        let error = add_wrapper_line(&mut list, "*.tar -t 'gunzip'").unwrap_err();
        assert!(error.text().contains("-t/-f wrappers"), "{}", error);
    }

    #[test]
    fn the_first_matching_wrapper_decides_even_without_a_keyword() {
        let wrappers = Wrappers {
            permanent: vec![
                Wrapper {
                    pattern: "*.dat".to_string(),
                    rcs: None,
                    copy: false,
                },
                Wrapper {
                    pattern: "*".to_string(),
                    rcs: Some("b".to_string()),
                    copy: false,
                },
            ],
            temporary: Vec::new(),
        };
        assert_eq!(wrappers.rcs_option("one.dat"), None);
        assert_eq!(wrappers.rcs_option("one.txt").as_deref(), Some("-kb"));
    }

    #[test]
    fn an_edited_message_drops_the_instruction_lines() {
        let text = "bug 1\nCVS: Enter Log.\nCVS: -----\nsecond line\n";
        assert_eq!(
            message_from_edited(text).as_deref(),
            Some("bug 1\nsecond line\n")
        );
        assert_eq!(message_from_edited("CVS: only instructions\n"), None);
        assert_eq!(message_from_edited("\n"), None);
    }

    #[test]
    fn the_merge_hint_keeps_the_fields_and_ignores_text_parts() {
        let mut hint = MergeHint::default();
        hint.feed("conflicts 3");
        hint.feed("text  conflicts created by this import.");
        hint.feed("mergetag1 1.1");
        hint.feed("mergetag2 1.1.1.1");
        hint.feed("repository /repo/mod");
        assert!(hint.complete());
        assert_eq!(hint.conflicts, Some(3));
        assert_eq!(hint.tag1.as_deref(), Some("1.1"));
        assert_eq!(hint.tag2.as_deref(), Some("1.1.1.1"));
        assert_eq!(hint.repository.as_deref(), Some("/repo/mod"));
    }

    #[test]
    fn an_incomplete_merge_hint_is_not_printed() {
        let mut hint = MergeHint::default();
        hint.feed("conflicts 0");
        assert!(!hint.complete());
        let mut hint = MergeHint::default();
        hint.feed("conflicts 2");
        hint.feed("repository /repo/mod");
        assert!(
            !hint.complete(),
            "a missing tag leaves the group incomplete"
        );
    }

    #[test]
    fn a_folders_cvswrappers_replace_its_parents_temporary_entries() {
        let root = std::env::temp_dir().join(format!(
            "cvsdesk-import-wrapper-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let child = root.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(root.join(".cvswrappers"), "*.bin -k 'b'\n").unwrap();
        std::fs::write(child.join(".cvswrappers"), "*.dat -k 'o'\n").unwrap();

        let wrappers = Wrappers::default().enter(&root).unwrap();
        assert_eq!(wrappers.rcs_option("one.bin").as_deref(), Some("-kb"));
        let below = wrappers.enter(&child).unwrap();
        assert_eq!(below.rcs_option("two.dat").as_deref(), Some("-ko"));
        assert_eq!(
            below.rcs_option("two.bin"),
            None,
            "the parent's temporary wrappers do not reach the child"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_unchanged_message_answers_are_read_like_the_reference() {
        assert_eq!(action_for("a"), EditAction::Abort);
        assert_eq!(action_for("E"), EditAction::Edit);
        assert_eq!(action_for("!"), EditAction::Reuse);
        assert_eq!(action_for(""), EditAction::Continue);
        assert_eq!(action_for("whatever"), EditAction::Continue);
    }

    #[test]
    fn the_editor_starts_with_the_message_above_the_template() {
        let text = initial_text(Some("template line\n"), Some("my message"));
        assert!(text.starts_with("my message\ntemplate line\n"), "{}", text);
        assert!(text.contains("CVS: Enter Log."), "{}", text);
    }
}
