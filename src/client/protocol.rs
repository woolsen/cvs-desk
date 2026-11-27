//! The CVS client/server protocol.
//!
//! A session starts with three requests — `Root`, `Valid-responses`, `valid-requests` — and
//! then the client says which optional behaviours it can handle (`UseUnchanged`, `Case`,
//! `Utf8`). Every later request is answered by a stream of responses ending in `ok` or
//! `error`.
//!
//! `Valid-responses` is a promise: a name is listed only if [`read_response`] can consume its
//! body. `Rcs-diff` and `Patched` are deliberately absent, so the server sends whole files
//! instead of patches (CVS itself behaves this way for clients that do not advertise them).
//! That costs bandwidth and saves the client from applying RCS patches to a working copy.
//!
//! The file-carrying responses all have the same shape in this server:
//!
//! ```text
//! Created <update-dir>/
//! <repository path>
//! <entry line>
//! <mode>
//! <length>
//! <length bytes>
//! ```
//!
//! The entry line names the file; the update directory plus that name is its local path.

use std::io;
use std::path::Path;

use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::root::Root;
use crate::client::transport::{self, Connection};
use crate::client::work::codec::{self, Codec};
use crate::client::work::dir;

/// The optional responses this client can consume. See the module comment for why some are
/// missing.
pub const VALID_RESPONSES: &[&str] = &[
    "ok",
    "error",
    "Valid-requests",
    "Checked-in",
    "New-entry",
    "Copy-file",
    "Updated",
    "Created",
    "Update-existing",
    "Merged",
    "Patched",
    "Mode",
    "Mod-time",
    "Removed",
    "Remove-entry",
    "Set-static-directory",
    "Clear-static-directory",
    "Set-sticky",
    "Clear-sticky",
    "Module-expansion",
    "Notified",
    "Template",
    "MT",
    "M",
    "E",
    "F",
];

/// One response from the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Ok,
    Error {
        code: String,
        message: String,
    },
    /// A message the server wants shown (stderr, like cvs does).
    E(String),
    /// A message the server wants printed (stdout).
    M(String),
    ValidRequests(Vec<String>),
    /// `Created`, `Updated`, `Merged` or `Update-existing` with its file body.
    File {
        kind: FileKind,
        update_dir: String,
        repository: String,
        entry: String,
        mode: String,
        content: Vec<u8>,
    },
    /// The server asked the client to copy a working file to a backup name.
    CopyFile {
        update_dir: String,
        repository: String,
        name: String,
    },
    Mode(String),
    ModTime(String),
    Removed(String),
    RemoveEntry(String),
    SetStaticDirectory {
        dir: String,
        repository: String,
    },
    ClearStaticDirectory {
        dir: String,
        repository: String,
    },
    SetSticky {
        dir: String,
        repository: String,
        /// The protocol's sticky marker, e.g. `TTESTTAG` (branch) or `NTESTTAG` (tag).
        tag: String,
    },
    ClearSticky {
        dir: String,
        repository: String,
    },
    ModuleExpansion(String),
    /// `Notified <dir>/` + repository: the server passed a `Notify` on to the watchers. The
    /// client has nothing left to do, so every command ignores it.
    Notified,
    /// `Checked-in <dir>/` + repository + entry: the entry's revision/tag changed, but the
    /// file's contents did not (used by `update -A`, and by `commit`'s confirmation).
    CheckedIn {
        update_dir: String,
        repository: String,
        entry: String,
    },
    /// `New-entry <dir>/` + repository/file + Entries line: a file's entry changed but its
    /// contents did not travel.
    NewEntry {
        update_dir: String,
        repository: String,
        entry: String,
    },
    F(String),
    /// A CVSNT GUI trace line (`MT …`): no output, no state change.
    Trace(String),
    /// The checksum of the content that follows; this client writes whole files, so unused.
    Checksum(String),
    /// The repository's `rcsinfo` template for a folder, to be saved as `CVS/Template`.
    Template {
        dir: String,
        content: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Created,
    Updated,
    Merged,
    UpdateExisting,
    /// A context diff to apply to the working file, not the file's contents.
    Patched,
}

