//! `cvs init`: ask the server to create a repository.
//!
//! There is no working copy to look at, and no files to send: the client names the repository
//! and the server creates it. The reference sends `init <path>`, where the path is the root's
//! own directory with `:method:user@host:` stripped off.
//!
//! `init` is the one command whose startup is *rootless*: the reference does not send `Root`
//! before it (client.c:4461-4467), because the repository may not exist yet. A `Root` request
//! would make a stock server fail in `serve_root` before `serve_init` is ever reached. This
//! command therefore opens the transport itself and exchanges only `Valid-responses` and
//! `valid-requests` before sending `init`.
//!
//! A stock CVS 1.11.23 server answers `E init may not be run remotely` and then an empty
//! `error`; the request exists so that a server which does allow it (CVSNT among them) can
//! honour it. This client prints the `E` line and the command's exit code is 1, with no extra
//! message for the empty error (client.c:928-962).
//!
//! A local root (a bare path or `:local:`) is refused: this client has no server side, so it
//! cannot create the repository's `CVSROOT` files itself.

use std::io;

use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::VALID_RESPONSES;
use crate::client::root::{Method, Root};
use crate::client::transport::{self, Connection};
use crate::client::work::codec;

pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    if !args.is_empty() {
        // The reference prints the usage text and stops without connecting.
        sink.err("Usage: cvs init".to_string());
        sink.err("(Specify the --help global option for a list of other help options)".to_string());
        return Ok(1);
    }
    if matches!(root.method, Method::Local) {
        return Err(Error::aborted(
            "init",
            "creating a local repository is not supported by this cvs; use :pserver: or :ext:",
        ));
    }
    if cancel.is_cancelled() {
        return Err(Error::interrupted("init"));
    }
    let conn = match transport::connect(&root, "init", cancel, sink) {
        Ok(conn) => conn,
        Err(_) if cancel.is_cancelled() => return Err(Error::interrupted("init")),
        Err(error) => return Err(error),
    };
    run_connected(conn, &root, cancel, sink)
}

/// The exchange after the transport is open. The connection is closed on every path.
fn run_connected(
    mut conn: Connection,
    root: &Root,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let outcome = exchange(&mut conn, root, cancel, sink);
    conn.close();
    outcome
}

fn exchange(
    conn: &mut Connection,
    root: &Root,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let Some(capabilities) = handshake(conn, cancel, sink)? else {
        // The server answered `error` to the startup requests; it was already shown.
        return Ok(1);
    };

    // Global options travel after the rootless handshake. `-z` does not: the reference
    // explicitly skips compression when the startup is rootless (client.c:4623).
    let options = global_options(root);
    if !options.is_empty() && !supports(&capabilities, "Global_option") {
        return Err(Error::aborted(
            "init",
            format!(
                "This server does not support the global {} option.",
                options[0]
            ),
        ));
    }
    for option in options {
        write_line(conn, format!("Global_option {}", option), cancel)?;
    }

    // `init` takes the repository directory directly on its request line; it is not sent as
    // `Argument` (client.c:5941-5946).
    write_line(conn, format!("init {}", root.protocol_root()), cancel)?;
    flush(conn, cancel)?;
    read_init_response(conn, cancel, sink)
}

/// `Valid-responses` + `valid-requests`, then the server's `Valid-requests`/`ok`.
///
/// Returns `None` when the server answered `error` during startup (the message, if any, has
/// been shown already), so the caller exits 1 without saying anything more.
fn handshake(
    conn: &mut Connection,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<Option<Vec<String>>> {
    write_line(
        conn,
        format!("Valid-responses {}", VALID_RESPONSES.join(" ")),
        cancel,
    )?;
    write_line(conn, "valid-requests".to_string(), cancel)?;
    flush(conn, cancel)?;

    let mut capabilities = Vec::new();
    loop {
        let line = read_line(conn, cancel)?.ok_or_else(|| closed())?;
        let (name, rest) = split(&line);
        match name {
            "Valid-requests" => {
                capabilities = rest.split_whitespace().map(str::to_string).collect();
            }
            "ok" => return Ok(Some(capabilities)),
            "E" => sink.err(rest.to_string()),
            "M" => sink.out(rest.to_string()),
            // `F` only names the file an `E` belongs to; the line itself prints nothing.
            "F" => {}
            "error" => {
                report_error(rest, sink);
                return Ok(None);
            }
            other => {
                return Err(Error::aborted(
                    "init",
                    format!("unrecognized response from the server: {}", other),
                ))
            }
        }
    }
}

/// Read the answer to `init`: `ok` succeeds, `E` is printed, and `error` fails.
///
/// The stock server sends `E init may not be run remotely` followed by `error  ` with no
/// text; the empty error prints nothing and only sets the exit code to 1.
fn read_init_response(
    conn: &mut Connection,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    loop {
        let line = read_line(conn, cancel)?.ok_or_else(|| closed())?;
        let (name, rest) = split(&line);
        match name {
            "ok" => return Ok(0),
            "E" => sink.err(rest.to_string()),
            "M" => sink.out(rest.to_string()),
            "F" => {}
            "error" => {
                report_error(rest, sink);
                return Ok(1);
            }
            other => {
                return Err(Error::aborted(
                    "init",
                    format!("unrecognized response from the server: {}", other),
                ))
            }
        }
    }
}

/// The global options the reference forwards, in its order (client.c:4500-4553).
fn global_options(root: &Root) -> Vec<&'static str> {
    let flags = &root.options;
    let mut options = Vec::new();
    if flags.no_execute {
        options.push("-n");
    }
    if flags.quiet >= 2 {
        options.push("-Q");
    } else if flags.quiet == 1 {
        options.push("-q");
    }
    if flags.readonly {
        options.push("-r");
    }
    if flags.trace {
        options.push("-t");
    }
    options
}

