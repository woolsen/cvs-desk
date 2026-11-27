//! Opening the connection that carries the CVS protocol.
//!
//! Two methods are supported today:
//!
//! * `pserver` — a TCP connection, opened with the plain-text authentication handshake CVS
//!   has used since 1.6. The password is scrambled with CVS's substitution both in
//!   `.cvspass` and on the wire; the server unscrambles it. That is not encryption, which is
//!   why CVS itself calls this protocol insecure.
//! * `ext` — a child process, started through `CVS_RSH` (or `rsh`) and `CVS_SERVER`
//!   (or `cvs`). This is how `:ext:` works, and it is also the cheapest way to test the
//!   protocol against the real server program on one machine.
//!
//! The protocol is lock-step: a request is written, the server's whole answer is read, then
//! the next request. Every write is flushed for that reason.

use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};

use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::passfile;
use crate::client::registry;
use crate::client::root::{Method, Root};

/// The `:ext:` child, shared between the connection and its canceller.
type SharedChild = Arc<Mutex<Option<Child>>>;

/// The pserver socket, shared by the reader, the writer and the canceller.
///
/// One handle for all three matters on Windows: shutting down a `try_clone` duplicate does
/// not always wake a read blocked on another duplicate, but shutting down the very handle
/// the reader is using does.
#[derive(Clone)]
struct SharedStream(Arc<TcpStream>);

impl Read for SharedStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl Write for SharedStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}

/// How a cancel breaks a blocked read: shut a socket down, or kill the child holding the
/// pipes. The test and in-memory connections have nothing to break.
#[derive(Clone)]
enum Canceller {
    Nothing,
    Stream(Arc<Mutex<Option<Arc<TcpStream>>>>),
    Child(SharedChild),
}

impl Canceller {
    fn call(&self) {
        match self {
            Canceller::Nothing => {}
            Canceller::Stream(stream) => {
                let stream = stream
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(stream) = stream.as_ref() {
                    let _ = stream.shutdown(Shutdown::Both);
                }
            }
            Canceller::Child(child) => {
                let mut child = child
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(child) = child.as_mut() {
                    let _ = child.kill();
                }
            }
        }
    }
}

/// One direction of the `Gzip-stream` compression: a zlib stream over the connection.
///
/// `flate2`'s read-side decoder asks its source for more input before it drains output the
/// decompressor is already holding. A CVS server keeps the socket open after a reply, so that
/// read waits forever whenever the tail of a reply sits inside the decoder. This reader always
/// drains the decompressor first and reads the pipe only when it has no output left.
///
/// Building it does not touch the pipe: compression is turned on before the command is sent,
/// and the server writes nothing until it has that command.
struct ZlibStream<R> {
    source: R,
    decompress: Decompress,
    input: Vec<u8>,
    pos: usize,
    /// A fresh chunk buffer, reused for every read from the pipe.
    chunk: Vec<u8>,
    ended: bool,
}

impl<R: Read> ZlibStream<R> {
    fn new(source: R) -> Self {
        Self {
            source,
            decompress: Decompress::new(true),
            input: Vec::new(),
            pos: 0,
            chunk: vec![0u8; 32 * 1024],
            ended: false,
        }
    }
}

impl<R: Read> Read for ZlibStream<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.ended {
                return Ok(0);
            }
            let before_in = self.decompress.total_in();
            let before_out = self.decompress.total_out();
            let status = self
                .decompress
                .decompress(&self.input[self.pos..], out, FlushDecompress::None)
                .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("corrupt compressed stream: {}", error),
                    )
                })?;
            let consumed = (self.decompress.total_in() - before_in) as usize;
            let produced = (self.decompress.total_out() - before_out) as usize;
            self.pos += consumed;
            if produced > 0 {
                return Ok(produced);
            }
            if status == Status::StreamEnd {
                self.ended = true;
                return Ok(0);
            }
            if consumed > 0 {
                // Progress was made; the next pass may produce output from the bytes left.
                continue;
            }
            // The decoder needs more input. A header can be split across reads, so give it
            // every byte the pipe has, but never buffer an unbounded broken stream.
            const MAX_PENDING: usize = 1024 * 1024;
            if self.input.len() - self.pos > MAX_PENDING {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the compressed stream made no progress",
                ));
            }
            if self.pos > 0 {
                self.input.drain(..self.pos);
                self.pos = 0;
            }
            let read = self.source.read(&mut self.chunk)?;
            if read == 0 {
                // The peer closed its end. The reply ended with it; anything still pending
                // has already been drained above.
                self.ended = true;
                return Ok(0);
            }
            self.input.extend_from_slice(&self.chunk[..read]);
        }
    }
}

