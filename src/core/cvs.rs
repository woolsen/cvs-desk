//! Running one cvs command and building its arguments.
//!
//! The window calls the `cvs` client in its own process through [`engine::run`]; no other
//! cvs program is started. One command runs at a time (a second request is refused), its
//! output is piped into the bottom pane, and the run ends with the banner
//! "***** CVS exited normally with code N *****".

use std::path::Path;

use crate::client::engine::{self, CancelToken, Request, Sink};

/// One row of the command table (command ids and keys).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Cmd {
    Update,
    QueryUpdate,
    Commit,
    Add,
    AddBinary,
    AddUnicode,
    Remove,
    Diff,
    Log,
    Status,
    Annotate,
    Tag,
    Rtag,
    Checkout,
    Import,
    Init,
    Edit,
    Unedit,
    Editors,
    Watchers,
    WatchOn,
    WatchOff,
    Login,
    Logout,
    Export,
    Admin,
    History,
    Rdiff,
}

impl Default for Cmd {
    fn default() -> Self {
        Cmd::Status
    }
}

impl Cmd {
    fn name(&self) -> &'static str {
        match self {
            Cmd::Update => "update",
            Cmd::QueryUpdate => "update",
            Cmd::Commit => "commit",
            Cmd::Add | Cmd::AddBinary | Cmd::AddUnicode => "add",
            Cmd::Remove => "remove",
            Cmd::Diff => "diff",
            Cmd::Log => "log",
            Cmd::Status => "status",
            Cmd::Annotate => "annotate",
            Cmd::Tag => "tag",
            Cmd::Rtag => "rtag",
            Cmd::Checkout => "checkout",
            Cmd::Import => "import",
            Cmd::Init => "init",
            Cmd::Edit => "edit",
            Cmd::Unedit => "unedit",
            Cmd::Editors => "editors",
            Cmd::Watchers => "watchers",
            Cmd::WatchOn | Cmd::WatchOff => "watch",
            Cmd::Login => "login",
            Cmd::Logout => "logout",
            Cmd::Export => "export",
            Cmd::Admin => "admin",
            Cmd::History => "history",
            Cmd::Rdiff => "rdiff",
        }
    }

    /// Does this command write the working copy the window is showing? Update, commit,
    /// add, remove and the commands that edit CVS/Entries change files or recorded
    /// states, so the list must be read again afterwards. Log, status, diff, annotate
    /// and the other queries only read, so they must not redraw the folder.
    pub fn changes_working_copy(&self) -> bool {
        matches!(
            self,
            Cmd::Update
                | Cmd::Commit
                | Cmd::Add
                | Cmd::AddBinary
                | Cmd::AddUnicode
                | Cmd::Remove
                | Cmd::Tag
                | Cmd::Checkout
                | Cmd::Export
                | Cmd::Edit
                | Cmd::Unedit
        )
    }

    /// What the menu says.
    pub fn menu(&self) -> &'static str {
        match self {
            Cmd::Update => "Update",
            Cmd::QueryUpdate => "Query Update",
            Cmd::Commit => "Commit…",
            Cmd::Add => "Add",
            Cmd::AddBinary => "Add Binary",
            Cmd::AddUnicode => "Add Unicode",
            Cmd::Remove => "Remove",
            Cmd::Diff => "Diff",
            Cmd::Log => "Log",
            Cmd::Status => "Status",
            Cmd::Annotate => "Annotate",
            Cmd::Tag => "Create Tag…",
            Cmd::Rtag => "Create Rtag…",
            Cmd::Checkout => "Checkout…",
            Cmd::Import => "Import…",
            Cmd::Init => "Init",
            Cmd::Edit => "Edit",
            Cmd::Unedit => "Unedit",
            Cmd::Editors => "Editors",
            Cmd::Watchers => "Watchers",
            Cmd::WatchOn => "Watch On",
            Cmd::WatchOff => "Watch Off",
            Cmd::Login => "Login…",
            Cmd::Logout => "Logout",
            Cmd::Export => "Export…",
            Cmd::Admin => "Admin…",
            Cmd::History => "Repository History…",
            Cmd::Rdiff => "Patch (rdiff)",
        }
    }
}