fn supports(capabilities: &[String], name: &str) -> bool {
    capabilities.iter().any(|cap| cap == name)
}

fn split(line: &str) -> (&str, &str) {
    match line.find(' ') {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, ""),
    }
}

/// The text of an `error` response: the symbolic code is ignored, and a line with no text
/// (the stock server's trailing `error  `) prints nothing (client.c:928-962).
fn report_error(rest: &str, sink: &mut dyn Sink) {
    let message = match rest.find(' ') {
        Some(i) => &rest[i + 1..],
        None => "",
    };
    if !message.is_empty() {
        sink.err(message.to_string());
    }
}

fn read_line(conn: &mut Connection, cancel: &CancelToken) -> Result<Option<String>> {
    match conn.read_line() {
        Ok(Some(bytes)) => Ok(Some(codec::system_ansi().decode(&bytes))),
        Ok(None) => Ok(None),
        Err(error) => Err(io_error(cancel, error)),
    }
}

fn write_line(conn: &mut Connection, line: String, cancel: &CancelToken) -> Result<()> {
    conn.write_line(&line).map_err(|e| io_error(cancel, e))
}

fn flush(conn: &mut Connection, cancel: &CancelToken) -> Result<()> {
    conn.flush().map_err(|e| io_error(cancel, e))
}

fn closed() -> Error {
    Error::aborted("init", "the server closed the connection")
}

fn io_error(cancel: &CancelToken, error: io::Error) -> Error {
    if cancel.is_cancelled() {
        Error::interrupted("init")
    } else {
        Error::aborted("init", error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::engine::test_sink::{Channel, RecordingSink};
    use std::io::{Cursor, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Writer(Arc<Mutex<Vec<u8>>>);

    impl Write for Writer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn connection(replies: &str) -> (Connection, Arc<Mutex<Vec<u8>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let conn = Connection::from_parts_for_test(
            Box::new(Cursor::new(replies.as_bytes().to_vec())),
            Box::new(Writer(written.clone())),
        );
        (conn, written)
    }

    fn written_lines(written: &Arc<Mutex<Vec<u8>>>) -> Vec<String> {
        let bytes = written.lock().unwrap();
        String::from_utf8(bytes.clone())
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn remote_root() -> Root {
        Root::parse(":pserver:ann@host:/repo").unwrap()
    }

    #[test]
    fn startup_is_rootless_and_an_empty_error_adds_no_line() {
        let (conn, written) = connection(
            "Valid-requests Root Valid-responses Global_option init\nok\n\
             E init may not be run remotely\nerror  \n",
        );
        let mut sink = RecordingSink::default();
        let code = run_connected(conn, &remote_root(), &CancelToken::default(), &mut sink).unwrap();

        assert_eq!(code, 1);
        assert_eq!(
            sink.lines,
            vec![(Channel::Err, "init may not be run remotely".to_string())]
        );
        let lines = written_lines(&written);
        assert_eq!(lines[1], "valid-requests");
        assert_eq!(lines.last().unwrap(), "init /repo");
        assert!(
            !lines.iter().any(|line| line.starts_with("Root")),
            "{:?}",
            lines
        );
        assert_eq!(lines.len(), 3, "no extra startup requests: {:?}", lines);
    }

    #[test]
    fn a_successful_remote_init_exits_zero() {
        let (conn, _) = connection("Valid-requests Global_option init\nok\nok\n");
        let mut sink = RecordingSink::default();
        let code = run_connected(conn, &remote_root(), &CancelToken::default(), &mut sink).unwrap();
        assert_eq!(code, 0);
        assert!(sink.lines.is_empty(), "{:?}", sink.lines);
    }

    #[test]
    fn global_options_go_before_init_and_compression_is_skipped() {
        let (conn, written) = connection("Valid-requests Global_option init\nok\nok\n");
        let mut root = remote_root();
        root.options.no_execute = true;
        root.options.compression = 9;
        let mut sink = RecordingSink::default();
        let code = run_connected(conn, &root, &CancelToken::default(), &mut sink).unwrap();

        assert_eq!(code, 0);
        let lines = written_lines(&written);
        assert_eq!(
            lines,
            vec![
                format!("Valid-responses {}", VALID_RESPONSES.join(" ")),
                "valid-requests".to_string(),
                "Global_option -n".to_string(),
                "init /repo".to_string(),
            ]
        );
        assert!(!lines.iter().any(|line| line == "Gzip-stream 9"));
    }

    #[test]
    fn a_local_root_is_refused_before_any_connection() {
        let root = Root::parse(r"C:\repo").unwrap();
        let mut sink = RecordingSink::default();
        let error = run(root, &[], &CancelToken::default(), &mut sink).unwrap_err();
        assert!(
            error
                .text()
                .contains("creating a local repository is not supported"),
            "{}",
            error
        );
        assert!(sink.lines.is_empty());
    }

    #[test]
    fn arguments_print_usage_without_connecting() {
        let mut sink = RecordingSink::default();
        let code = run(
            remote_root(),
            &["extra".to_string()],
            &CancelToken::default(),
            &mut sink,
        )
        .unwrap();
        assert_eq!(code, 1);
        assert_eq!(
            sink.lines,
            vec![
                (Channel::Err, "Usage: cvs init".to_string()),
                (
                    Channel::Err,
                    "(Specify the --help global option for a list of other help options)"
                        .to_string()
                ),
            ]
        );
    }
}