/// A started session: the pipe, the root, and what the server said it can do.
///
/// The sink is borrowed for the session's whole life, so everything the server says is
/// written where the caller asked before the next request goes out.
pub struct Session<'a> {
    conn: Connection,
    pub root: Root,
    pub capabilities: Vec<String>,
    pub command: String,
    /// True when the server accepted the `Utf8` request. Names on the wire are then UTF-8;
    /// otherwise they are the local code page's bytes.
    pub utf8_names: bool,
    /// The codec used for every name and path on the wire.
    wire_codec: Codec,
    /// The line the server is building with `MT text`/`MT fname`, emitted at `MT newline`.
    mt_line: String,
    sink: &'a mut dyn Sink,
    cancel: CancelToken,
}

impl<'a> Session<'a> {
    /// Exchange the opening requests and leave the session ready for a command.
    pub fn start(
        conn: Connection,
        root: Root,
        command: impl Into<String>,
        cancel: &CancelToken,
        sink: &'a mut dyn Sink,
    ) -> Result<Session<'a>> {
        let command = command.into();
        if cancel.is_cancelled() {
            return Err(Error::interrupted(&command));
        }
        let mut session = Session {
            conn,
            root,
            capabilities: Vec::new(),
            command,
            utf8_names: false,
            wire_codec: Codec::Utf8,
            mt_line: String::new(),
            sink,
            cancel: cancel.clone(),
        };
        // Register before the first request: a stop while the server is silent at startup
        // still has to break the read. `connect` already did this for a real connection, so
        // the call is a no-op there and only acts for an in-memory test connection.
        session.conn.register_canceller(&session.cancel);

        session.send(&format!("Root {}", session.root.protocol_root()))?;
        session.send(&format!("Valid-responses {}", VALID_RESPONSES.join(" ")))?;
        session.send("valid-requests")?;
        session.flush()?;

        loop {
            match session.read_response()? {
                Response::ValidRequests(names) => session.capabilities = names,
                Response::Ok => break,
                Response::E(text) => session.err(text),
                Response::Error { message, .. } => {
                    return Err(Error::aborted(&session.command, message));
                }
                other => {
                    return Err(Error::aborted(
                        &session.command,
                        format!("unexpected response at startup: {:?}", other),
                    ));
                }
            }
        }

        // Global options go to the server before any behaviour request, as the reference
        // client sends them (client.c:4501-4552). A server without `Global_option` gets a
        // warning and the command still runs.
        let flags = session.root.options.clone();
        if flags.no_execute || flags.quiet > 0 || flags.readonly || flags.trace {
            if session.supports("Global_option") {
                if flags.no_execute {
                    session.send("Global_option -n")?;
                }
                if flags.quiet >= 2 {
                    session.send("Global_option -Q")?;
                } else if flags.quiet == 1 {
                    session.send("Global_option -q")?;
                }
                if flags.readonly {
                    session.send("Global_option -r")?;
                }
                if flags.trace {
                    session.send("Global_option -t")?;
                }
                session.flush()?;
            } else {
                session.err("This server does not support the global options.".to_string());
            }
        }

        // Tell the server how the client will behave. `UseUnchanged` makes it possible to say
        // "this file is unchanged" without sending its contents.
        session.send("UseUnchanged")?;
        if session.supports("Case") {
            session.send("Case")?;
        }
        if session.supports("Utf8") {
            session.send("Utf8")?;
            session.utf8_names = true;
            session.wire_codec = Codec::Utf8;
        } else {
            // An older CVSNT stores and expects the local code page (GBK here).
            session.wire_codec = codec::system_ansi();
        }
        if trace_enabled() {
            session.err(format!(
                "cvs trace: capabilities: {:?}",
                session.capabilities
            ));
            session.err(format!("cvs trace: wire codec: {:?}", session.wire_codec));
        }
        session.flush()?;