/// Everything the command dialogs collect before a command is built.
#[derive(Clone, Default, Debug)]
pub struct CommandRequest {
    pub cmd: Cmd,
    pub files: Vec<String>,
    pub message: String,
    pub tag: String,
    pub branch: bool,
    /// `tag -d`: remove the tag instead of creating it. Kept apart from `extra` so the
    /// switch stays before the tag name, where cvs parses it as an option.
    pub delete: bool,
    pub force: bool,
    pub message_file: String,
    pub revisions: String,
    pub query: bool,
    pub readonly: bool,
    /// `-P`: prune empty folders. It is a command switch, not a global one, so it goes
    /// after `update` or `checkout` where the client accepts it.
    pub prune: bool,
    /// Extra switches a dialog added (diff options and the like).
    pub extra: Vec<String>,
    /// The root this one command runs against. `None` means the folder's own `CVS/Root`;
    /// Checkout, Import and Login name one when there is no working copy to read it from.
    pub root: Option<String>,
    /// Text for the child's standard input. `login` uses it so the password never appears
    /// on the command line. `None` closes its input.
    pub password: Option<String>,
    /// Read the folder again when the command ends even though its first word names a
    /// command that normally only reads. The Command Line dialog sets it for a line that
    /// may write to the working copy.
    pub refresh: bool,
    /// The words after the program name when the caller typed a whole command line
    /// (the Command Line dialog). `Some` makes [`Self::to_args`] use them as they are,
    /// so every command the client implements is reachable, not only the ones the
    /// window has a dialog for.
    pub raw_args: Option<Vec<String>>,
}

impl CommandRequest {
    pub fn new(cmd: Cmd) -> Self {
        Self {
            cmd,
            ..Default::default()
        }
    }

    /// A whole command line typed by the user: the words after the program name, sent to
    /// the client exactly as typed. The structured fields (`cmd`, `files`, ...) stay empty.
    pub fn raw(args: Vec<String>) -> Self {
        Self {
            raw_args: Some(args),
            ..Default::default()
        }
    }

    /// The arguments after the program name, exactly as the client should receive them.
    /// Nothing here is quoted: each word is one argument.
    pub fn to_args(&self, root: &str, globals: &[String]) -> Vec<String> {
        let mut args = Vec::new();
        if !root.trim().is_empty() {
            args.push("-d".into());
            args.push(root.trim().to_string());
        }
        args.extend(globals.iter().cloned());
        if let Some(raw) = &self.raw_args {
            args.extend(raw.iter().cloned());
            return args;
        }
        args.push(self.cmd.name().to_string());

        // `-P` is an update/checkout switch. Putting it in the global options made a
        // stricter client reject the whole command.
        if self.prune && matches!(self.cmd, Cmd::Update | Cmd::QueryUpdate | Cmd::Checkout) {
            args.push("-P".into());
        }

        match self.cmd {
            Cmd::QueryUpdate => args.push("-n".into()),
            Cmd::AddBinary => args.push("-kb".into()),
            Cmd::AddUnicode => args.push("-ku".into()),
            Cmd::Commit => {
                if !self.message.trim().is_empty() {
                    args.push("-m".into());
                    args.push(self.message.trim().to_string());
                } else if !self.message_file.trim().is_empty() {
                    args.push("-F".into());
                    args.push(self.message_file.trim().to_string());
                }
                if self.force {
                    args.push("-f".into());
                }
            }
            Cmd::Tag | Cmd::Rtag => {
                if self.branch {
                    args.push("-b".into());
                }
                if self.delete {
                    args.push("-d".into());
                }
                if self.force {
                    args.push("-F".into());
                }
                if !self.tag.trim().is_empty() {
                    args.push(self.tag.trim().to_string());
                }
            }
            Cmd::Log => {
                if self.query {
                    args.push("-N".into());
                }
                if !self.revisions.trim().is_empty() {
                    args.push(format!("-r{}", self.revisions.trim()));
                }
            }
            Cmd::Diff => {
                // Switches come from the Diff settings dialog (P_UnifiedDiff etc).
            }
            Cmd::WatchOn => args.push("on".into()),
            Cmd::WatchOff => args.push("off".into()),
            Cmd::Checkout => {
                if self.readonly {
                    args.push("-r".into());
                }
                args.push("-d".into());
                if !self.tag.trim().is_empty() {
                    args.push(self.tag.trim().to_string());
                }
            }
            _ => {}
        }

        // Dialog switches (diff options and the like) go after the command's own.
        args.extend(self.extra.iter().cloned());

        args.extend(self.files.iter().cloned());
        args
    }

