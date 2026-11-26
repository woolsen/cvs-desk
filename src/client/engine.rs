//! Running one CVS command, wherever the request came from.
//!
//! The `cvs` program builds a [`Request`] from its own command line and calls [`run`]; the
//! window calls the same function in its own process. Everything a command or the server
//! shows goes through the [`Sink`], so the caller decides where the text lands. Questions
//! go through the sink too: a console sink reads the answer, a window sink answers "no"
//! or reports that it has no password, instead of reading the process's stdin.
//!
//! The engine stays window-agnostic: no channels, no `egui`, and no working-folder change.
//! [`Request::cwd`] is the folder the command was started in, exactly the one the command
//! line would have used.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::client::cli::{self, Invocation};
use crate::client::commands;
use crate::client::error::{Error, Result};
use crate::client::passfile;
use crate::client::protocol;
use crate::client::root::{self, Method};
use crate::client::transport;

/// One command to run.
#[derive(Clone)]
pub struct Request {
    /// The folder the command was started in.
    pub cwd: PathBuf,
    /// The arguments after the program name, e.g. `["-d", root, "status", "-q", "a.txt"]`.
    pub args: Vec<String>,
    /// A password for `login` when the caller already has one; `None` lets `login` ask.
    pub password: Option<String>,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("cwd", &self.cwd)
            .field("args", &self.args)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Where a command's output goes, without the line ending. A call may carry one line or a
/// multi-line block (the help and version text), so a sink must not assume one line.
pub trait Sink {
    /// Text that belongs on the caller's stdout (`M` responses, command reports, help).
    fn out(&mut self, line: String);
    /// Text that belongs on the caller's stderr (`E` responses, warnings, trace lines).
    fn err(&mut self, line: String);
    /// Text that must stay on the same line as what follows, such as a question whose
    /// answer is typed next. A sink that only records lines may treat it as ordinary output.
    fn prompt(&mut self, text: String) {
        self.out(text);
    }
    /// Ask a question whose answer is one line. A sink with no terminal returns an empty
    /// answer, which every caller reads as "no".
    fn ask(&mut self, text: String) -> String {
        self.prompt(text);
        String::new()
    }
    /// Ask a yes/no question. Only an answer starting with `y` or `Y` is yes.
    fn confirm(&mut self, text: String) -> bool {
        matches!(self.ask(text).trim_start().chars().next(), Some('y' | 'Y'))
    }
    /// Ask for a password. `None` means this caller cannot ask; the command then fails
    /// with a clear message rather than reading the process's stdin.
    fn secret(&mut self, text: String) -> Option<String> {
        self.prompt(text);
        None
    }
}

/// A flag that asks a running command to stop.
///
/// Clones share the flag and the registered actions, so the caller can keep one and hand
/// another to the command. The transport registers how to break a blocked read or a child
/// process; [`cancel`](Self::cancel) sets the flag and runs those actions once.
#[derive(Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    actions: Arc<Mutex<Vec<Action>>>,
}

type Action = Box<dyn Fn() + Send + Sync + 'static>;