        // `-z`: the reference enables compression after those plain behaviour requests and
        // before any command option, so the first compressed line is the folder block. A
        // server without the capability gets a plain stream and a note.
        let level = session.root.options.compression;
        if level > 0 {
            if session.supports("Gzip-stream") {
                session.send(&format!("Gzip-stream {}", level))?;
                session.flush()?;
                session
                    .conn
                    .compress(level)
                    .map_err(|e| Error::aborted(&session.command, e.to_string()))?;
            } else {
                session.err("server doesn't support gzip-file-contents".to_string());
            }
        }
        log::debug!(
            target: "cvsdesk::protocol",
            "phase=start cmd={} utf8={} capabilities={} compression={}",
            session.command,
            session.utf8_names,
            session.capabilities.len(),
            level
        );
        Ok(session)
    }

    pub fn supports(&self, name: &str) -> bool {
        self.capabilities.iter().any(|cap| cap == name)
    }

    /// `-n`: the command must not change the disk, locally or on the server.
    pub fn no_execute(&self) -> bool {
        self.root.options.no_execute
    }

    /// Write one line where the caller's stdout goes: an `M` response or a command report.
    pub fn out(&mut self, line: impl Into<String>) {
        self.sink.out(line.into());
    }

    /// Write one line where the caller's stderr goes: an `E` response or a trace line.
    pub fn err(&mut self, line: impl Into<String>) {
        self.sink.err(line.into());
    }

    /// Ask a yes/no question through the same sink the session writes to.
    pub fn confirm(&mut self, text: String) -> bool {
        self.sink.confirm(text)
    }

    /// Ask one line through the same sink. A window's sink answers `None` at once; a console
    /// sink reads the keyboard. Either way the command never reads the process's stdin.
    pub fn ask(&mut self, text: String) -> Option<String> {
        self.sink.ask(text)
    }

    pub fn send(&mut self, line: &str) -> Result<()> {
        let bytes = self
            .wire_codec
            .encode(line)
            .map_err(|e| Error::aborted(&self.command, e.to_string()))?;
        if trace_enabled() && (line.starts_with("Entry ") || line.starts_with("Directory ")) {
            let hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
            self.err(format!("cvs trace: send {}: {}", bytes.len(), hex));
        }
        self.conn.write_all(&bytes).map_err(|e| self.io_error(e))?;
        self.conn.write_all(b"\n").map_err(|e| self.io_error(e))
    }

    /// Raw bytes that belong to the previous request, such as a file's contents. No line
    /// ending is added: the server reads exactly the length it was told.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.conn.write_all(bytes).map_err(|e| self.io_error(e))
    }

    /// Send a file name as an `Argument`, with `Argumentx` for any newline it carries.
    pub fn argument(&mut self, text: &str) -> Result<()> {
        let mut pieces = text.split('\n');
        if let Some(first) = pieces.next() {
            self.send(&format!("Argument {}", first))?;
        }
        for piece in pieces {
            self.send(&format!("Argumentx {}", piece))?;
        }
        Ok(())
    }

    /// `Directory <local>` followed by the repository path on its own line, then the two
    /// markers the server cannot see for itself: `Static-directory` for a folder that holds
    /// `CVS/Entries.Static`, and `Sticky <tag>` for one with a `CVS/Tag`. Each is sent only
    /// when the server listed it in `Valid-requests`, in the reference client's order.
    pub fn directory(&mut self, local: &str, repository: &str, folder: &Path) -> Result<()> {
        self.send(&format!("Directory {}", local))?;
        self.send(repository)?;
        if self.supports("Static-directory") && dir::is_static(folder) {
            self.send("Static-directory")?;
        }
        if self.supports("Sticky") {
            if let Some(tag) = dir::sticky_tag(folder) {
                self.send(&format!("Sticky {}", tag))?;
            }
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.conn.flush().map_err(|e| self.io_error(e))
    }

    /// An error from the pipe: a stop that broke the read or write is the interrupted error,
    /// anything else keeps its own text.
    fn io_error(&self, error: io::Error) -> Error {
        if self.cancel.is_cancelled() {
            Error::interrupted(&self.command)
        } else {
            Error::aborted(&self.command, error.to_string())
        }
    }

    /// The server closed the pipe. After a stop that is the stop, not a lost connection.
    fn closed(&self) -> Error {
        if self.cancel.is_cancelled() {
            Error::interrupted(&self.command)
        } else {
            Error::aborted(&self.command, "the server closed the connection")
        }
    }

    /// Read one response, including the body that belongs to it.
    pub fn read_response(&mut self) -> Result<Response> {
        let line = match self.conn.read_line() {
            Ok(Some(line)) => line,
            Ok(None) => return Err(self.closed()),
            Err(e) => return Err(self.io_error(e)),
        };
        let text = self.wire_codec.decode(&line);
        let (name, rest) = split_name(&text);
        let response = match name {
            "ok" => Response::Ok,
            "error" => {
                let (code, message) = split_error(rest);
                Response::Error { code, message }
            }
            "E" => Response::E(rest.to_string()),
            "M" => Response::M(rest.to_string()),
            "F" => Response::F(rest.to_string()),
            "Valid-requests" => {
                Response::ValidRequests(rest.split_whitespace().map(|s| s.to_string()).collect())
            }
            "Checked-in" => Response::CheckedIn {
                update_dir: rest.to_string(),
                repository: self.extra_line()?,
                entry: self.extra_line()?,
            },
            "New-entry" => {
                // The server writes the folder, then the repository/file line, then the Entries
                // line (server.c:3559-3575,3820-3824).
                let update_dir = rest.to_string();
                let repository = self.extra_line()?;
                let entry = self.extra_line()?;
                Response::NewEntry {
                    update_dir,
                    repository,
                    entry,
                }
            }
            "Mode" => Response::Mode(rest.to_string()),
            "Mod-time" => Response::ModTime(rest.to_string()),
            "Removed" => {
                let update_dir = rest.to_string();
                let repository = self.extra_line()?;
                Response::Removed(response_path(&update_dir, &repository))
            }
            "Remove-entry" => {
                let update_dir = rest.to_string();
                let repository = self.extra_line()?;
                Response::RemoveEntry(response_path(&update_dir, &repository))
            }
            "Module-expansion" => Response::ModuleExpansion(rest.to_string()),
            "Notified" => {
                let _repository = self.extra_line()?;
                Response::Notified
            }
            // `MT` builds one line from parts: `text` is the prefix, `fname` names the file
            // the prefix belongs to, and `newline` ends it. CVSNT sends `U ` and the file
            // name separately, so printing each part on its own lost every file name.
            // Other tagged blocks (`+importmergecmd`, …) belong to the command that asked.
            "MT" => match rest.strip_prefix("text ") {
                Some(text) => {
                    self.mt_line.push_str(text);
                    Response::Trace(format!("text {}", text))
                }
                None => match rest.strip_prefix("fname ") {
                    Some(name) => {
                        self.mt_line.push_str(name);
                        Response::Trace(format!("fname {}", name))
                    }
                    None if rest == "newline" => Response::M(std::mem::take(&mut self.mt_line)),
                    None => Response::Trace(rest.to_string()),
                },
            },
            "Checksum" => Response::Checksum(rest.to_string()),
            "Template" => {
                // `Template <dir>`, then a decimal size and exactly that many bytes.
                let size_text = self.extra_line()?;
                let size: usize = size_text.trim().parse().map_err(|_| {
                    Error::aborted(
                        &self.command,
                        format!("the server sent a bad template size: {}", size_text),
                    )
                })?;
                let content = self.conn.read_exact(size).map_err(|e| self.io_error(e))?;
                Response::Template {
                    dir: rest.to_string(),
                    content,
                }
            }
            "Set-static-directory" => Response::SetStaticDirectory {
                dir: rest.to_string(),
                repository: self.extra_line()?,
            },
            "Clear-static-directory" => Response::ClearStaticDirectory {
                dir: rest.to_string(),
                repository: self.extra_line()?,
            },
            "Set-sticky" => Response::SetSticky {
                dir: rest.to_string(),
                repository: self.extra_line()?,
                tag: self.extra_line()?,
            },
            "Clear-sticky" => Response::ClearSticky {
                dir: rest.to_string(),
                repository: self.extra_line()?,
            },
            "Copy-file" => Response::CopyFile {
                update_dir: rest.to_string(),
                repository: self.extra_line()?,
                name: self.extra_line()?,
            },
            "Created" => self.file_response(FileKind::Created, rest)?,
            "Updated" => self.file_response(FileKind::Updated, rest)?,
            "Merged" => self.file_response(FileKind::Merged, rest)?,
            "Update-existing" => self.file_response(FileKind::UpdateExisting, rest)?,
            "Patched" => self.file_response(FileKind::Patched, rest)?,
            other => {
                return Err(Error::aborted(
                    &self.command,
                    format!("unrecognized response from the server: {}", other),
                ))
            }
        };
        Ok(response)
    }

    fn file_response(&mut self, kind: FileKind, update_dir: &str) -> Result<Response> {
        let repository = self.extra_line()?;
        let entry = self.extra_line()?;
        let mode = self.extra_line()?;
        let length_text = self.extra_line()?;
        let length: usize = length_text.trim().parse().map_err(|_| {
            Error::aborted(
                &self.command,
                format!("the server sent a bad file length: {}", length_text),
            )
        })?;
        // A corrupt or out-of-step stream must not make the client allocate gigabytes. The
        // protocol has no legitimate file this large in a working copy.
        const MAX_FILE: usize = 512 * 1024 * 1024;
        if length > MAX_FILE {
            return Err(Error::aborted(
                &self.command,
                format!("the server sent an impossible file length: {}", length),
            ));
        }
        let content = self.conn.read_exact(length).map_err(|e| self.io_error(e))?;
        Ok(Response::File {
            kind,
            update_dir: update_dir.to_string(),
            repository,
            entry,
            mode,
            content,
        })
    }

    fn extra_line(&mut self) -> Result<String> {
        match self.conn.read_line() {
            Ok(Some(line)) => Ok(self.wire_codec.decode(&line)),
            Ok(None) => Err(self.closed()),
            Err(e) => Err(self.io_error(e)),
        }
    }

    /// Read responses until `ok`, or return the server's error. Text responses are printed
    /// the way cvs prints them: `M` on stdout, `E` on stderr.
    pub fn read_to_ok(&mut self) -> Result<Vec<Response>> {
        let mut kept = Vec::new();
        loop {
            match self.read_response()? {
                Response::Ok => return Ok(kept),
                Response::Error { message, .. } => {
                    if message.trim().is_empty() {
                        return Err(Error::aborted(
                            &self.command,
                            "the server reported an error",
                        ));
                    }
                    return Err(Error::aborted(&self.command, message));
                }
                Response::E(text) => self.err(text),
                Response::M(text) => self.out(text),
                other => kept.push(other),
            }
        }
    }

    /// A one-shot request whose answer carries nothing but text (`noop`, `version`).
    pub fn simple(&mut self, request: &str) -> Result<Vec<Response>> {
        self.send(request)?;
        self.flush()?;
        self.read_to_ok()
    }

    /// `cvs version`: the server answers with one `M` line carrying its own banner.
    pub fn version(&mut self) -> Result<String> {
        // Old servers have no `version` request: the reference sends `noop` and prints
        // `(unknown)` instead of failing (version.c:74-80).
        if !self.supports("version") {
            let _ = self.simple("noop")?;
            return Ok("(unknown)".to_string());
        }
        self.send("version")?;
        self.flush()?;
        let mut server = String::new();
        loop {
            match self.read_response()? {
                Response::Ok => return Ok(server),
                Response::M(text) => server = text,
                Response::E(text) => self.err(text),
                Response::Error { message, .. } => {
                    if message.trim().is_empty() {
                        return Err(Error::aborted(
                            &self.command,
                            "the server reported an error",
                        ));
                    }
                    return Err(Error::aborted(&self.command, message));
                }
                other => {
                    return Err(Error::aborted(
                        &self.command,
                        format!("unexpected response to version: {:?}", other),
                    ))
                }
            }
        }
    }

    pub fn close(mut self) {
        log::debug!(
            target: "cvsdesk::protocol",
            "phase=close cmd={}",
            self.command
        );
        self.conn.close();
    }
}