/// A byte pipe to a CVS server, with the child that carries it when there is one.
pub struct Connection {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: BufWriter<Box<dyn Write + Send>>,
    child: Option<SharedChild>,
    canceller: Canceller,
    /// True once this connection has given its canceller to a token.
    canceller_registered: bool,
}

impl Connection {
    fn from_parts(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        child: Option<Child>,
    ) -> Self {
        let child = child.map(|child| Arc::new(Mutex::new(Some(child))));
        let canceller = match &child {
            Some(child) => Canceller::Child(child.clone()),
            None => Canceller::Nothing,
        };
        Self {
            reader: BufReader::new(reader),
            writer: BufWriter::new(writer),
            child,
            canceller,
            canceller_registered: false,
        }
    }

    /// A closure another thread may call to interrupt a blocked read or write. It is a
    /// no-op for an in-memory connection.
    pub fn canceller(&self) -> impl Fn() + Send + Sync + 'static {
        let canceller = self.canceller.clone();
        move || canceller.call()
    }

    /// Hand `cancel` the way to interrupt this connection. `connect` does this before the
    /// pserver authentication exchange; a later `Session::start` repeats it, and the second
    /// call does nothing.
    pub fn register_canceller(&mut self, cancel: &CancelToken) {
        if self.canceller_registered {
            return;
        }
        self.canceller_registered = true;
        cancel.register(self.canceller());
    }

    /// An in-memory connection, so protocol tests can play both ends.
    #[cfg(test)]
    pub(crate) fn from_parts_for_test(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
    ) -> Self {
        Self::from_parts(reader, writer, None)
    }

    /// One protocol line, without its line ending. `None` means the server closed the pipe.
    pub fn read_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut line = Vec::new();
        // A protocol line is far shorter than this; the limit keeps a corrupt stream from
        // growing without bound.
        const MAX_LINE: u64 = 1024 * 1024;
        let read = self
            .reader
            .by_ref()
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(None);
        }
        if line.len() as u64 > MAX_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "protocol line is too long",
            ));
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Ok(Some(line))
    }

    /// The same line as text. Control lines are ASCII or UTF-8 in this protocol.
    pub fn read_line_text(&mut self) -> io::Result<Option<String>> {
        Ok(self
            .read_line()?
            .map(|line| String::from_utf8_lossy(&line).into_owned()))
    }

    /// Exactly `count` bytes, for file contents that follow a length line.
    pub fn read_exact(&mut self, count: usize) -> io::Result<Vec<u8>> {
        // A corrupt or out-of-step stream can name an enormous block; never allocate it.
        const MAX_BLOCK: usize = 512 * 1024 * 1024;
        if count > MAX_BLOCK {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("impossible block size: {}", count),
            ));
        }
        let mut bytes = vec![0u8; count];
        self.reader.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)
    }

    /// One protocol request line. Protocol lines cannot contain a newline.
    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        debug_assert!(!line.contains('\n'), "protocol line contains a newline");
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    /// Put both directions into the compressed stream the server agreed to. Despite the
    /// request's name, `Gzip-stream` carries a zlib stream, not a gzip one; the reference
    /// client writes those frames and a gzip decoder rejects the server's `78 da` header.
    /// The `Gzip-stream` line must already be written and flushed; nothing may be left in the
    /// read buffer, because those bytes would not pass through the decoder.
    pub fn compress(&mut self, level: u32) -> io::Result<()> {
        self.writer.flush()?;
        if !self.reader.buffer().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the server sent bytes beyond the handshake before compression started",
            ));
        }
        let writer = std::mem::replace(&mut self.writer, BufWriter::new(Box::new(io::sink())));
        let writer = writer.into_inner()?;
        self.writer = BufWriter::new(Box::new(ZlibEncoder::new(writer, Compression::new(level))));

        let reader = std::mem::replace(&mut self.reader, BufReader::new(Box::new(io::empty())));
        let reader = reader.into_inner();
        self.reader = BufReader::new(Box::new(ZlibStream::new(reader)));
        Ok(())
    }

    /// Flush, close our end, and wait for the child to end. A pserver socket stays open for
    /// reading until it is dropped with the connection.
    pub fn close(&mut self) {
        // Drop the canceller's handle, so a late cancel has nothing left to shut down; the
        // reader and writer keep their own references until the connection is dropped.
        if let Canceller::Stream(stream) = &self.canceller {
            let mut stream = stream
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            stream.take();
        }
        let _ = self.writer.flush();
        // Replace the writer so the real one is dropped and the child sees EOF.
        self.writer = BufWriter::new(Box::new(io::sink()));
        // Take the child out of the shared slot before waiting: a canceller may still need
        // the lock to kill it, and waiting while holding it would deadlock.
        let child = self.child.take().and_then(|child| {
            let mut child = child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            child.take()
        });
        if let Some(mut child) = child {
            let _ = child.wait();
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

/// Open a connection for `root`, taking the password from `.cvspass`, the CVSNT store,
/// or the caller's sink. A sink with no terminal has no password to give, so the command
/// fails with a message that names the way out.
pub fn connect(
    root: &Root,
    command: &str,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<Connection> {
    log::debug!(
        target: "cvsdesk::connect",
        "phase=open method={}",
        root.method.token()
    );
    match root.method {
        Method::Pserver => {
            let password = match passfile::scrambled_for(&root.key)
                .or_else(|| registry::scrambled_for(&root.key))
            {
                Some(password) => password,
                None => match sink.secret("CVS password: ".to_string()) {
                    Some(password) => passfile::scramble(password.as_bytes()),
                    None => {
                        return Err(Error::aborted(
                            command,
                            "no saved password for this root; run 'cvs login' first",
                        ))
                    }
                },
            };
            pserver(root, &password, command, cancel)
        }
        Method::Ext => ext(root, command, cancel),
        ref other => Err(Error::aborted(
            command,
            format!(
                "{} access is not supported by this cvs; use :pserver: or :ext:",
                other.token()
            ),
        )),
    }
}

/// Open a pserver connection with a known plain password (used by tests and by `cvs login`
/// checks). The password is scrambled here, the way CVS sends it.
pub fn connect_with_password(
    root: &Root,
    password: &str,
    command: &str,
    cancel: &CancelToken,
) -> Result<Connection> {
    match root.method {
        Method::Pserver => pserver(
            root,
            &passfile::scramble(password.as_bytes()),
            command,
            cancel,
        ),
        Method::Ext => ext(root, command, cancel),
        ref other => Err(Error::aborted(
            command,
            format!(
                "{} access is not supported by this cvs; use :pserver: or :ext:",
                other.token()
            ),
        )),
    }
}

/// An error from the login exchange: a stop that broke it is the interrupted error, anything
/// else keeps its own text.
fn auth_error(cancel: &CancelToken, command: &str, error: io::Error) -> Error {
    if cancel.is_cancelled() {
        log::debug!(
            target: "cvsdesk::connect",
            "phase=auth result=interrupted"
        );
        Error::interrupted(command)
    } else {
        log::warn!(
            target: "cvsdesk::connect",
            "phase=auth result=error {}",
            crate::diag::io_error(&error)
        );
        Error::aborted(command, error.to_string())
    }
}

fn pserver(
    root: &Root,
    password: &[u8],
    command: &str,
    cancel: &CancelToken,
) -> Result<Connection> {
    if root.proxy.is_some() {
        return Err(Error::aborted(
            command,
            "proxy connections are not supported by this cvs yet",
        ));
    }
    let stream = match tcp_connect(&root.host, root.port, command) {
        Ok(stream) => stream,
        Err(_) if cancel.is_cancelled() => return Err(Error::interrupted(command)),
        Err(error) => return Err(error),
    };
    let _ = stream.set_nodelay(true);
    // One handle for reading, writing and the canceller: see [`SharedStream`].
    let stream = Arc::new(stream);
    let mut conn = Connection::from_parts(
        Box::new(SharedStream(stream.clone())),
        Box::new(SharedStream(stream.clone())),
        None,
    );
    conn.canceller = Canceller::Stream(Arc::new(Mutex::new(Some(stream))));
    // Register before the first auth line: a stop must break the wait for `I LOVE YOU` too.
    conn.register_canceller(cancel);

    // The handshake carries the repository path, the user and the password, each on its own
    // line. The password is plain text here; pserver has no encryption.
    let auth_started = std::time::Instant::now();
    conn.write_line("BEGIN AUTH REQUEST")
        .map_err(|e| auth_error(cancel, command, e))?;
    conn.write_line(&root.path)
        .map_err(|e| auth_error(cancel, command, e))?;
    conn.write_line(&root.user)
        .map_err(|e| auth_error(cancel, command, e))?;
    conn.write_all(password)
        .map_err(|e| auth_error(cancel, command, e))?;
    conn.write_all(b"\n")
        .map_err(|e| auth_error(cancel, command, e))?;
    conn.write_line("END AUTH REQUEST")
        .map_err(|e| auth_error(cancel, command, e))?;
    conn.flush().map_err(|e| auth_error(cancel, command, e))?;

    let answer = conn
        .read_line_text()
        .map_err(|e| auth_error(cancel, command, e))?
        .ok_or_else(|| {
            if cancel.is_cancelled() {
                Error::interrupted(command)
            } else {
                Error::aborted(command, "server closed the connection during login")
            }
        })?;
    if answer.trim() != "I LOVE YOU" {
        log::warn!(
            target: "cvsdesk::connect",
            "phase=auth result=rejected elapsed_ms={}",
            auth_started.elapsed().as_millis()
        );
        return Err(Error::plain(
            command,
            format!(
                "authorization failed: server {} rejected access to {} for user {}",
                root.host, root.path, root.user
            ),
        ));
    }
    log::info!(
        target: "cvsdesk::connect",
        "phase=auth result=ok elapsed_ms={}",
        auth_started.elapsed().as_millis()
    );
    Ok(conn)
}

fn tcp_connect(host: &str, port: u16, command: &str) -> Result<TcpStream> {
    let address = format!("{}:{}", host, port);
    let started = std::time::Instant::now();
    let addrs: Vec<_> = match address.to_socket_addrs() {
        Ok(addrs) => addrs.collect(),
        Err(error) => {
            log::warn!(
                target: "cvsdesk::connect",
                "phase=resolve result=error {} elapsed_ms={}",
                crate::diag::io_error(&error),
                started.elapsed().as_millis()
            );
            return Err(Error::aborted(
                command,
                format!("cannot resolve {}: {}", host, error),
            ));
        }
    };
    log::debug!(
        target: "cvsdesk::connect",
        "phase=resolve result=ok addresses={} elapsed_ms={}",
        addrs.len(),
        started.elapsed().as_millis()
    );
    if addrs.is_empty() {
        log::warn!(
            target: "cvsdesk::connect",
            "phase=resolve result=no-address"
        );
        return Err(Error::aborted(
            command,
            format!("cannot resolve {}: no address", host),
        ));
    }
    let mut last = None;
    for (index, addr) in addrs.into_iter().enumerate() {
        let attempt = std::time::Instant::now();
        match TcpStream::connect_timeout(&addr, Duration::from_secs(30)) {
            Ok(stream) => {
                log::debug!(
                    target: "cvsdesk::connect",
                    "phase=tcp result=ok attempt={} elapsed_ms={}",
                    index,
                    attempt.elapsed().as_millis()
                );
                return Ok(stream);
            }
            Err(error) => {
                log::debug!(
                    target: "cvsdesk::connect",
                    "phase=tcp result=error attempt={} {} elapsed_ms={}",
                    index,
                    crate::diag::io_error(&error),
                    attempt.elapsed().as_millis()
                );
                last = Some(error);
            }
        }
    }
    log::warn!(
        target: "cvsdesk::connect",
        "phase=tcp result=error port={} elapsed_ms={}",
        port,
        started.elapsed().as_millis()
    );
    Err(Error::aborted(
        command,
        format!(
            "connect to {}:{} failed: {}",
            host,
            port,
            last.map(|e| e.to_string()).unwrap_or_default()
        ),
    ))
}

fn ext(root: &Root, command: &str, cancel: &CancelToken) -> Result<Connection> {
    let rsh = std::env::var("CVS_RSH").unwrap_or_else(|_| "rsh".to_string());
    let server = std::env::var("CVS_SERVER").unwrap_or_else(|_| "cvs".to_string());
    log::debug!(target: "cvsdesk::connect", "phase=spawn method=ext");
    let mut cmd = shell_command(&rsh);
    cmd.arg(&root.host).arg(&server).arg("server");
    let mut conn = spawn(cmd, command)?;
    conn.register_canceller(cancel);
    Ok(conn)
}

/// A command that may be a `.bat`/`.cmd` script, which Windows needs a shell to start.
fn shell_command(program: &str) -> Command {
    let lower = program.to_ascii_lowercase();
    if lower.ends_with(".bat") || lower.ends_with(".cmd") {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(program);
        cmd
    } else {
        Command::new(program)
    }
}

/// Start `program` and use its standard input and output as the protocol pipe.
pub fn spawn(mut command: Command, what: &str) -> Result<Connection> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            log::warn!(
                target: "cvsdesk::connect",
                "phase=spawn result=error {}",
                crate::diag::io_error(&error)
            );
            return Err(Error::aborted(
                what,
                format!("cannot start the server: {}", error),
            ));
        }
    };
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::aborted(what, "the server has no input pipe"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::aborted(what, "the server has no output pipe"))?;
    Ok(Connection::from_parts(
        Box::new(stdout),
        Box::new(stdin),
        Some(child),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::engine::CancelToken;
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn read_line(reader: &mut BufReader<TcpStream>) -> String {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line.trim_end_matches(['\r', '\n']).to_string()
    }

    #[test]
    fn the_pserver_handshake_carries_path_user_and_password() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            assert_eq!(read_line(&mut reader), "BEGIN AUTH REQUEST");
            tx.send(read_line(&mut reader)).unwrap();
            assert_eq!(read_line(&mut reader), "ann");
            assert_eq!(
                read_line(&mut reader),
                String::from_utf8(passfile::scramble(b"secret")).unwrap()
            );
            assert_eq!(read_line(&mut reader), "END AUTH REQUEST");
            writer.write_all(b"I LOVE YOU\n").unwrap();
            writer.flush().unwrap();
            // Keep the socket alive until the client is done with it.
            std::thread::sleep(Duration::from_millis(50));
        });
        let root = Root::parse(&format!(":pserver:ann@127.0.0.1:{}:/repo", port)).unwrap();
        let conn =
            connect_with_password(&root, "secret", "login", &CancelToken::default()).unwrap();
        assert_eq!(rx.recv().unwrap(), "/repo");
        drop(conn);
        server.join().unwrap();
    }

    #[test]
    fn a_refused_login_is_reported_the_way_cvs_reports_it() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            for _ in 0..5 {
                read_line(&mut reader);
            }
            writer.write_all(b"I HATE YOU\n").unwrap();
            writer.flush().unwrap();
        });
        let root = Root::parse(&format!(":pserver:ann@127.0.0.1:{}:/repo", port)).unwrap();
        let err = match connect_with_password(&root, "wrong", "login", &CancelToken::default()) {
            Ok(_) => panic!("a refused login must not produce a connection"),
            Err(e) => e,
        };
        assert!(err.text().contains("authorization failed"), "{}", err);
        server.join().unwrap();
    }

    struct Silent;

    impl Sink for Silent {
        fn out(&mut self, _line: String) {}
        fn err(&mut self, _line: String) {}
    }

    #[test]
    fn a_sink_without_a_password_is_told_to_log_in() {
        let root_text = format!(
            ":pserver:nobody{}@example.invalid:/repo",
            std::process::id()
        );
        let root = Root::parse(&root_text).unwrap();
        let mut sink = Silent;
        let err = match connect(&root, "status", &CancelToken::default(), &mut sink) {
            Ok(_) => panic!("a connection without a password must fail"),
            Err(e) => e,
        };
        assert!(err.text().contains("cvs login"), "{}", err);
    }

    #[test]
    fn a_test_connection_has_a_canceller_that_does_nothing() {
        let conn = Connection::from_parts_for_test(Box::new(io::empty()), Box::new(io::sink()));
        let cancel = CancelToken::default();
        cancel.register(conn.canceller());
        cancel.cancel();
        assert!(cancel.is_cancelled());
    }

    /// Killing the child closes its pipes, so a reader blocked on them sees the end.
    #[cfg(windows)]
    #[test]
    fn cancelling_an_ext_child_ends_its_pipe() {
        let mut command = Command::new("ping");
        command.args(["-n", "30", "127.0.0.1"]);
        let mut conn = spawn(command, "test").unwrap();
        let cancel = CancelToken::default();
        cancel.register(conn.canceller());

        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            while let Ok(Some(_)) = conn.read_line() {}
            let _ = tx.send(());
        });

        // Let the child start, then stop it the way the window's Stop button will.
        std::thread::sleep(Duration::from_millis(250));
        cancel.cancel();
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the child's pipe stayed open after the stop"
        );
        reader.join().unwrap();
    }

    /// A source that hands out its bytes, then waits for permission to report EOF. It
    /// stands in for a server that keeps the connection open after its reply.
    struct OpenStream {
        data: Vec<u8>,
        pos: usize,
        gate: mpsc::Receiver<()>,
    }

    impl Read for OpenStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos == self.data.len() {
                let _ = self.gate.recv();
                return Ok(0);
            }
            let count = buf.len().min(self.data.len() - self.pos);
            buf[..count].copy_from_slice(&self.data[self.pos..self.pos + count]);
            self.pos += count;
            Ok(count)
        }
    }

    /// A compressed reply can end while the server keeps the socket open. Once every
    /// compressed byte has arrived, the rest of the reply may still sit inside the
    /// decompressor; asking the socket for more input first waits forever.
    #[test]
    fn a_complete_compressed_reply_does_not_wait_for_more_input() {
        let mut text = String::new();
        for i in 0..1500 {
            text.push_str(&format!("\tTAG_{:04}: 1.{}\n", i, i % 20));
        }
        text.push_str("ok\n");
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(9));
        encoder.write_all(text.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();

        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut conn = Connection::from_parts_for_test(
                Box::new(OpenStream {
                    data: compressed,
                    pos: 0,
                    gate: release_rx,
                }),
                Box::new(Vec::new()),
            );
            conn.compress(9).unwrap();
            let mut lines = 0;
            loop {
                match conn.read_line() {
                    Ok(Some(line)) if line == b"ok" => break,
                    Ok(Some(_)) => lines += 1,
                    Ok(None) => break,
                    Err(error) => panic!("read failed: {}", error),
                }
            }
            let _ = done_tx.send(lines);
            conn.close();
        });

        let result = done_rx.recv_timeout(Duration::from_secs(2));
        let _ = release_tx.send(());
        assert_eq!(
            result.expect("the decoder waited for input although the reply was complete"),
            1500
        );
    }
}