impl fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl CancelToken {
    /// Ask the running command to stop. Safe to call more than once: each registered action
    /// runs at most once.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let actions = {
            let mut actions = self
                .actions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *actions)
        };
        for action in actions {
            action();
        }
    }

    /// Register how to interrupt a command, for example a socket shutdown or a child kill.
    /// Registering after [`cancel`](Self::cancel) runs the action straight away.
    pub fn register(&self, action: impl Fn() + Send + Sync + 'static) {
        let mut actions = self
            .actions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.cancelled.load(Ordering::SeqCst) {
            drop(actions);
            action();
            return;
        }
        actions.push(Box::new(action));
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Run one request, writing everything the command shows to `sink`.
pub fn run(req: Request, sink: &mut dyn Sink, cancel: &CancelToken) -> Result<i32> {
    if cancel.is_cancelled() {
        return Err(Error::interrupted("cvs"));
    }
    let (opts, invocation) = cli::parse_args(&req.args)?;
    let name = invocation_name(&invocation).to_string();
    let result = match invocation {
        Invocation::Version => {
            sink.out(cli::banner());
            Ok(0)
        }
        Invocation::Help | Invocation::None => {
            sink.out(cli::general_help());
            Ok(1)
        }
        Invocation::HelpCommands => {
            sink.out(cli::help_commands());
            Ok(1)
        }
        Invocation::HelpOptions => {
            sink.out("CVS global options (specified before the command name) are:".to_string());
            sink.out(cli::global_options_help().to_string());
            Ok(1)
        }
        Invocation::HelpSynonyms => {
            sink.out(cli::help_synonyms());
            Ok(1)
        }
        Invocation::HelpCommand(name) => {
            if name.is_empty() {
                sink.out(cli::general_help());
                return Ok(1);
            }
            match cli::command_help(&name) {
                Some(text) => {
                    sink.out(text.to_string());
                    Ok(1)
                }
                None => Err(Error::new(format!("cvs: unknown command: `{}'", name))),
            }
        }
        Invocation::Run { name, args } => run_command(&req, opts, &name, &args, cancel, sink),
    };
    // The command can return while the token is being set, or return success after the
    // server answered a cancelled request. The stop wins either way.
    if cancel.is_cancelled() {
        return Err(Error::interrupted(&name));
    }
    result
}

/// The name a cancellation error carries when nothing more specific is known.
fn invocation_name(invocation: &Invocation) -> &str {
    match invocation {
        Invocation::Version => "version",
        Invocation::HelpCommand(name) if !name.is_empty() => name,
        Invocation::Run { name, .. } => name,
        _ => "cvs",
    }
}

fn run_command(
    req: &Request,
    opts: cli::Options,
    name: &str,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let cwd = req.cwd.as_path();
    match cli::canonical(name).as_str() {
        "version" => {
            let root = resolve_root(&opts, cwd, "version")?;
            let mut session = protocol::open(root, "version", cancel, sink)?;
            let server = session.version()?;
            session.close();
            sink.out(format!(
                "Client: {}",
                cli::banner().lines().next().unwrap_or("")
            ));
            sink.out(format!("Server: {}", server));
            Ok(0)
        }
        "login" => {
            let root = resolve_root(&opts, cwd, "login")?;
            require_method(&root, "login", Method::Pserver)?;
            let password = match &req.password {
                Some(password) => password.clone(),
                None => sink
                    .secret("CVS password: ".to_string())
                    .ok_or_else(|| Error::aborted("login", "no password given"))?,
            };
            // Log in the way cvs does: the server has the last word, and a refused
            // password must not replace a good entry in .cvspass.
            let connection = transport::connect_with_password(&root, &password, "login", cancel)?;
            drop(connection);
            let path = passfile::set_password(&root.key, &password)?;
            if opts.trace {
                sink.err(format!("cvs login: saved in {}", path.display()));
            }
            Ok(0)
        }
        "logout" => {
            let root = resolve_root(&opts, cwd, "logout")?;
            passfile::remove_password(&root.key)?;
            Ok(0)
        }
        "noop" => {
            let root = resolve_root(&opts, cwd, "noop")?;
            let mut session = protocol::open(root, "noop", cancel, sink)?;
            session.simple("noop")?;
            session.close();
            Ok(0)
        }
        "admin" => {
            let root = resolve_root(&opts, cwd, "admin")?;
            commands::admin::run(cwd, root, args, cancel, sink)
        }
        "checkout" => {
            let root = resolve_root(&opts, cwd, "checkout")?;
            commands::checkout::run(cwd, root, args, opts.readonly, cancel, sink)
        }
        "export" => {
            let root = resolve_root(&opts, cwd, "export")?;
            commands::export::run(cwd, root, args, opts.readonly, cancel, sink)
        }
        "status" => {
            let root = resolve_root(&opts, cwd, "status")?;
            commands::status::run(cwd, root, args, cancel, sink)
        }
        "log" => {
            let root = resolve_root(&opts, cwd, "log")?;
            commands::log::run(cwd, root, args, cancel, sink)
        }
        "rlog" => {
            let root = resolve_root(&opts, cwd, "rlog")?;
            commands::rlog::run(root, args, cancel, sink)
        }
        "rannotate" => {
            let root = resolve_root(&opts, cwd, "rannotate")?;
            commands::rannotate::run(root, args, cancel, sink)
        }
        "diff" => {
            let root = resolve_root(&opts, cwd, "diff")?;
            commands::diff::run(cwd, root, args, cancel, sink)
        }
        "annotate" => {
            let root = resolve_root(&opts, cwd, "annotate")?;
            commands::query::annotate(cwd, root, args, cancel, sink)
        }
        "edit" => {
            let root = resolve_root(&opts, cwd, "edit")?;
            commands::edit::run(cwd, root, args, cancel, sink)
        }
        "editors" => {
            let root = resolve_root(&opts, cwd, "editors")?;
            commands::query::file_query(cwd, root, "editors", args, cancel, sink)
        }
        "watch" => {
            let root = resolve_root(&opts, cwd, "watch")?;
            commands::watch::run(cwd, root, args, cancel, sink)
        }
        "watchers" => {
            let root = resolve_root(&opts, cwd, "watchers")?;
            commands::query::file_query(cwd, root, "watchers", args, cancel, sink)
        }
        "unedit" => {
            let root = resolve_root(&opts, cwd, "unedit")?;
            commands::unedit::run(cwd, root, args, cancel, sink)
        }
        "update" => {
            let root = resolve_root(&opts, cwd, "update")?;
            commands::update::run(cwd, root, args, opts.readonly, cancel, sink)
        }
        "commit" => {
            let root = resolve_root(&opts, cwd, "commit")?;
            commands::commit::run(cwd, root, args, opts.readonly, cancel, sink)
        }
        "add" => {
            let root = resolve_root(&opts, cwd, "add")?;
            commands::add::run(cwd, root, args, cancel, sink)
        }
        "remove" => {
            let root = resolve_root(&opts, cwd, "remove")?;
            commands::remove::run(cwd, root, args, cancel, sink)
        }
        "tag" => {
            let root = resolve_root(&opts, cwd, "tag")?;
            commands::tag::run(cwd, root, args, cancel, sink)
        }
        "rtag" => {
            let root = resolve_root(&opts, cwd, "rtag")?;
            commands::rtag::run(root, args, cancel, sink)
        }
        "init" => {
            let root = resolve_root(&opts, cwd, "init")?;
            commands::init::run(root, args, cancel, sink)
        }
        "history" => {
            let root = resolve_root(&opts, cwd, "history")?;
            commands::history::run(root, args, cancel, sink)
        }
        "import" => {
            let root = resolve_root(&opts, cwd, "import")?;
            commands::import::run(cwd, root, args, cancel, sink)
        }
        "rdiff" => {
            let root = resolve_root(&opts, cwd, "rdiff")?;
            commands::rdiff::run(root, args, cancel, sink)
        }
        "release" => {
            let root = resolve_root(&opts, cwd, "release")?;
            commands::release::run(cwd, root, args, cancel, sink)
        }
        // A name this build knows from the help but cannot run yet says so; anything else is
        // not a CVS command at all.
        other if cli::command_help(other).is_some() => Err(Error::aborted(
            other,
            "this command is not implemented by this cvs yet",
        )),
        other => Err(Error::new(format!("cvs: unknown command: `{}'", other))),
    }
}

/// Resolve the root and carry the `-z` level into the connection settings.
fn resolve_root(opts: &cli::Options, cwd: &Path, command: &str) -> Result<root::Root> {
    let (mut root, _) = root::resolve(opts.root.as_deref(), cwd, command)?;
    root.compression = opts.compression;
    Ok(root)
}

fn require_method(root: &root::Root, command: &str, method: Method) -> Result<()> {
    if root.method != method {
        return Err(Error::aborted(
            command,
            format!(
                "this command needs a :{}: root, not :{}:",
                method.token(),
                root.method.token()
            ),
        ));
    }
    Ok(())
}

/// A sink that keeps every line, in order, for tests to inspect.
#[cfg(test)]
pub(crate) mod test_sink {
    use super::Sink;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Channel {
        Out,
        Err,
    }

    #[derive(Debug, Default)]
    pub struct RecordingSink {
        /// Every line in the order it arrived, with the channel it used.
        pub lines: Vec<(Channel, String)>,
    }

    impl Sink for RecordingSink {
        fn out(&mut self, line: String) {
            self.lines.push((Channel::Out, line));
        }

        fn err(&mut self, line: String) {
            self.lines.push((Channel::Err, line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::engine::test_sink::{Channel, RecordingSink};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::{mpsc, Mutex, MutexGuard};
    use std::time::Duration;

    /// `CVS_PASSFILE` is process-wide; tests that use it take turns.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-engine-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_passfile(path: &Path, root: &str) {
        let mut line = root.as_bytes().to_vec();
        line.push(b' ');
        line.extend_from_slice(&passfile::scramble(b"secret"));
        line.push(b'\n');
        std::fs::write(path, line).unwrap();
    }

    /// Read one protocol line from the client.
    fn line(reader: &mut BufReader<TcpStream>) -> String {
        let mut text = String::new();
        reader.read_line(&mut text).unwrap();
        text.trim_end_matches(['\r', '\n']).to_string()
    }

    /// Play the server's side of the pserver handshake, then let the test answer the command.
    fn pserver<F>(after_handshake: F) -> (String, std::thread::JoinHandle<()>)
    where
        F: FnOnce(&mut BufReader<TcpStream>, &mut TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let password = String::from_utf8(passfile::scramble(b"secret")).unwrap();
            for expected in [
                "BEGIN AUTH REQUEST",
                "/repo",
                "ann",
                password.as_str(),
                "END AUTH REQUEST",
            ] {
                assert_eq!(line(&mut reader), expected);
            }
            writer.write_all(b"I LOVE YOU\n").unwrap();
            assert_eq!(line(&mut reader), "Root /repo");
            assert!(line(&mut reader).starts_with("Valid-responses "));
            assert_eq!(line(&mut reader), "valid-requests");
            writer
                .write_all(b"Valid-requests Root Valid-responses\nok\n")
                .unwrap();
            assert_eq!(line(&mut reader), "UseUnchanged");
            after_handshake(&mut reader, &mut writer);
            writer.flush().unwrap();
        });
        let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
        (root, server)
    }

    /// Play only the pserver authentication handshake, then answer with `reply`. `login`
    /// closes the connection after that, so there is no session opening to read.
    fn login_server(
        password: String,
        reply: &'static str,
    ) -> (String, std::thread::JoinHandle<()>, mpsc::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (done_tx, done_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            for expected in [
                "BEGIN AUTH REQUEST",
                "/repo",
                "ann",
                password.as_str(),
                "END AUTH REQUEST",
            ] {
                assert_eq!(line(&mut reader), expected);
            }
            writer.write_all(reply.as_bytes()).unwrap();
            writer.flush().unwrap();
            let _ = done_tx.send(());
        });
        (
            format!(":pserver:ann@127.0.0.1:{}:/repo", port),
            server,
            done_rx,
        )
    }

    #[test]
    fn version_is_written_to_the_sink() {
        let mut sink = RecordingSink::default();
        let code = run(
            Request {
                cwd: PathBuf::from("."),
                args: vec!["--version".to_string()],
                password: None,
            },
            &mut sink,
            &CancelToken::default(),
        )
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(sink.lines, vec![(Channel::Out, cli::banner())]);
    }

    #[test]
    fn a_cancelled_request_does_not_run() {
        let cancel = CancelToken::default();
        assert!(!cancel.is_cancelled());
        cancel.cancel();
        assert!(cancel.clone().is_cancelled(), "clones share the flag");

        let mut sink = RecordingSink::default();
        let error = run(
            Request {
                cwd: PathBuf::from("."),
                args: vec!["--version".to_string()],
                password: None,
            },
            &mut sink,
            &cancel,
        )
        .unwrap_err();
        assert!(error.is_interrupted(), "{}", error);
        assert!(sink.lines.is_empty());
    }

    #[test]
    fn a_registered_action_runs_once_and_immediately_after_a_cancel() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        let cancel = CancelToken::default();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        cancel.register(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let clone = cancel.clone();
        clone.cancel();
        clone.cancel();
        assert_eq!(runs.load(Ordering::SeqCst), 1, "cancel must act once");

        let late_runs = Arc::new(AtomicUsize::new(0));
        let late = late_runs.clone();
        cancel.register(move || {
            late.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(
            late_runs.load(Ordering::SeqCst),
            1,
            "a late registration runs at once"
        );
    }

    #[test]
    fn the_debug_form_of_a_request_hides_the_password() {
        let req = Request {
            cwd: PathBuf::from("."),
            args: vec!["login".to_string()],
            password: Some("secret".to_string()),
        };
        let text = format!("{:?}", req);
        assert!(text.contains("<redacted>"), "{}", text);
        assert!(!text.contains("secret"), "{}", text);

        let none = Request {
            password: None,
            ..req
        };
        assert!(!format!("{:?}", none).contains("<redacted>"));
    }

    #[test]
    fn a_verified_password_on_the_request_is_saved_without_asking() {
        let _env = lock_env();
        let dir = scratch("login-password");
        let pass_path = dir.join(".cvspass");
        std::env::set_var("CVS_PASSFILE", &pass_path);

        let scrambled = String::from_utf8(passfile::scramble(b"secret")).unwrap();
        let (root, server, done) = login_server(scrambled, "I LOVE YOU\n");
        let mut sink = RecordingSink::default();
        let code = run(
            Request {
                cwd: dir.clone(),
                args: vec!["-d".to_string(), root.clone(), "login".to_string()],
                password: Some("secret".to_string()),
            },
            &mut sink,
            &CancelToken::default(),
        )
        .unwrap();

        assert_eq!(code, 0);
        assert!(
            done.recv_timeout(Duration::from_secs(5)).is_ok(),
            "login did not contact the server"
        );
        server.join().unwrap();
        let mut expected = root.as_bytes().to_vec();
        expected.push(b' ');
        expected.extend_from_slice(&passfile::scramble(b"secret"));
        expected.push(b'\n');
        assert_eq!(std::fs::read(&pass_path).unwrap(), expected);
        assert!(sink.lines.is_empty(), "login says nothing when it works");

        std::env::remove_var("CVS_PASSFILE");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_refused_login_is_not_saved() {
        let _env = lock_env();
        let dir = scratch("login-refused");
        let pass_path = dir.join(".cvspass");
        std::env::set_var("CVS_PASSFILE", &pass_path);

        let scrambled = String::from_utf8(passfile::scramble(b"wrong")).unwrap();
        let (root, server, done) = login_server(scrambled, "I HATE YOU\n");
        let mut sink = RecordingSink::default();
        let error = run(
            Request {
                cwd: dir.clone(),
                args: vec!["-d".to_string(), root.clone(), "login".to_string()],
                password: Some("wrong".to_string()),
            },
            &mut sink,
            &CancelToken::default(),
        )
        .expect_err("a refused login must fail");

        assert!(error.text().contains("authorization failed"), "{}", error);
        assert!(
            done.recv_timeout(Duration::from_secs(5)).is_ok(),
            "login did not contact the server"
        );
        server.join().unwrap();
        assert!(!pass_path.exists(), "a refused password must not be saved");

        std::env::remove_var("CVS_PASSFILE");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn status_prints_unknown_files_and_server_text_through_the_sink() {
        let _env = lock_env();
        let (root, server) = pserver(|reader, writer| {
            let mut request = String::new();
            for _ in 0..20 {
                request = line(reader);
                if request == "status" {
                    break;
                }
            }
            assert_eq!(request, "status");
            writer
                .write_all(b"M status answer\nE a server note\nok\n")
                .unwrap();
        });

        let cwd = scratch("status-work");
        std::fs::create_dir_all(cwd.join("CVS")).unwrap();
        std::fs::write(cwd.join("CVS").join("Repository"), ".\n").unwrap();
        std::fs::write(cwd.join("a.txt"), "one\n").unwrap();
        std::fs::write(cwd.join("b.txt"), "two\n").unwrap();

        let pass_dir = scratch("status-pass");
        let pass_path = pass_dir.join(".cvspass");
        write_passfile(&pass_path, &root);
        std::env::set_var("CVS_PASSFILE", &pass_path);

        let mut sink = RecordingSink::default();
        let code = run(
            Request {
                cwd: cwd.clone(),
                args: vec!["-d".to_string(), root.clone(), "status".to_string()],
                password: None,
            },
            &mut sink,
            &CancelToken::default(),
        )
        .unwrap();

        assert_eq!(code, 0);
        assert_eq!(
            sink.lines,
            vec![
                (Channel::Out, "? a.txt".to_string()),
                (Channel::Out, "? b.txt".to_string()),
                (Channel::Out, "status answer".to_string()),
                (Channel::Err, "a server note".to_string()),
            ]
        );

        server.join().unwrap();
        std::env::remove_var("CVS_PASSFILE");
        std::fs::remove_dir_all(&cwd).ok();
        std::fs::remove_dir_all(&pass_dir).ok();
    }

    /// A workspace and passfile that let `status` reach the fake pserver.
    fn status_workspace(name: &str, root: &str) -> (PathBuf, PathBuf) {
        let cwd = scratch(&format!("{}-work", name));
        std::fs::create_dir_all(cwd.join("CVS")).unwrap();
        std::fs::write(cwd.join("CVS").join("Repository"), ".\n").unwrap();
        std::fs::write(cwd.join("a.txt"), "one\n").unwrap();

        let pass_dir = scratch(&format!("{}-pass", name));
        let pass_path = pass_dir.join(".cvspass");
        write_passfile(&pass_path, root);
        std::env::set_var("CVS_PASSFILE", &pass_path);
        (cwd, pass_dir)
    }

    /// Run `status` on a worker thread and hand back its result over a channel.
    fn run_status_in_background(
        cwd: PathBuf,
        root: String,
        cancel: CancelToken,
    ) -> (std::thread::JoinHandle<()>, mpsc::Receiver<Result<i32>>) {
        let (done_tx, done) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut sink = RecordingSink::default();
            let result = run(
                Request {
                    cwd,
                    args: vec!["-d".to_string(), root, "status".to_string()],
                    password: None,
                },
                &mut sink,
                &cancel,
            );
            let _ = done_tx.send(result);
        });
        (worker, done)
    }

    #[test]
    fn a_cancelled_command_stops_a_blocked_pserver_read() {
        let _env = lock_env();
        let (seen_tx, seen) = mpsc::channel();
        let (eof_tx, eof) = mpsc::channel();
        let (root, server) = pserver(move |reader, _writer| {
            let mut request = String::new();
            for _ in 0..20 {
                request = line(reader);
                if request == "status" {
                    break;
                }
            }
            assert_eq!(request, "status");
            seen_tx.send(()).unwrap();
            // The stop must shut the socket down, so this read sees the end of the pipe.
            let mut byte = [0u8; 1];
            let closed = matches!(reader.read(&mut byte), Ok(0) | Err(_));
            eof_tx.send(closed).unwrap();
        });

        let (cwd, pass_dir) = status_workspace("cancel-read", &root);
        let cancel = CancelToken::default();
        let (worker, done) = run_status_in_background(cwd.clone(), root.clone(), cancel.clone());

        seen.recv_timeout(Duration::from_secs(5))
            .expect("the server never saw the status command");
        cancel.cancel();
        let result = done
            .recv_timeout(Duration::from_secs(5))
            .expect("the command did not stop within 5 s");
        let error = result.unwrap_err();
        assert!(error.is_interrupted(), "{}", error);
        assert_eq!(error.text(), "cvs [status aborted]: stopped by the user");
        assert!(
            eof.recv_timeout(Duration::from_secs(5))
                .expect("the socket stayed open after the stop"),
            "the server did not see the connection close"
        );

        worker.join().unwrap();
        server.join().unwrap();
        std::env::remove_var("CVS_PASSFILE");
        std::fs::remove_dir_all(&cwd).ok();
        std::fs::remove_dir_all(&pass_dir).ok();
    }

    #[test]
    fn a_cancelled_command_stops_a_silent_pserver_login() {
        let _env = lock_env();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen_tx, seen) = mpsc::channel();
        let (eof_tx, eof) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            // Read the whole auth request, then never answer `I LOVE YOU`.
            for _ in 0..5 {
                line(&mut reader);
            }
            seen_tx.send(()).unwrap();
            let mut byte = [0u8; 1];
            let closed = matches!(reader.read(&mut byte), Ok(0) | Err(_));
            eof_tx.send(closed).unwrap();
        });
        let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);

        let (cwd, pass_dir) = status_workspace("cancel-login", &root);
        let cancel = CancelToken::default();
        let (worker, done) = run_status_in_background(cwd.clone(), root.clone(), cancel.clone());

        seen.recv_timeout(Duration::from_secs(5))
            .expect("the server never saw the authentication request");
        cancel.cancel();
        let result = done
            .recv_timeout(Duration::from_secs(5))
            .expect("the login did not stop within 5 s");
        let error = result.unwrap_err();
        assert!(error.is_interrupted(), "{}", error);
        assert_eq!(error.text(), "cvs [status aborted]: stopped by the user");
        assert!(
            eof.recv_timeout(Duration::from_secs(5))
                .expect("the socket stayed open after the stop"),
            "the server did not see the connection close"
        );

        worker.join().unwrap();
        server.join().unwrap();
        std::env::remove_var("CVS_PASSFILE");
        std::fs::remove_dir_all(&cwd).ok();
        std::fs::remove_dir_all(&pass_dir).ok();
    }

    #[test]
    fn a_cancelled_command_stops_a_silent_opening_handshake() {
        let _env = lock_env();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen_tx, seen) = mpsc::channel();
        let (eof_tx, eof) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            for _ in 0..5 {
                line(&mut reader);
            }
            writer.write_all(b"I LOVE YOU\n").unwrap();
            assert!(line(&mut reader).starts_with("Root /repo"));
            assert!(line(&mut reader).starts_with("Valid-responses "));
            assert_eq!(line(&mut reader), "valid-requests");
            seen_tx.send(()).unwrap();
            let mut byte = [0u8; 1];
            let closed = matches!(reader.read(&mut byte), Ok(0) | Err(_));
            eof_tx.send(closed).unwrap();
        });
        let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);

        let (cwd, pass_dir) = status_workspace("cancel-handshake", &root);
        let cancel = CancelToken::default();
        let (worker, done) = run_status_in_background(cwd.clone(), root.clone(), cancel.clone());

        seen.recv_timeout(Duration::from_secs(5))
            .expect("the server never saw the opening requests");
        cancel.cancel();
        let result = done
            .recv_timeout(Duration::from_secs(5))
            .expect("the command did not stop within 5 s");
        let error = result.unwrap_err();
        assert!(error.is_interrupted(), "{}", error);
        assert!(
            eof.recv_timeout(Duration::from_secs(5))
                .expect("the socket stayed open after the stop"),
            "the server did not see the connection close"
        );

        worker.join().unwrap();
        server.join().unwrap();
        std::env::remove_var("CVS_PASSFILE");
        std::fs::remove_dir_all(&cwd).ok();
        std::fs::remove_dir_all(&pass_dir).ok();
    }

    /// A sink that stops the command as soon as it sees the first line.
    struct CancellingSink {
        cancel: CancelToken,
        lines: Vec<String>,
    }

    impl Sink for CancellingSink {
        fn out(&mut self, line: String) {
            self.cancel.cancel();
            self.lines.push(line);
        }

        fn err(&mut self, line: String) {
            self.lines.push(line);
        }
    }

    #[test]
    fn a_stop_after_a_successful_command_wins_over_the_result() {
        let cancel = CancelToken::default();
        let mut sink = CancellingSink {
            cancel: cancel.clone(),
            lines: Vec::new(),
        };
        let error = run(
            Request {
                cwd: PathBuf::from("."),
                args: vec!["--version".to_string()],
                password: None,
            },
            &mut sink,
            &cancel,
        )
        .unwrap_err();
        assert!(error.is_interrupted(), "{}", error);
        assert_eq!(sink.lines, vec![cli::banner()], "the command had run");
    }
}