fn trace_enabled() -> bool {
    std::env::var_os("CVSDESK_TRACE").is_some()
}

fn split_name(text: &str) -> (&str, &str) {
    match text.find(' ') {
        Some(i) => (&text[..i], &text[i + 1..]),
        None => (text, ""),
    }
}

/// `Removed` and `Remove-entry` name their folder on the first line and the repository plus
/// file name on the next, the way `Checked-in` does. Turn the pair back into the local path
/// (`d/a.txt`, or `a.txt` at the top) that the rest of the client already understands.
fn response_path(update_dir: &str, repository: &str) -> String {
    let name = repository
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(repository);
    let dir = update_dir.trim_matches('/');
    if dir.is_empty() || dir == "." {
        name.to_string()
    } else {
        format!("{}/{}", dir, name)
    }
}

fn split_error(rest: &str) -> (String, String) {
    let trimmed = rest.trim_start();
    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && trimmed.as_bytes().get(digits) == Some(&b' ') {
        (
            trimmed[..digits].to_string(),
            trimmed[digits + 1..].to_string(),
        )
    } else {
        (String::new(), trimmed.to_string())
    }
}

/// Open the connection for `root` and start a session.
pub fn open<'a>(
    root: Root,
    command: &str,
    cancel: &CancelToken,
    sink: &'a mut dyn Sink,
) -> Result<Session<'a>> {
    if cancel.is_cancelled() {
        return Err(Error::interrupted(command));
    }
    let conn = match transport::connect(&root, command, cancel, sink) {
        Ok(conn) => conn,
        Err(_) if cancel.is_cancelled() => return Err(Error::interrupted(command)),
        Err(error) => return Err(error),
    };
    Session::start(conn, root, command, cancel, sink)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::engine::test_sink::{Channel, RecordingSink};
    use std::io::{self, Cursor, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn session_with<'a>(
        replies: &[u8],
        sink: &'a mut dyn crate::client::engine::Sink,
    ) -> (Session<'a>, Arc<Mutex<Vec<u8>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let reader = Cursor::new(replies.to_vec());
        let conn =
            Connection::from_parts_for_test(Box::new(reader), Box::new(Sink(written.clone())));
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        let cancel = CancelToken::default();
        let session = Session::start(conn, root, "noop", &cancel, sink).unwrap();
        (session, written)
    }

    #[test]
    fn startup_sends_the_opening_requests_and_reads_capabilities() {
        let replies = b"Valid-requests Root Valid-responses Case Utf8 noop\nok\nok\n";
        let mut sink = RecordingSink::default();
        let (mut session, written) = session_with(replies, &mut sink);
        assert!(session.capabilities.iter().any(|c| c == "Case"));
        assert!(session.utf8_names);
        session.simple("noop").unwrap();
        let text = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        assert!(text.contains("Root /repo\n"), "{}", text);
        assert!(text.contains("Valid-responses "), "{}", text);
        assert!(text.contains("valid-requests\n"), "{}", text);
        assert!(text.contains("UseUnchanged\n"), "{}", text);
        assert!(text.contains("Case\n"), "{}", text);
        assert!(text.contains("Utf8\n"), "{}", text);
        assert!(text.ends_with("noop\n"), "{}", text);
    }

    #[test]
    fn server_messages_reach_the_sink_on_their_own_channels_in_order() {
        let replies = b"Valid-requests noop\nok\nM one\nE two\nM three\nok\n";
        let mut sink = RecordingSink::default();
        let (mut session, _) = session_with(replies, &mut sink);
        session.simple("noop").unwrap();
        assert_eq!(
            sink.lines,
            vec![
                (Channel::Out, "one".to_string()),
                (Channel::Err, "two".to_string()),
                (Channel::Out, "three".to_string()),
            ]
        );
    }

    #[test]
    fn a_created_file_is_read_with_its_entry_and_bytes() {
        let mut replies = Vec::new();
        replies.extend_from_slice(b"Valid-requests noop\nok\n");
        replies.extend_from_slice(
            b"Created d/\n/repo/d/one.txt\n/one.txt/1.2///\nu=rw,g=rw,o=rw\n5\nhellook\n",
        );
        let mut sink = RecordingSink::default();
        let (mut session, _) = session_with(&replies, &mut sink);
        session.send("update").unwrap();
        let responses = session.read_to_ok().unwrap();
        assert_eq!(responses.len(), 1);
        match &responses[0] {
            Response::File {
                kind,
                update_dir,
                repository,
                entry,
                mode,
                content,
            } => {
                assert_eq!(*kind, FileKind::Created);
                assert_eq!(update_dir, "d/");
                assert_eq!(repository, "/repo/d/one.txt");
                assert_eq!(entry, "/one.txt/1.2///");
                assert_eq!(mode, "u=rw,g=rw,o=rw");
                assert_eq!(content, b"hello");
            }
            other => panic!("not a file response: {:?}", other),
        }
    }

    #[test]
    fn a_remove_entry_reads_the_repository_line_that_follows_it() {
        let replies = b"Valid-requests noop\nok\nRemove-entry d/\n/repo/d/gone.txt\nok\n";
        let mut sink = RecordingSink::default();
        let (mut session, _) = session_with(replies, &mut sink);
        assert_eq!(
            session.read_response().unwrap(),
            Response::RemoveEntry("d/gone.txt".to_string())
        );
        assert_eq!(session.read_response().unwrap(), Response::Ok);
    }

    #[test]
    fn an_error_response_stops_the_command() {
        let replies = b"Valid-requests noop\nok\nerror 1 no such module\n";
        let mut sink = RecordingSink::default();
        let (mut session, _) = session_with(replies, &mut sink);
        let err = session.simple("update").unwrap_err();
        assert!(err.text().contains("no such module"), "{}", err);
    }

    #[test]
    fn a_short_file_body_is_an_error_not_a_short_write() {
        let mut replies = Vec::new();
        replies.extend_from_slice(b"Valid-requests noop\nok\n");
        replies.extend_from_slice(
            b"Created d/\n/repo/d/one.txt\n/one.txt/1.2///\nu=rw,g=rw,o=rw\n50\nhello",
        );
        let mut sink = RecordingSink::default();
        let (mut session, _) = session_with(&replies, &mut sink);
        session.send("update").unwrap();
        assert!(session.read_to_ok().is_err());
    }

    #[test]
    fn a_pipe_that_ends_after_a_cancel_is_the_interrupted_error() {
        let replies = b"Valid-requests noop\nok\n";
        let conn = Connection::from_parts_for_test(
            Box::new(Cursor::new(replies.to_vec())),
            Box::new(io::sink()),
        );
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        let mut sink = RecordingSink::default();
        let cancel = CancelToken::default();
        let mut session = Session::start(conn, root, "noop", &cancel, &mut sink).unwrap();

        cancel.cancel();
        let error = session.read_response().unwrap_err();
        assert!(error.is_interrupted(), "{}", error);
        assert_eq!(error.text(), "cvs [noop aborted]: stopped by the user");
    }
}