    /// The command line printed in the console. It quotes words for reading only;
    /// [`Self::to_args`] is what actually runs.
    pub fn to_line(&self, program: &str, root: &str, globals: &[String]) -> String {
        let mut parts = vec![program.to_string()];
        parts.extend(self.to_args(root, globals).into_iter().map(|a| quote(&a)));
        parts.join(" ")
    }
}

fn quote(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".into();
    }
    if s.contains(' ') || s.contains('&') || s.contains('(') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Messages the console pane receives while a command runs.
pub enum Msg {
    Line(String),
    Done(i32),
}

pub struct Runner {
    pub lines: Vec<String>,
    pub running: bool,
    cancel: CancelToken,
}

impl Default for Runner {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            running: false,
            cancel: CancelToken::default(),
        }
    }
}

impl Runner {
    /// Start one command in `dir`, reporting through `on_msg` on a worker thread. Only one
    /// command runs at a time; a second call while running is refused.
    ///
    /// `args` are the words after the program name, already split. `line` is the same
    /// command as the console shows it. `password` goes to `login`, never to the line.
    pub fn start<F>(
        &mut self,
        dir: &Path,
        args: Vec<String>,
        password: Option<String>,
        line: String,
        on_msg: F,
    ) -> bool
    where
        F: Fn(Msg) + Send + 'static,
    {
        if self.running {
            self.lines
                .push("Error : cvs is already running !".to_string());
            return false;
        }
        if args.is_empty() {
            self.lines.push("Error : empty command line".into());
            return false;
        }
        self.cancel = CancelToken::default();
        self.lines.push(format!("$ {}", line));
        self.running = true;

        let dir = dir.to_path_buf();
        let cancel = self.cancel.clone();
        std::thread::spawn(move || {
            let mut sink = ChannelSink { on_msg };
            let request = Request {
                cwd: dir,
                args,
                password,
            };
            match engine::run(request, &mut sink, &cancel) {
                Ok(code) => sink.send(Msg::Done(code)),
                Err(error) => {
                    // A stop is reported by the window as "Stopped."; printing the
                    // interrupt would make it look like a failure.
                    if !cancel.is_cancelled() {
                        sink.lines(error.to_string());
                    }
                    sink.send(Msg::Done(1));
                }
            }
        });
        true
    }

    pub fn stop(&mut self) {
        self.cancel.cancel();
        // `running` stays true until the worker reports Done: a second command must not
        // start while the old one is still unwinding.
    }

    pub fn finish(&mut self, code: i32) {
        self.running = false;
        self.lines.push(String::new());
        self.lines.push(format!(
            "***** CVS exited normally with code {} *****",
            code
        ));
        self.lines.push(String::new());
        if self.lines.len() > 5000 {
            let drop = self.lines.len() - 5000;
            self.lines.drain(0..drop);
        }
    }
}

/// The window's sink: every line becomes a message on the runner's channel.
struct ChannelSink<F: Fn(Msg)> {
    on_msg: F,
}

impl<F: Fn(Msg)> ChannelSink<F> {
    fn send(&self, msg: Msg) {
        (self.on_msg)(msg);
    }