#[cfg(test)]
mod template_tests {
    use super::*;
    use crate::client::engine::test_sink::RecordingSink;
    use std::io::Cursor;

    #[test]
    fn a_template_response_carries_its_counted_body() {
        let replies = b"Valid-requests Template noop\nok\nTemplate .\n5\nhellook\n";
        let conn = Connection::from_parts_for_test(
            Box::new(Cursor::new(replies.to_vec())),
            Box::new(std::io::sink()),
        );
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        let cancel = CancelToken::default();
        let mut sink = RecordingSink::default();
        let mut session = Session::start(conn, root, "noop", &cancel, &mut sink).unwrap();
        let responses = session.simple("noop").unwrap();
        assert_eq!(
            responses,
            vec![Response::Template {
                dir: ".".to_string(),
                content: b"hello".to_vec()
            }]
        );
    }
}

#[cfg(test)]
mod mt_tests {
    use super::*;
    use crate::client::engine::test_sink::RecordingSink;
    use std::io::Cursor;

    #[test]
    fn server_text_carried_by_mt_is_an_ordinary_message() {
        let replies = b"Valid-requests MT noop\nok\n\
            MT text U \nMT fname mod/a.txt\nMT newline\n\
            MT text keep going\nMT newline\nMT +importmergecmd\nok\n";
        let conn = Connection::from_parts_for_test(
            Box::new(Cursor::new(replies.to_vec())),
            Box::new(std::io::sink()),
        );
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        let cancel = CancelToken::default();
        let mut sink = RecordingSink::default();
        let mut session = Session::start(conn, root, "noop", &cancel, &mut sink).unwrap();
        assert!(matches!(
            session.read_response().unwrap(),
            Response::Trace(_)
        ));
        assert!(matches!(
            session.read_response().unwrap(),
            Response::Trace(_)
        ));
        assert_eq!(
            session.read_response().unwrap(),
            Response::M("U mod/a.txt".to_string())
        );
        assert!(matches!(
            session.read_response().unwrap(),
            Response::Trace(_)
        ));
        assert_eq!(
            session.read_response().unwrap(),
            Response::M("keep going".to_string())
        );
        assert_eq!(
            session.read_response().unwrap(),
            Response::Trace("+importmergecmd".to_string())
        );
    }
}