    fn lines(&self, text: String) {
        for line in text.lines() {
            self.send(Msg::Line(line.to_string()));
        }
    }
}

impl<F: Fn(Msg)> Sink for ChannelSink<F> {
    fn out(&mut self, line: String) {
        self.lines(line);
    }

    fn err(&mut self, line: String) {
        self.lines(line);
    }

    fn prompt(&mut self, text: String) {
        self.lines(text);
    }

    fn secret(&mut self, _text: String) -> Option<String> {
        // The window has no terminal to type into; `transport` turns this into a message
        // that names `cvs login`.
        None
    }
}

/// Split a command line into arguments, honouring double quotes.
pub fn split_args(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut any = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                any = true;
            }
            c if c.is_whitespace() && !in_quote => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if in_quote {
        return None;
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// The command line cvs is actually given for the watch-related commands. The server
    /// only knows `watch` and `watchers`; `status` with an `-a` switch is not one of its
    /// commands and is refused before anything is sent.
    fn line_for(cmd: Cmd, files: &[&str]) -> String {
        let mut req = CommandRequest::new(cmd);
        req.files = files.iter().map(|f| f.to_string()).collect();
        req.to_line(
            "cvs.exe",
            ":pserver:user@host:/repo",
            &["-q".to_string(), "-z9".to_string()],
        )
    }

    #[test]
    fn a_tag_delete_puts_the_switch_before_the_name() {
        let mut req = CommandRequest::new(Cmd::Tag);
        req.tag = "Release_1_0".to_string();
        req.delete = true;
        assert_eq!(
            req.to_line("cvs.exe", "", &[]),
            "cvs.exe tag -d Release_1_0"
        );
    }

    #[test]
    fn a_tag_create_keeps_the_scope_files_after_the_name() {
        let mut req = CommandRequest::new(Cmd::Tag);
        req.tag = "Release_1_0".to_string();
        req.files = vec!["a.txt".to_string(), "sub".to_string()];
        assert_eq!(
            req.to_line("cvs.exe", "", &[]),
            "cvs.exe tag Release_1_0 a.txt sub"
        );

        req.branch = true;
        assert_eq!(
            req.to_line("cvs.exe", "", &[]),
            "cvs.exe tag -b Release_1_0 a.txt sub"
        );
    }

    #[test]
    fn an_rtag_line_keeps_the_arguments_typed_after_the_command() {
        let mut req = CommandRequest::new(Cmd::Rtag);
        req.extra = vec!["-d".to_string(), "T1".to_string(), "mod".to_string()];
        assert_eq!(req.to_line("cvs.exe", "", &[]), "cvs.exe rtag -d T1 mod");
    }

    #[test]
    fn the_arguments_are_not_quoted_and_keep_their_spaces() {
        let mut req = CommandRequest::new(Cmd::Commit);
        req.message = "first drop".to_string();
        req.files = vec!["a b.txt".to_string()];
        assert_eq!(
            req.to_args("", &[]),
            vec!["commit", "-m", "first drop", "a b.txt"]
        );
    }

    #[test]
    fn the_root_and_the_global_switches_come_before_the_command() {
        let mut req = CommandRequest::new(Cmd::Watchers);
        req.files = vec!["a.txt".to_string()];
        assert_eq!(
            req.to_args(
                ":pserver:user@host:/repo",
                &["-q".to_string(), "-z9".to_string()]
            ),
            vec![
                "-d",
                ":pserver:user@host:/repo",
                "-q",
                "-z9",
                "watchers",
                "a.txt"
            ]
        );
    }

    #[test]
    fn raw_words_come_after_the_root_and_the_global_switches() {
        let req = CommandRequest::raw(vec![
            "admin".to_string(),
            "-kb".to_string(),
            "a.txt".to_string(),
        ]);
        assert_eq!(
            req.to_args(":pserver:user@host:/repo", &["-q".to_string()]),
            vec![
                "-d",
                ":pserver:user@host:/repo",
                "-q",
                "admin",
                "-kb",
                "a.txt"
            ]
        );
    }

    #[test]
    fn prune_goes_after_the_command_and_only_where_it_belongs() {
        let mut update = CommandRequest::new(Cmd::Update);
        update.prune = true;
        assert_eq!(update.to_args("", &[]), vec!["update", "-P"]);

        let mut query = CommandRequest::new(Cmd::QueryUpdate);
        query.prune = true;
        assert_eq!(query.to_args("", &[]), vec!["update", "-P", "-n"]);

        let mut checkout = CommandRequest::new(Cmd::Checkout);
        checkout.prune = true;
        assert_eq!(checkout.to_args("", &[]), vec!["checkout", "-P", "-d"]);

        let mut status = CommandRequest::new(Cmd::Status);
        status.prune = true;
        assert_eq!(
            status.to_args("", &[]),
            vec!["status"],
            "-P is not a status switch"
        );
    }

    #[test]
    fn the_watch_commands_are_named_the_way_cvs_expects() {
        assert_eq!(
            line_for(Cmd::Watchers, &["a.txt"]),
            "cvs.exe -d :pserver:user@host:/repo -q -z9 watchers a.txt"
        );
        assert_eq!(
            line_for(Cmd::WatchOn, &["a.txt"]),
            "cvs.exe -d :pserver:user@host:/repo -q -z9 watch on a.txt"
        );
        assert_eq!(
            line_for(Cmd::WatchOff, &["a.txt"]),
            "cvs.exe -d :pserver:user@host:/repo -q -z9 watch off a.txt"
        );
        assert_eq!(
            line_for(Cmd::Editors, &["a.txt"]),
            "cvs.exe -d :pserver:user@host:/repo -q -z9 editors a.txt"
        );
        assert_eq!(
            line_for(Cmd::Unedit, &["a.txt"]),
            "cvs.exe -d :pserver:user@host:/repo -q -z9 unedit a.txt"
        );

        let mut edit = CommandRequest::new(Cmd::Edit);
        edit.files = vec!["a.txt".to_string()];
        edit.extra = vec!["-c".to_string()];
        assert_eq!(
            edit.to_line("cvs.exe", ":pserver:user@host:/repo", &[]),
            "cvs.exe -d :pserver:user@host:/repo edit -c a.txt"
        );
    }

    fn collect(rx: mpsc::Receiver<Msg>) -> (Vec<String>, i32) {
        let mut lines = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Msg::Line(line)) => lines.push(line),
                Ok(Msg::Done(code)) => return (lines, code),
                Err(error) => panic!("the command did not finish: {}", error),
            }
        }
    }

    #[test]
    fn a_command_reports_the_client_output_and_code() {
        let (tx, rx) = mpsc::channel();
        let mut runner = Runner::default();
        assert!(runner.start(
            &std::env::temp_dir(),
            vec!["--version".to_string()],
            None,
            "cvs --version".to_string(),
            move |m| {
                let _ = tx.send(m);
            },
        ));
        assert_eq!(runner.lines, vec!["$ cvs --version"]);
        let (lines, code) = collect(rx);
        assert_eq!(code, 0);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Concurrent Versions System")),
            "{:?}",
            lines
        );
    }

    #[test]
    fn an_error_is_a_line_then_a_nonzero_code() {
        let (tx, rx) = mpsc::channel();
        let mut runner = Runner::default();
        assert!(runner.start(
            &std::env::temp_dir(),
            vec![
                "-d".to_string(),
                ":local:/repo".to_string(),
                "noop".to_string()
            ],
            None,
            "cvs -d :local:/repo noop".to_string(),
            move |m| {
                let _ = tx.send(m);
            },
        ));
        let (lines, code) = collect(rx);
        assert_eq!(code, 1);
        assert!(
            lines.iter().any(|line| line.contains("not supported")),
            "{:?}",
            lines
        );
    }
}
