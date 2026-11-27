//! The `cvs` program as a program: its command line, and a whole pserver session.
//!
//! The session test plays the server itself, so it needs no account, no network, and no
//! external `cvs.exe`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex};

use cvsdesk::client::passfile::scramble;
use cvsdesk::client::work::codec;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

/// `CVS_PASSFILE` is process-wide; tests that use it take turns.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn cvs() -> &'static str {
    env!("CARGO_BIN_EXE_cvs")
}

/// What the server sees for the test password; it unscrambles it before checking.
fn scrambled_secret() -> String {
    String::from_utf8(scramble(b"secret")).unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cvsdesk-cli-{}-{}-{}",
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

#[test]
fn version_needs_no_server_and_names_the_protocol_it_speaks() {
    let out = Command::new(cvs()).arg("--version").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Concurrent Versions System"), "{}", text);
    assert!(text.contains("1.11.1.3"), "{}", text);
}

#[test]
fn help_commands_lists_the_commands_that_exist() {
    let out = Command::new(cvs()).arg("--help-commands").output().unwrap();
    // CVS itself exits 1 for help, so callers that ignore the code still see the text.
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    for name in ["login", "logout", "noop", "version"] {
        assert!(text.contains(name), "{} missing from:\n{}", name, text);
    }
}

#[test]
fn a_pserver_noop_goes_through_the_real_handshake() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "BEGIN AUTH REQUEST");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "/repo");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "ann");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), scrambled_secret());
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "END AUTH REQUEST");
        writer.write_all(b"I LOVE YOU\n").unwrap();

        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("Root /repo"), "{}", line);
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("Valid-responses "), "{}", line);
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "valid-requests");
        writer
            .write_all(b"Valid-requests Root Valid-responses noop\nok\n")
            .unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "UseUnchanged");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "noop");
        writer.write_all(b"ok\n").unwrap();
        writer.flush().unwrap();
        tx.send(()).unwrap();
    });

    let dir = scratch("pserver");
    let passfile = dir.join(".cvspass");
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let mut line = root.as_bytes().to_vec();
    line.push(b' ');
    line.extend_from_slice(&scramble(b"secret"));
    line.push(b'\n');
    std::fs::write(&passfile, line).unwrap();

    let out = Command::new(cvs())
        .env("CVS_PASSFILE", &passfile)
        .args(["-d", &root, "noop"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    rx.recv().unwrap();
    server.join().unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_command_with_no_root_says_so_and_fails() {
    let dir = scratch("no-root");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .arg("noop")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("No CVSROOT specified"), "{}", text);
    std::fs::remove_dir_all(&dir).ok();
}

/// Read one protocol line from the fake server's client. A closed pipe ends the test with a
/// clear message: without this, a loop that waits for a request the client never sends would
/// read `""` forever and grow its line list until the process runs out of memory.
fn line<R: BufRead>(reader: &mut R) -> String {
    let mut text = String::new();
    let read = reader.read_line(&mut text).unwrap();
    assert!(
        read != 0,
        "the client closed the connection before sending the expected line"
    );
    text.trim_end_matches(['\r', '\n']).to_string()
}

/// Accept one connection, or give up after ten seconds so a client that never connects (a
/// command this build does not dispatch yet) fails its assertion instead of hanging the run.
fn accept_within(listener: &TcpListener) -> Option<std::net::TcpStream> {
    accept_for(listener, std::time::Duration::from_secs(10))
}

fn accept_for(listener: &TcpListener, wait: std::time::Duration) -> Option<std::net::TcpStream> {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + wait;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // Windows hands back a socket that inherited the listener's non-blocking flag.
                stream.set_nonblocking(false).unwrap();
                return Some(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => panic!("accept failed: {}", e),
        }
    }
}

#[test]
fn checkout_writes_files_entries_and_times_the_way_cvs_does() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;

        let password = scrambled_secret();
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

        assert!(line(&mut reader).starts_with("Root /repo"));
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(b"Valid-requests Root Valid-responses Case Utf8 expand-modules\nok\n")
            .unwrap();
        assert_eq!(line(&mut reader), "UseUnchanged");
        assert_eq!(line(&mut reader), "Case");
        assert_eq!(line(&mut reader), "Utf8");

        assert_eq!(line(&mut reader), "Argument mod");
        assert_eq!(line(&mut reader), "Directory .");
        assert_eq!(line(&mut reader), "/repo");
        assert_eq!(line(&mut reader), "expand-modules");
        writer.write_all(b"Module-expansion mod\nok\n").unwrap();

        assert_eq!(line(&mut reader), "Argument -N");
        assert_eq!(line(&mut reader), "Argument --");
        assert_eq!(line(&mut reader), "Argument mod");
        assert_eq!(line(&mut reader), "Directory .");
        assert_eq!(line(&mut reader), "/repo");
        assert_eq!(line(&mut reader), "co");

        let mut out = Vec::new();
        out.extend_from_slice(b"E cvs server: Updating mod\n");
        // Both files get the same recorded time; the client must write it as UTC asctime.
        out.extend_from_slice(b"Mod-time 01 Jan 2020 00:00:00 -0000\n");
        out.extend_from_slice(b"M U mod/hello.txt\n");
        out.extend_from_slice(
            b"Created mod/\n/repo/mod/hello.txt\n/hello.txt/1.1///\nu=rw,g=rw,o=rw\n7\none\ntwo",
        );
        out.extend_from_slice(b"Mod-time 01 Jan 2020 00:00:00 -0000\n");
        out.extend_from_slice(b"M U mod/bin.dat\n");
        out.extend_from_slice(
            b"Created mod/\n/repo/mod/bin.dat\n/bin.dat/1.1//-kb/\nu=rw,g=rw,o=rw\n3\na\nb",
        );
        out.extend_from_slice(b"ok\n");
        writer.write_all(&out).unwrap();
        writer.flush().unwrap();
    });

    let dir = scratch("checkout");
    let passfile = dir.join(".cvspass");
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let out = Command::new(cvs())
        .env("CVS_PASSFILE", &passfile)
        .current_dir(&dir)
        .args(["-d", &root, "checkout", "mod"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.matches("U mod/hello.txt").count(), 1, "{}", stdout);
    assert_eq!(stdout.matches("U mod/bin.dat").count(), 1, "{}", stdout);

    let text = std::fs::read(dir.join("mod").join("hello.txt")).unwrap();
    let expected: &[u8] = if cfg!(windows) {
        b"one\r\ntwo"
    } else {
        b"one\ntwo"
    };
    assert_eq!(text, expected, "text files get the platform line ending");
    let binary = std::fs::read(dir.join("mod").join("bin.dat")).unwrap();
    assert_eq!(binary, b"a\nb", "a -kb file is written byte for byte");

    let entries = std::fs::read_to_string(dir.join("mod").join("CVS").join("Entries")).unwrap();
    let entries = entries.replace("\r\n", "\n");
    assert!(
        entries.contains("/hello.txt/1.1/Wed Jan  1 00:00:00 2020//"),
        "{}",
        entries
    );
    assert!(
        entries.contains("/bin.dat/1.1/Wed Jan  1 00:00:00 2020/-kb/"),
        "{}",
        entries
    );
    let repository =
        std::fs::read_to_string(dir.join("mod").join("CVS").join("Repository")).unwrap();
    assert_eq!(repository.trim_end(), "mod");
    let cvs_root = std::fs::read_to_string(dir.join("mod").join("CVS").join("Root")).unwrap();
    assert_eq!(cvs_root.trim_end(), root);

    server.join().unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_checkout_destination_renames_the_top_folder_only_on_disk() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        for _ in 0..5 {
            line(&mut reader);
        }
        writer.write_all(b"I LOVE YOU\n").unwrap();
        for _ in 0..3 {
            line(&mut reader);
        }
        writer
            .write_all(b"Valid-requests Root Valid-responses expand-modules\nok\n")
            .unwrap();
        assert_eq!(line(&mut reader), "UseUnchanged");
        for _ in 0..4 {
            line(&mut reader);
        }
        writer.write_all(b"Module-expansion mod\nok\n").unwrap();
        for _ in 0..5 {
            line(&mut reader);
        }
        writer
            .write_all(
                b"M U mod/a.txt\nCreated mod/\n/repo/mod/a.txt\n/a.txt/1.1///\nu=rw,g=rw,o=rw\n2\na\nok\n",
            )
            .unwrap();
        writer.flush().unwrap();
    });

    let dir = scratch("destination");
    let passfile = dir.join(".cvspass");
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let out = Command::new(cvs())
        .env("CVS_PASSFILE", &passfile)
        .current_dir(&dir)
        .args(["-d", &root, "checkout", "-d", "out", "mod"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("out").join("a.txt").is_file());
    assert!(!dir.join("mod").exists());
    let repository =
        std::fs::read_to_string(dir.join("out").join("CVS").join("Repository")).unwrap();
    assert_eq!(
        repository.trim_end(),
        "mod",
        "the repository path is not renamed"
    );

    server.join().unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

/// Run one repository query against a fake pserver and return its output together with the
/// protocol lines the client sent after the opening handshake.
///
/// The query always runs in a fresh folder with no `CVS` in it: `rlog`, `rannotate`, `init`,
/// `history` and `rdiff` need no working copy. The fake server offers no capabilities beyond
/// `Root` and `Valid-responses`, so the only prologue line is `UseUnchanged`.
///
/// A client that never connects (a command this build does not implement yet) is reported as
/// no lines at all after ten seconds, so a broken command fails the assertion instead of
/// hanging the test run.
fn repo_query(
    name: &str,
    args: &[&str],
    reply: &'static [u8],
) -> (std::process::Output, Vec<String>) {
    let rootless = args.first().is_some_and(|arg| *arg == "init");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let Some(stream) = accept_within(&listener) else {
            return Vec::new();
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let password = scrambled_secret();
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
        // `init` is rootless: the reference sends no `Root` before it (client.c:4461-4467).
        if !rootless {
            assert!(line(&mut reader).starts_with("Root /repo"));
        }
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(
                b"Valid-requests Root Valid-responses rlog rannotate history rdiff init\nok\n",
            )
            .unwrap();

        let mut lines = Vec::new();
        loop {
            let text = line(&mut reader);
            // `init` carries the repository path on its own request line; the others are the
            // bare command name.
            let done = matches!(text.as_str(), "rlog" | "rannotate" | "history" | "rdiff")
                || text.starts_with("init ");
            lines.push(text);
            if done {
                break;
            }
        }
        writer.write_all(reply).unwrap();
        writer.flush().unwrap();
        lines
    });

    let dir = scratch(&format!("query-{}", name));
    assert!(
        !dir.join("CVS").exists(),
        "the query must not need a working copy"
    );
    let pass_dir = scratch(&format!("query-{}-pass", name));
    let passfile = pass_dir.join(".cvspass");
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let out = Command::new(cvs())
        .env("CVS_PASSFILE", &passfile)
        .current_dir(&dir)
        .args(["-d", &root])
        .args(args)
        .output()
        .unwrap();
    let lines = server.join().unwrap();
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&pass_dir).ok();
    (out, lines)
}

/// The reference's text for a local midnight, in UTC: `31 Dec 2019 16:00:00 -0000`.
fn local_midnight(year: i32, month: u32, day: u32) -> String {
    use chrono::{Local, NaiveDate, TimeZone};
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let utc = Local
        .from_local_datetime(&naive)
        .single()
        .unwrap()
        .with_timezone(&chrono::Utc);
    format!("{} -0000", utc.format("%-d %b %Y %H:%M:%S"))
}

fn text(output: &[u8]) -> String {
    String::from_utf8_lossy(output).replace("\r\n", "\n")
}

#[test]
fn rlog_sends_the_module_and_nothing_else() {
    let (out, lines) = repo_query("rlog-default", &["rlog", "mod"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        ["UseUnchanged", "Argument --", "Argument mod", "rlog"]
    );
}

#[test]
fn rlog_sends_n_alone() {
    let (out, lines) = repo_query("rlog-n", &["rlog", "-N", "mod"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "UseUnchanged",
            "Argument -N",
            "Argument --",
            "Argument mod",
            "rlog"
        ]
    );
}

#[test]
fn rlog_widens_one_revision_to_a_range() {
    let (out, lines) = repo_query("rlog-r", &["rlog", "-r1.1", "mod"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "UseUnchanged",
            "Argument -r1.1:1.1",
            "Argument --",
            "Argument mod",
            "rlog"
        ]
    );
}

#[test]
fn rlog_sends_a_date_as_its_own_argument() {
    let (out, lines) = repo_query("rlog-d", &["rlog", "-d", "2020-01-01", "mod"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        vec![
            "UseUnchanged".to_string(),
            "Argument -d".to_string(),
            format!("Argument {}", local_midnight(2020, 1, 1)),
            "Argument --".to_string(),
            "Argument mod".to_string(),
            "rlog".to_string(),
        ]
    );
}

#[test]
fn rannotate_sends_a_revision_as_two_arguments() {
    let (out, lines) = repo_query("rannotate-r", &["rannotate", "-r", "1.1", "mod"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "UseUnchanged",
            "Argument -r",
            "Argument 1.1",
            "Argument --",
            "Argument mod",
            "rannotate"
        ]
    );
}

#[test]
fn rannotate_sends_a_date_as_its_own_argument() {
    let (out, lines) = repo_query(
        "rannotate-d",
        &["rannotate", "-D", "2020-01-01", "mod"],
        b"ok\n",
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        vec![
            "UseUnchanged".to_string(),
            "Argument -D".to_string(),
            format!("Argument {}", local_midnight(2020, 1, 1)),
            "Argument --".to_string(),
            "Argument mod".to_string(),
            "rannotate".to_string(),
        ]
    );
}

#[test]
fn rannotate_r_is_client_side_only() {
    let (out, lines) = repo_query("rannotate-R", &["rannotate", "-R", "mod"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        ["UseUnchanged", "Argument --", "Argument mod", "rannotate"]
    );
}

#[test]
fn a_repository_query_routes_m_to_stdout_and_e_to_stderr() {
    let (out, lines) = repo_query(
        "query-channels",
        &["rlog", "mod"],
        b"M a history line\nE a server note\nok\n",
    );
    assert_eq!(
        lines,
        ["UseUnchanged", "Argument --", "Argument mod", "rlog"]
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("a history line"),
        "{}",
        text(&out.stdout)
    );
    assert!(
        text(&out.stderr).contains("a server note"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn a_server_error_response_fails_the_query() {
    let (out, _) = repo_query(
        "query-error",
        &["rannotate", "mod"],
        b"error 1 no such module\n",
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("no such module"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn rlog_with_no_arguments_reports_the_missing_module() {
    // The reference client sends the bare `rlog` request and the server answers the missing
    // module (log.c:239-240 only prints usage for a `-H` style call, not for an empty list).
    let (out, lines) = repo_query("rlog-noargs", &["rlog"], b"error 1 no modules specified\n");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("no modules specified"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(lines, ["UseUnchanged", "Argument --", "rlog"]);
}

#[test]
fn help_names_the_repository_queries_with_their_reference_summaries_and_synonyms() {
    let commands = Command::new(cvs()).arg("--help-commands").output().unwrap();
    let commands = text(&commands.stdout);
    assert!(
        commands.contains("rannotate    Show last revision where each line of module was modified"),
        "{}",
        commands
    );
    assert!(
        commands.contains("rlog         Print out history information for a module"),
        "{}",
        commands
    );

    let synonyms = Command::new(cvs()).arg("--help-synonyms").output().unwrap();
    let synonyms = text(&synonyms.stdout);
    assert!(
        synonyms.contains("        rannotate     rann ra"),
        "{}",
        synonyms
    );
    assert!(
        synonyms.contains("        rlog          rl"),
        "{}",
        synonyms
    );

    let help = Command::new(cvs())
        .args(["-H", "rannotate"])
        .output()
        .unwrap();
    let help = text(&help.stdout);
    assert!(
        help.starts_with("Usage: cvs rannotate [-lRfF] [-r rev] [-D date] [files...]\n\t-l\tLocal directory only, no recursion."),
        "{}",
        help
    );
    assert!(
        help.ends_with("(Specify the --help global option for a list of other help options)\n"),
        "{}",
        help
    );

    let help = Command::new(cvs()).args(["-H", "rlog"]).output().unwrap();
    let help = text(&help.stdout);
    assert!(
        help.starts_with("Usage: cvs rlog [-lRhtNb] [-r[revisions]] [-d dates] [-s states]\n    [-w[logins]] [files...]\n"),
        "{}",
        help
    );
    assert!(
        help.contains("\t-r[revisions]\tA comma-separated list of revisions to print:"),
        "{}",
        help
    );
    assert!(
        help.ends_with("(Specify the --help global option for a list of other help options)\n"),
        "{}",
        help
    );
}

/// A fresh working copy of module `mod` with `hello.txt` at revision 1.1.
fn working_copy(name: &str, root: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::create_dir_all(dir.join("CVS")).unwrap();
    std::fs::write(dir.join("CVS").join("Root"), format!("{}\n", root)).unwrap();
    std::fs::write(dir.join("CVS").join("Repository"), "mod\n").unwrap();
    std::fs::write(
        dir.join("CVS").join("Entries"),
        "/hello.txt/1.1/Tue Nov 24 09:52:53 2026//\nD\n",
    )
    .unwrap();
    std::fs::write(dir.join("hello.txt"), "hello\n").unwrap();
    dir
}

/// Give `name` an entry whose date is the file's own time, so a state line says `Unchanged`.
fn record_current_time(dir: &Path, name: &str, revision: &str) {
    let date = codec::asctime(
        std::fs::metadata(dir.join(name))
            .unwrap()
            .modified()
            .unwrap(),
    );
    std::fs::write(
        dir.join("CVS").join("Entries"),
        format!("/{}/{}/{date}//\nD\n", name, revision),
    )
    .unwrap();
}

/// A versioned subfolder with its own repository path and index, for nested tests.
fn versioned_subfolder(dir: &Path, name: &str, repository: &str, entries: &str) {
    let folder = dir.join(name);
    std::fs::create_dir_all(folder.join("CVS")).unwrap();
    std::fs::write(
        folder.join("CVS").join("Repository"),
        format!("{}\n", repository),
    )
    .unwrap();
    std::fs::write(folder.join("CVS").join("Entries"), entries).unwrap();
}

/// Run one working-copy command against a fake pserver.
///
/// The fake server offers `Case` and `Utf8`, reads the opening requests, then collects every
/// line up to the command name and writes `reply`. The work folder is filled by `prepare` and
/// handed back so a test can inspect what the command wrote.
fn working_session(
    name: &str,
    args: &[&str],
    reply: &'static [u8],
    prepare: impl FnOnce(&Path),
) -> (std::process::Output, Vec<String>, PathBuf, String) {
    working_session_with(name, args, "Case Utf8", reply, prepare)
}

/// The same, but with the capabilities this fake server offers. The client reads them to
/// decide which optional prologue lines and folder markers it may send (`Case`, `Utf8`,
/// `Sticky`, `Static-directory`).
fn working_session_with(
    name: &str,
    args: &[&str],
    capabilities: &'static str,
    reply: &'static [u8],
    prepare: impl FnOnce(&Path),
) -> (std::process::Output, Vec<String>, PathBuf, String) {
    session_run(name, args, capabilities, reply, prepare)
}

fn session_run(
    name: &str,
    args: &[&str],
    capabilities: &'static str,
    reply: &'static [u8],
    prepare: impl FnOnce(&Path),
) -> (std::process::Output, Vec<String>, PathBuf, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let server = std::thread::spawn(move || {
        let Some(stream) = accept_within(&listener) else {
            return Vec::new();
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let password = scrambled_secret();
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
        assert!(line(&mut reader).starts_with("Root /repo"));
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(
                format!("Valid-requests Root Valid-responses {}\nok\n", capabilities).as_bytes(),
            )
            .unwrap();
        assert_eq!(line(&mut reader), "UseUnchanged");
        if capabilities.contains("Case") {
            assert_eq!(line(&mut reader), "Case");
        }
        if capabilities.contains("Utf8") {
            assert_eq!(line(&mut reader), "Utf8");
        }
        let mut lines = Vec::new();
        loop {
            let text = line(&mut reader);
            let done = matches!(
                text.as_str(),
                "add"
                    | "remove"
                    | "tag"
                    | "rtag"
                    | "update"
                    | "diff"
                    | "ci"
                    | "admin"
                    | "release"
                    | "import"
                    | "noop"
            );
            lines.push(text);
            if done {
                break;
            }
        }
        writer.write_all(reply).unwrap();
        writer.flush().unwrap();
        lines
    });

    let dir = working_copy(name, &root);
    prepare(&dir);

    let pass_dir = scratch(&format!("{}-pass", name));
    let passfile = pass_dir.join(".cvspass");
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let mut command = Command::new(cvs());
    command
        .env("CVS_PASSFILE", &passfile)
        .current_dir(&dir)
        .args(["-d", &root])
        .args(args);
    let out = command.output().unwrap();
    let lines = server.join().unwrap();
    std::fs::remove_dir_all(&pass_dir).ok();
    (out, lines, dir, root)
}

/// The Entries text with CRLF folded to LF, so an assertion reads the same on every platform.
fn entries_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("CVS").join("Entries"))
        .unwrap()
        .replace("\r\n", "\n")
}

const ADD_NEW_OK: &[u8] = b"E cvs server: scheduling file `new.txt' for addition\n\
Checked-in ./\n/repo/mod/new.txt\n/new.txt/0///\n\
E cvs server: use 'cvs commit' to add this file permanently\nok\n";
const ADD_KB_OK: &[u8] = b"E cvs server: scheduling file `bin.dat' for addition\n\
Checked-in ./\n/repo/mod/bin.dat\n/bin.dat/0//-kb/\n\
E cvs server: use 'cvs commit' to add this file permanently\nok\n";
const ADD_MSG_OK: &[u8] = b"E cvs server: scheduling file `msg.txt' for addition\n\
Checked-in ./\n/repo/mod/msg.txt\n/msg.txt/0///\n\
E cvs server: use 'cvs commit' to add this file permanently\nok\n";
const ADD_FILE_OK: &[u8] = b"E cvs server: scheduling file `file.txt' for addition\n\
Checked-in dir1/\n/repo/mod/dir1/file.txt\n/file.txt/0///\n\
E cvs server: use 'cvs commit' to add this file permanently\nok\n";
const ADD_DIR_OK: &[u8] = b"M Directory /repo/mod/dir1 added to the repository\nok\n";
const REMOVE_OK: &[u8] = b"E cvs server: scheduling `hello.txt' for removal\n\
Checked-in ./\n/repo/mod/hello.txt\n/hello.txt/-1.1///\n\
E cvs server: use 'cvs commit' to remove this file permanently\nok\n";

#[test]
fn add_schedules_a_file_and_writes_the_dummy_timestamp() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("add-file", &["add", "new.txt"], ADD_NEW_OK, |dir| {
            std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        });
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Is-modified new.txt",
            "Argument new.txt",
            "add"
        ]
    );
    assert!(text(&out.stderr).contains("scheduling file `new.txt' for addition"));
    assert!(text(&out.stderr).contains("use 'cvs commit' to add this file permanently"));
    let entries = entries_text(&dir);
    assert!(
        entries.contains("/new.txt/0/dummy timestamp//"),
        "{}",
        entries
    );
    assert!(entries.trim_end().ends_with("D"), "{}", entries);
    assert!(
        dir.join("new.txt").is_file(),
        "add does not send the contents"
    );
    if cfg!(windows) {
        let raw = std::fs::read(dir.join("CVS").join("Entries")).unwrap();
        assert!(
            raw.windows(2).any(|pair| pair == b"\r\n"),
            "Entries keeps CRLF"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn add_kb_sends_the_combined_kflag_and_records_it() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("add-kb", &["add", "-kb", "bin.dat"], ADD_KB_OK, |dir| {
            std::fs::write(dir.join("bin.dat"), b"a\nb").unwrap();
        });
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -kb",
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Is-modified bin.dat",
            "Argument bin.dat",
            "add"
        ]
    );
    assert!(
        entries_text(&dir).contains("/bin.dat/0/dummy timestamp/-kb/"),
        "{}",
        entries_text(&dir)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn add_m_sends_the_message_as_its_own_argument() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "add-m",
        &["add", "-m", "creation log", "msg.txt"],
        ADD_MSG_OK,
        |dir| {
            std::fs::write(dir.join("msg.txt"), "msg\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -m",
            "Argument creation log",
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Is-modified msg.txt",
            "Argument msg.txt",
            "add"
        ]
    );
    assert!(
        entries_text(&dir).contains("/msg.txt/0/dummy timestamp//"),
        "{}",
        entries_text(&dir)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn add_directory_creates_its_cvs_files_and_parent_entry() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, root) = working_session("add-dir", &["add", "dir1"], ADD_DIR_OK, |dir| {
        std::fs::create_dir_all(dir.join("dir1")).unwrap();
        std::fs::write(dir.join("dir1").join("file.txt"), "nested\n").unwrap();
    });
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory dir1",
            "/repo/mod/dir1",
            "Questionable file.txt",
            "Directory .",
            "/repo/mod",
            "Argument dir1",
            "add"
        ]
    );
    assert!(text(&out.stdout).contains("added to the repository"));
    let entries = entries_text(&dir);
    assert!(entries.contains("D/dir1////"), "{}", entries);
    assert_eq!(
        std::fs::read_to_string(dir.join("dir1").join("CVS").join("Entries"))
            .unwrap()
            .replace("\r\n", "\n"),
        "D\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("dir1").join("CVS").join("Repository"))
            .unwrap()
            .trim_end(),
        "mod/dir1"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("dir1").join("CVS").join("Root"))
            .unwrap()
            .trim_end(),
        root
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn add_nested_file_names_both_folders_and_returns_to_the_top() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "add-nested",
        &["add", "dir1/file.txt"],
        ADD_FILE_OK,
        |dir| {
            std::fs::create_dir_all(dir.join("dir1").join("CVS")).unwrap();
            std::fs::write(
                dir.join("dir1").join("CVS").join("Repository"),
                "mod/dir1\n",
            )
            .unwrap();
            std::fs::write(dir.join("dir1").join("CVS").join("Entries"), "").unwrap();
            std::fs::write(dir.join("dir1").join("file.txt"), "nested\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Directory dir1",
            "/repo/mod/dir1",
            "Is-modified file.txt",
            "Directory .",
            "/repo/mod",
            "Argument dir1/file.txt",
            "add"
        ]
    );
    let entries = std::fs::read_to_string(dir.join("dir1").join("CVS").join("Entries"))
        .unwrap()
        .replace("\r\n", "\n");
    assert!(
        entries.contains("/file.txt/0/dummy timestamp//"),
        "{}",
        entries
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remove_sends_the_entry_and_an_unchanged_state() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("remove-file", &["remove", "hello.txt"], REMOVE_OK, |dir| {
            record_current_time(dir, "hello.txt", "1.1")
        });
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "Argument hello.txt",
            "remove"
        ]
    );
    assert!(dir.join("hello.txt").is_file(), "without -f the file stays");
    let entries = entries_text(&dir);
    assert!(
        entries.contains("/hello.txt/-1.1/dummy timestamp//"),
        "{}",
        entries
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remove_f_deletes_the_file_and_sends_no_state() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "remove-f",
        &["remove", "-f", "hello.txt"],
        REMOVE_OK,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Argument hello.txt",
            "remove"
        ]
    );
    assert!(
        !dir.join("hello.txt").exists(),
        "-f deletes the working file"
    );
    assert!(
        entries_text(&dir).contains("/hello.txt/-1.1/dummy timestamp//"),
        "{}",
        entries_text(&dir)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remove_leaves_the_entry_when_the_server_still_sees_the_file() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "remove-still",
        &["remove", "hello.txt"],
        b"E cvs server: file `hello.txt' still in working directory\n\
E cvs server: 1 file exists; remove it first\nok\n",
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "the reference exits 0 here");
    assert!(lines.contains(&"Entry /hello.txt/1.1///".to_string()));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("file `hello.txt' still in working directory"),
        "{}",
        stderr
    );
    assert!(
        stderr.contains("1 file exists; remove it first"),
        "{}",
        stderr
    );
    let entries = entries_text(&dir);
    assert!(!entries.contains("-1.1"), "{}", entries);
    std::fs::remove_dir_all(&dir).ok();
}

/// Run a command that must fail before any connection is attempted.
fn failing_command(
    name: &str,
    args: &[&str],
    prepare: impl FnOnce(&Path),
) -> (std::process::Output, PathBuf) {
    let dir = working_copy(name, ":pserver:ann@127.0.0.1:1:/repo");
    prepare(&dir);
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args(["-d", ":pserver:ann@127.0.0.1:1:/repo"])
        .args(args)
        .output()
        .unwrap();
    (out, dir)
}

#[test]
fn add_a_missing_file_says_nothing_known_about_it() {
    let (out, dir) = failing_command("add-missing", &["add", "missing.txt"], |_| {});
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("nothing known about missing.txt"),
        "{}",
        stderr
    );
    assert!(!entries_text(&dir).contains("missing.txt"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn add_a_tracked_file_names_its_version() {
    let (out, dir) = failing_command("add-tracked", &["add", "hello.txt"], |_| {});
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("hello.txt already exists, with version number 1.1"),
        "{}",
        stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

const REMOVE_UNKNOWN: &[u8] = b"E cvs server: nothing known about `loose.txt'\nok\n";

#[test]
fn remove_f_an_untracked_file_is_sent_without_an_entry() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "remove-untracked-f",
        &["remove", "-f", "loose.txt"],
        REMOVE_UNKNOWN,
        |dir| {
            std::fs::write(dir.join("loose.txt"), "loose\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Argument loose.txt",
            "remove"
        ]
    );
    assert!(
        !dir.join("loose.txt").exists(),
        "-f deletes the untracked file"
    );
    assert!(
        text(&out.stderr).contains("nothing known about `loose.txt'"),
        "{}",
        text(&out.stderr)
    );
    assert!(!entries_text(&dir).contains("loose.txt"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remove_an_untracked_file_is_sent_and_the_server_answers() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "remove-untracked",
        &["remove", "loose.txt"],
        b"E cvs server: file `loose.txt' still in working directory\n\
E cvs server: 1 file exists; remove it first\nok\n",
        |dir| {
            std::fs::write(dir.join("loose.txt"), "loose\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Modified loose.txt",
            "u=rw,g=rw,o=rw",
            "6",
            "loose",
            "Argument loose.txt",
            "remove"
        ]
    );
    assert!(dir.join("loose.txt").is_file(), "without -f the file stays");
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("file `loose.txt' still in working directory"),
        "{}",
        stderr
    );
    assert!(
        stderr.contains("1 file exists; remove it first"),
        "{}",
        stderr
    );
    assert!(!entries_text(&dir).contains("loose.txt"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remove_f_an_untracked_file_in_a_subfolder_uses_its_block() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "remove-untracked-sub",
        &["remove", "-f", "sub/loose.txt"],
        b"E cvs server: nothing known about `loose.txt'\nok\n",
        |dir| {
            versioned_subfolder(dir, "sub", "mod/sub", "");
            std::fs::write(dir.join("sub").join("loose.txt"), "loose\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory sub",
            "/repo/mod/sub",
            "Directory .",
            "/repo/mod",
            "Argument sub/loose.txt",
            "remove"
        ]
    );
    assert!(
        !dir.join("sub").join("loose.txt").exists(),
        "-f deletes the file in its own folder"
    );
    assert!(
        text(&out.stderr).contains("nothing known about `loose.txt'"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        !entries_text(&dir).contains("sub/loose.txt"),
        "{}",
        entries_text(&dir)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remove_f_deletes_untracked_files_in_two_folders() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "remove-untracked-two",
        &["remove", "-f", "sub1/a.txt", "sub2/b.txt"],
        b"E cvs server: nothing known about `a.txt'\n\
E cvs server: nothing known about `b.txt'\nok\n",
        |dir| {
            versioned_subfolder(dir, "sub1", "mod/sub1", "");
            versioned_subfolder(dir, "sub2", "mod/sub2", "");
            std::fs::write(dir.join("sub1").join("a.txt"), "a\n").unwrap();
            std::fs::write(dir.join("sub2").join("b.txt"), "b\n").unwrap();
            std::fs::write(dir.join("sub1").join("keep1.txt"), "keep\n").unwrap();
            std::fs::write(dir.join("sub2").join("keep2.txt"), "keep\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory sub1",
            "/repo/mod/sub1",
            "Directory sub2",
            "/repo/mod/sub2",
            "Directory .",
            "/repo/mod",
            "Argument sub1/a.txt",
            "Argument sub2/b.txt",
            "remove"
        ]
    );
    assert!(!dir.join("sub1").join("a.txt").exists());
    assert!(!dir.join("sub2").join("b.txt").exists());
    assert!(
        dir.join("sub1").join("keep1.txt").is_file(),
        "another file in the folder must stay"
    );
    assert!(dir.join("sub2").join("keep2.txt").is_file());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remove_an_untracked_file_in_a_subfolder_sends_modified() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "remove-untracked-sub-no-f",
        &["remove", "sub/loose.txt"],
        b"E cvs server: file `loose.txt' still in working directory\n\
E cvs server: 1 file exists; remove it first\nok\n",
        |dir| {
            versioned_subfolder(dir, "sub", "mod/sub", "");
            std::fs::write(dir.join("sub").join("loose.txt"), "loose\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory sub",
            "/repo/mod/sub",
            "Modified loose.txt",
            "u=rw,g=rw,o=rw",
            "6",
            "loose",
            "Directory .",
            "/repo/mod",
            "Argument sub/loose.txt",
            "remove"
        ]
    );
    assert!(dir.join("sub").join("loose.txt").is_file());
    assert!(
        text(&out.stderr).contains("still in working directory"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_loose_checked_in_keeps_the_folders_other_entries() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, _, dir, _) = working_session(
        "remove-loose-checkedin",
        &["remove", "-f", "sub/loose.txt"],
        b"Checked-in sub/\n/repo/mod/sub/loose.txt\n/loose.txt/-1.1///\nok\n",
        |dir| {
            versioned_subfolder(
                dir,
                "sub",
                "mod/sub",
                "/kept.txt/1.1/Tue Nov 24 09:52:53 2026//\nD\n",
            );
            std::fs::write(dir.join("sub").join("loose.txt"), "loose\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let entries = std::fs::read_to_string(dir.join("sub").join("CVS").join("Entries"))
        .unwrap()
        .replace("\r\n", "\n");
    assert!(entries.contains("/kept.txt/1.1/"), "{}", entries);
    assert!(
        entries.contains("/loose.txt/-1.1/dummy timestamp//"),
        "{}",
        entries
    );
    assert!(entries.trim_end().ends_with("D"), "{}", entries);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_server_error_leaves_the_entries_alone() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, _, dir, _) = working_session(
        "add-error",
        &["add", "new.txt"],
        b"E cvs server: scheduling file `new.txt' for addition\nerror 1 add failed\n",
        |dir| {
            std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        },
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("add failed"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        !entries_text(&dir).contains("new.txt"),
        "a failed add must not write the index:\n{}",
        entries_text(&dir)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn help_names_add_and_remove_with_the_reference_text() {
    let commands = Command::new(cvs()).arg("--help-commands").output().unwrap();
    let commands = text(&commands.stdout);
    assert!(
        commands.contains("        add          Add a new file/directory to the repository"),
        "{}",
        commands
    );
    assert!(
        commands.contains("        remove       Remove an entry from the repository"),
        "{}",
        commands
    );

    let synonyms = Command::new(cvs()).arg("--help-synonyms").output().unwrap();
    let synonyms = text(&synonyms.stdout);
    assert!(
        synonyms.contains("        add           ad new"),
        "{}",
        synonyms
    );
    assert!(
        synonyms.contains("        remove        rm delete"),
        "{}",
        synonyms
    );

    let add_help = Command::new(cvs()).args(["-H", "add"]).output().unwrap();
    let add_help = text(&add_help.stdout);
    assert_eq!(
        add_help.trim_end_matches('\n'),
        "Usage: cvs add [-k rcs-kflag] [-m message] files...\n\
         \t-k rcs-kflag\tUse \"rcs-kflag\" to add the file with the specified\n\
         \t\t\tkflag.\n\
         \t-m message\tUse \"message\" for the creation log.\n\
         (Specify the --help global option for a list of other help options)"
    );

    let remove_help = Command::new(cvs()).args(["-H", "remove"]).output().unwrap();
    let remove_help = text(&remove_help.stdout);
    assert_eq!(
        remove_help.trim_end_matches('\n'),
        "Usage: cvs remove [-flR] [files...]\n\
         \t-f\tDelete the file before removing it.\n\
         \t-l\tProcess this directory only (not recursive).\n\
         \t-R\tProcess directories recursively.\n\
         (Specify the --help global option for a list of other help options)"
    );
}
#[test]
fn remove_f_of_an_added_file_drops_the_entry() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "remove-added",
        &["remove", "-f", "new.txt"],
        b"E cvs server: removed `new.txt'\nRemove-entry ./\n/repo/mod/new.txt\nok\n",
        |dir| {
            std::fs::write(dir.join("new.txt"), "new\n").unwrap();
            std::fs::write(
                dir.join("CVS").join("Entries"),
                "/new.txt/0/dummy timestamp//\nD\n",
            )
            .unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /new.txt/0///",
            "Argument new.txt",
            "remove"
        ]
    );
    assert!(!dir.join("new.txt").exists());
    let entries = entries_text(&dir);
    assert!(!entries.contains("new.txt"), "{}", entries);
    std::fs::remove_dir_all(&dir).ok();
}

/// A working copy whose one file is unchanged, so `tag` sends `Unchanged`.
fn unchanged_file(dir: &Path) {
    record_current_time(dir, "hello.txt", "1.1");
}

#[test]
fn tag_sends_the_name_before_one_working_copy_block() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "tag-default",
        &["tag", "T1"],
        b"M T hello.txt\nok\n",
        unchanged_file,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Argument T1",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "tag"
        ]
    );
    assert!(
        text(&out.stdout).contains("T hello.txt"),
        "{}",
        text(&out.stdout)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_b_sends_the_branch_flag_first() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("tag-b", &["tag", "-b", "B1"], b"ok\n", unchanged_file);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -b",
            "Argument --",
            "Argument B1",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "tag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_c_sends_the_check_flag() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("tag-c", &["tag", "-c", "T2"], b"ok\n", unchanged_file);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -c",
            "Argument --",
            "Argument T2",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "tag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_c_refuses_a_locally_modified_file() {
    // With `-c` the reference sends the file contents and lets the server refuse it
    // (tag.c:247); the two `E` lines below are the server's answer.
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "tag-c-modified",
        &["tag", "-c", "T1"],
        b"E cvs server: hello.txt is locally modified\n\
E cvs [server aborted]: correct the above errors first!\n\
error 1 correct the above errors first!\n",
        |dir| {
            std::fs::write(
                dir.join("CVS").join("Entries"),
                "/hello.txt/1.1/Sat Jan  1 00:00:00 2000//\nD\n",
            )
            .unwrap();
        },
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("cvs server: hello.txt is locally modified"),
        "{}",
        stderr
    );
    assert!(
        stderr.contains("cvs [server aborted]: correct the above errors first!"),
        "{}",
        stderr
    );
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert_eq!(
        lines[..4],
        ["Argument -c", "Argument --", "Argument T1", "Directory ."]
    );
    assert_eq!(lines[6], "Modified hello.txt", "{:?}", lines);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_sends_an_unknown_file_as_questionable_without_printing_it() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session("tag-unknown", &["tag", "T1"], b"ok\n", |dir| {
        unchanged_file(dir);
        std::fs::write(dir.join("extra.txt"), "extra\n").unwrap();
    });
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Argument T1",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "Questionable extra.txt",
            "tag"
        ]
    );
    assert!(
        !text(&out.stdout).contains('?'),
        "tag must not list unknown files: {}",
        text(&out.stdout)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_b_with_d_warns_and_still_sends_both_flags() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "tag-b-d",
        &["tag", "-b", "-d", "T1"],
        b"ok\n",
        unchanged_file,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("cvs tag: warning: -b ignored with -d options"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(lines[..2], ["Argument -b", "Argument -d"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_r_and_date_are_mutually_exclusive() {
    let (out, dir) = failing_command(
        "tag-r-and-D",
        &["tag", "-r", "1.1", "-D", "2020-01-01", "T1"],
        |_| {},
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("cvs [tag aborted]: -r and -D options are mutually exclusive"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_move_sends_the_move_flag() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("tag-F", &["tag", "-F", "T1"], b"ok\n", unchanged_file);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -F",
            "Argument --",
            "Argument T1",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "tag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_d_deletes_the_tag() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("tag-d", &["tag", "-d", "T1"], b"ok\n", unchanged_file);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -d",
            "Argument --",
            "Argument T1",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "tag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_r_sends_the_revision_as_two_arguments() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "tag-r",
        &["tag", "-r", "1.1", "T7"],
        b"ok\n",
        unchanged_file,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -r",
            "Argument 1.1",
            "Argument --",
            "Argument T7",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "tag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_date_sends_the_normalized_date_as_its_own_argument() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "tag-D",
        &["tag", "-D", "2020-01-01", "T5"],
        b"ok\n",
        unchanged_file,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        vec![
            "Argument -D".to_string(),
            format!("Argument {}", local_midnight(2020, 1, 1)),
            "Argument --".to_string(),
            "Argument T5".to_string(),
            "Directory .".to_string(),
            "/repo/mod".to_string(),
            "Entry /hello.txt/1.1///".to_string(),
            "Unchanged hello.txt".to_string(),
            "tag".to_string(),
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_l_is_sent_and_recursion_is_the_default() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("tag-l", &["tag", "-l", "T6"], b"ok\n", unchanged_file);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -l",
            "Argument --",
            "Argument T6",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "tag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();

    let (out, lines, dir, _) =
        working_session("tag-R", &["tag", "-R", "T7"], b"ok\n", unchanged_file);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Argument T7",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "tag"
        ],
        "-R is the default and must not be sent"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_names_a_nested_file_and_returns_to_the_top() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "tag-nested",
        &["tag", "T3", "sub/sub.txt"],
        b"ok\n",
        |dir| {
            versioned_subfolder(dir, "sub", "mod/sub", "");
            std::fs::write(dir.join("sub").join("sub.txt"), "sub\n").unwrap();
            record_current_time(&dir.join("sub"), "sub.txt", "1.1");
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Argument T3",
            "Directory sub",
            "/repo/mod/sub",
            "Entry /sub.txt/1.1///",
            "Unchanged sub.txt",
            "Directory .",
            "/repo/mod",
            "Argument sub/sub.txt",
            "tag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tag_without_a_name_prints_the_usage() {
    let dir = scratch("tag-no-name");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args(["-d", ":pserver:ann@127.0.0.1:1:/repo", "tag", "-b"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("Usage: cvs tag [-bcdFflR] [-r rev|-D date] tag [files...]"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_tag_server_error_fails_the_command() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, _, dir, _) = working_session(
        "tag-error",
        &["tag", "T1"],
        b"error 1 tag failed\n",
        unchanged_file,
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("tag failed"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rtag_sends_only_the_tag_and_the_module() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("rtag-default", &["rtag", "T3", "mod"], b"ok\n", |_| {});
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        ["Argument --", "Argument T3", "Argument mod", "rtag"]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rtag_ab_sends_both_flags_before_the_tag() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "rtag-ab",
        &["rtag", "-a", "-b", "B2", "mod"],
        b"ok\n",
        |_| {},
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -a",
            "Argument -b",
            "Argument --",
            "Argument B2",
            "Argument mod",
            "rtag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rtag_d_deletes_the_tag() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("rtag-d", &["rtag", "-d", "T9", "mod"], b"ok\n", |_| {});
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -d",
            "Argument --",
            "Argument T9",
            "Argument mod",
            "rtag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rtag_r_sends_the_revision_as_two_arguments() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "rtag-r",
        &["rtag", "-r", "1.1", "T8", "mod"],
        b"ok\n",
        |_| {},
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -r",
            "Argument 1.1",
            "Argument --",
            "Argument T8",
            "Argument mod",
            "rtag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rtag_date_sends_the_normalized_date_as_its_own_argument() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "rtag-D",
        &["rtag", "-D", "2020-01-01", "T9", "mod"],
        b"ok\n",
        |_| {},
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        vec![
            "Argument -D".to_string(),
            format!("Argument {}", local_midnight(2020, 1, 1)),
            "Argument --".to_string(),
            "Argument T9".to_string(),
            "Argument mod".to_string(),
            "rtag".to_string(),
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rtag_n_is_sent_to_the_server() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session("rtag-n", &["rtag", "-n", "T10", "mod"], b"ok\n", |_| {});
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -n",
            "Argument --",
            "Argument T10",
            "Argument mod",
            "rtag"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rtag_without_a_module_prints_the_usage() {
    let dir = scratch("rtag-no-module");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args(["-d", ":pserver:ann@127.0.0.1:1:/repo", "rtag", "T3"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr)
            .starts_with("Usage: cvs rtag [-abdFflnR] [-r rev|-D date] tag modules..."),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn help_names_tag_and_rtag_with_the_reference_text() {
    let commands = Command::new(cvs()).arg("--help-commands").output().unwrap();
    let commands = text(&commands.stdout);
    assert!(
        commands.contains("        rtag         Add a symbolic tag to a module"),
        "{}",
        commands
    );
    assert!(
        commands
            .contains("        tag          Add a symbolic tag to checked out version of files"),
        "{}",
        commands
    );

    let synonyms = Command::new(cvs()).arg("--help-synonyms").output().unwrap();
    let synonyms = text(&synonyms.stdout);
    assert!(
        synonyms.contains("        tag           ta freeze"),
        "{}",
        synonyms
    );
    assert!(
        synonyms.contains("        rtag          rt rfreeze"),
        "{}",
        synonyms
    );

    let tag_help = Command::new(cvs()).args(["-H", "tag"]).output().unwrap();
    let tag_help = text(&tag_help.stdout);
    assert_eq!(
        tag_help.trim_end_matches('\n'),
        "Usage: cvs tag [-bcdFflR] [-r rev|-D date] tag [files...]\n\
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
         (Specify the --help global option for a list of other help options)"
    );

    let rtag_help = Command::new(cvs()).args(["-H", "rtag"]).output().unwrap();
    let rtag_help = text(&rtag_help.stdout);
    assert_eq!(
        rtag_help.trim_end_matches('\n'),
        "Usage: cvs rtag [-abdFflnR] [-r rev|-D date] tag modules...\n\
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
         (Specify the --help global option for a list of other help options)"
    );
}
/// One protocol line from the client, or `None` when the pipe has closed.
fn next_line(reader: &mut BufReader<std::net::TcpStream>) -> Option<String> {
    let mut text = String::new();
    let read = reader.read_line(&mut text).ok()?;
    if read == 0 {
        return None;
    }
    Some(text.trim_end_matches(['\r', '\n']).to_string())
}

/// Run one watch-mode command against a fake pserver.
///
/// The fake server offers `Case` and `Utf8`, reads the opening requests, then collects every
/// line up to the request that ends the command: `noop` for `edit` and `unedit`, the
/// `watch-*` request for `watch`. `editors` is answered with `editors_reply`; the closing
/// request is answered with `reply`. `input` is written to the command's stdin.
fn watch_session(
    name: &str,
    args: &[&str],
    reply: &'static [u8],
    editors_reply: &'static [u8],
    input: Option<&[u8]>,
    prepare: impl FnOnce(&Path),
) -> (std::process::Output, Vec<String>, PathBuf) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let password = scrambled_secret();
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
        assert!(line(&mut reader).starts_with("Root /repo"));
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(b"Valid-requests Root Valid-responses Case Utf8\nok\n")
            .unwrap();
        assert_eq!(line(&mut reader), "UseUnchanged");
        assert_eq!(line(&mut reader), "Case");
        assert_eq!(line(&mut reader), "Utf8");

        let mut lines = Vec::new();
        while let Some(text) = next_line(&mut reader) {
            if text == "editors" {
                lines.push(text);
                writer.write_all(editors_reply).unwrap();
                writer.flush().unwrap();
                continue;
            }
            let done = matches!(
                text.as_str(),
                "noop" | "watch-on" | "watch-off" | "watch-add" | "watch-remove"
            );
            lines.push(text);
            if done {
                let _ = writer.write_all(reply);
                let _ = writer.flush();
                break;
            }
        }
        lines
    });

    let dir = working_copy(name, &root);
    prepare(&dir);

    let pass_dir = scratch(&format!("{}-pass", name));
    let passfile = pass_dir.join(".cvspass");
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let mut command = Command::new(cvs());
    command
        .env("CVS_PASSFILE", &passfile)
        .current_dir(&dir)
        .args(["-d", &root])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().unwrap();
    if let Some(bytes) = input {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let lines = server.join().unwrap();
    std::fs::remove_dir_all(&pass_dir).ok();
    (out, lines, dir)
}

fn make_readonly(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn is_readonly(path: &Path) -> bool {
    std::fs::metadata(path).unwrap().permissions().readonly()
}

/// A server answer to a notification: `Notified` names the folder and the file, then `ok`.
const NOTIFIED: &[u8] = b"Notified ./\n/repo/mod/hello.txt\nok\n";

#[test]
fn watch_on_sends_the_folder_block_and_the_request() {
    let (out, lines, dir) = watch_session(
        "watch-on",
        &["watch", "on", "hello.txt"],
        b"ok\n",
        b"ok\n",
        None,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "Argument hello.txt",
            "watch-on"
        ]
    );
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_off_sends_the_off_request() {
    let (out, lines, dir) = watch_session(
        "watch-off",
        &["watch", "off", "hello.txt"],
        b"ok\n",
        b"ok\n",
        None,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines.last().map(String::as_str), Some("watch-off"));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "Argument hello.txt",
            "watch-off"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_add_without_an_action_watches_all_three() {
    let (out, lines, dir) = watch_session(
        "watch-add",
        &["watch", "add", "hello.txt"],
        b"ok\n",
        b"ok\n",
        None,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -a",
            "Argument edit",
            "Argument -a",
            "Argument unedit",
            "Argument -a",
            "Argument commit",
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "Argument hello.txt",
            "watch-add"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_add_sends_one_action_per_flag() {
    let (out, lines, dir) = watch_session(
        "watch-add-commit",
        &["watch", "add", "-a", "commit", "hello.txt"],
        b"ok\n",
        b"ok\n",
        None,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -a",
            "Argument commit",
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "Argument hello.txt",
            "watch-add"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_add_takes_repeated_actions_but_not_a_comma_list() {
    let (out, lines, dir) = watch_session(
        "watch-add-two",
        &["watch", "add", "-a", "unedit", "-a", "edit", "hello.txt"],
        b"ok\n",
        b"ok\n",
        None,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines[..4],
        [
            "Argument -a",
            "Argument edit",
            "Argument -a",
            "Argument unedit"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();

    let dir = working_copy("watch-add-comma", ":pserver:ann@127.0.0.1:1:/repo");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args(["-d", ":pserver:ann@127.0.0.1:1:/repo"])
        .args(["watch", "add", "-a", "edit,unedit", "hello.txt"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("Usage: cvs watch "),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_remove_sends_the_remove_request() {
    let (out, lines, dir) = watch_session(
        "watch-remove",
        &["watch", "remove", "-a", "commit", "hello.txt"],
        b"ok\n",
        b"ok\n",
        None,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -a",
            "Argument commit",
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "Argument hello.txt",
            "watch-remove"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_on_with_no_files_names_the_folder_alone() {
    let (out, lines, dir) = watch_session(
        "watch-on-dir",
        &["watch", "on"],
        b"ok\n",
        b"ok\n",
        None,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "watch-on"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_l_is_sent_and_r_is_not() {
    let (out, lines, dir) = watch_session(
        "watch-l",
        &["watch", "on", "-l", "hello.txt"],
        b"ok\n",
        b"ok\n",
        None,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines[0], "Argument -l");
    assert_eq!(lines.last().map(String::as_str), Some("watch-on"));
    assert!(!lines.iter().any(|line| line == "Argument -R"));
    std::fs::remove_dir_all(&dir).ok();

    let (out, lines, dir) = watch_session(
        "watch-r",
        &["watch", "on", "-R", "hello.txt"],
        b"ok\n",
        b"ok\n",
        None,
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "Argument hello.txt",
            "watch-on"
        ],
        "-R is the default and must not be sent"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_without_a_subcommand_prints_the_usage() {
    let (out, dir) = failing_command("watch-usage", &["watch"], |_| {});
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with(
            "Usage: cvs watch {on|off|add|remove} [-lR] [-a <action>]... [<path>]...\n"
        ),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn watch_refuses_options_before_its_subcommand_and_actions_on_on() {
    for (name, args, complaint) in [
        (
            "watch-l-first",
            vec!["watch", "-l", "on", "hello.txt"],
            None,
        ),
        (
            "watch-on-action",
            vec!["watch", "on", "-a", "commit", "hello.txt"],
            Some("on: invalid option -- a"),
        ),
    ] {
        let (out, dir) = failing_command(name, &args, |_| {});
        assert_eq!(out.status.code(), Some(1));
        let stderr = text(&out.stderr);
        if let Some(complaint) = complaint {
            assert!(stderr.starts_with(complaint), "{}", stderr);
        }
        assert!(
            stderr.contains("Usage: cvs watch {on|off|add|remove}"),
            "{}",
            stderr
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn edit_sends_notify_e_and_the_closing_noop() {
    let (out, lines, dir) = watch_session(
        "edit-default",
        &["edit", "hello.txt"],
        NOTIFIED,
        b"ok\n",
        None,
        |dir| make_readonly(&dir.join("hello.txt")),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines[..3], ["Directory .", "/repo/mod", "Notify hello.txt"]);
    let notice: Vec<&str> = lines[3].split('\t').collect();
    assert_eq!(notice.len(), 5, "{}", lines[3]);
    assert_eq!(notice[0], "E");
    assert_eq!(notice[4], "EUC");
    assert_eq!(notice[3], dir.to_string_lossy());
    assert_eq!(lines[4], "noop");
    assert_eq!(lines.len(), 5);
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert!(
        !is_readonly(&dir.join("hello.txt")),
        "edit makes the file writable"
    );
    assert_eq!(
        std::fs::read(dir.join("CVS").join("Base").join("hello.txt")).unwrap(),
        b"hello\n",
        "edit keeps the file for unedit"
    );
    let baserev = std::fs::read_to_string(dir.join("CVS").join("Baserev")).unwrap();
    assert!(baserev.contains("Bhello.txt/1.1/"), "{}", baserev);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_l_is_client_side_only() {
    let (out, lines, dir) = watch_session(
        "edit-l",
        &["edit", "-l", "hello.txt"],
        NOTIFIED,
        b"ok\n",
        None,
        |_| {},
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines[..3], ["Directory .", "/repo/mod", "Notify hello.txt"]);
    assert!(
        !lines.iter().any(|line| line == "Argument -l"),
        "-l only limits the local walk"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_c_asks_editors_and_stops_when_someone_is_editing() {
    const EDITOR: &[u8] = b"M hello.txt\tann\tWed Nov 25 02:52:56 2026 GMT\thost\tC:/work\nok\n";
    let (out, lines, dir) = watch_session(
        "edit-c",
        &["edit", "-c", "hello.txt"],
        b"ok\n",
        EDITOR,
        None,
        |_| {},
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Is-modified hello.txt",
            "Directory .",
            "/repo/mod",
            "Argument hello.txt",
            "editors",
            "noop"
        ]
    );
    let stdout = text(&out.stdout);
    assert!(stdout.contains("hello.txt\tann"), "{}", stdout);
    assert!(
        text(&out.stderr).contains("cvs [edit aborted]: Files being edited!"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unedit_sends_notify_u_and_sets_the_file_read_only() {
    let (out, lines, dir) = watch_session(
        "unedit-default",
        &["unedit", "hello.txt"],
        NOTIFIED,
        b"ok\n",
        None,
        |dir| {
            record_current_time(dir, "hello.txt", "1.1");
            // `unedit` only reports a file that an earlier `edit` put in CVS/Base
            // (edit.c:481-491); without it the reference sends the bare `noop`.
            std::fs::create_dir_all(dir.join("CVS").join("Base")).unwrap();
            std::fs::write(dir.join("CVS").join("Base").join("hello.txt"), b"hello\n").unwrap();
            std::fs::write(dir.join("CVS").join("Baserev"), "Bhello.txt/1.1/\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        lines.len() >= 5,
        "lines: {:?}\nstderr: {}",
        lines,
        text(&out.stderr)
    );
    assert_eq!(lines[..3], ["Directory .", "/repo/mod", "Notify hello.txt"]);
    let notice: Vec<&str> = lines[3].split('\t').collect();
    assert_eq!(notice.len(), 5, "{}", lines[3]);
    assert_eq!(notice[0], "U");
    assert_eq!(notice[4], "", "unedit carries no watches");
    assert_eq!(lines[4], "noop");
    assert!(is_readonly(&dir.join("hello.txt")));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unedit_prompts_on_a_modified_file_and_a_declined_answer_leaves_it_alone() {
    let (out, lines, dir) = watch_session(
        "unedit-modified",
        &["unedit", "hello.txt"],
        NOTIFIED,
        b"ok\n",
        None,
        |dir| {
            let base = dir.join("CVS").join("Base");
            std::fs::create_dir_all(&base).unwrap();
            std::fs::write(base.join("hello.txt"), b"hello\n").unwrap();
            std::fs::write(dir.join("CVS").join("Baserev"), "Bhello.txt/1.1/\n").unwrap();
            std::fs::write(dir.join("hello.txt"), b"changed\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines, ["noop"], "a declined file is not reported");
    assert!(
        text(&out.stdout).contains("hello.txt has been modified; revert changes? "),
        "{}",
        text(&out.stdout)
    );
    assert!(
        !is_readonly(&dir.join("hello.txt")),
        "the declined file keeps its writable bit"
    );
    assert_eq!(
        std::fs::read(dir.join("hello.txt")).unwrap(),
        b"changed\n",
        "the declined file keeps its contents"
    );
    assert!(
        dir.join("CVS").join("Base").join("hello.txt").exists(),
        "a declined unedit keeps the base for a later try"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unedit_accepts_a_yes_and_marks_the_file_read_only() {
    let (out, lines, dir) = watch_session(
        "unedit-yes",
        &["unedit", "hello.txt"],
        NOTIFIED,
        b"ok\n",
        Some(b"y\n"),
        |dir| {
            let base = dir.join("CVS").join("Base");
            std::fs::create_dir_all(&base).unwrap();
            std::fs::write(base.join("hello.txt"), b"hello\n").unwrap();
            std::fs::write(dir.join("CVS").join("Baserev"), "Bhello.txt/1.1/\n").unwrap();
            std::fs::write(dir.join("hello.txt"), b"changed\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines[..3], ["Directory .", "/repo/mod", "Notify hello.txt"]);
    assert!(lines[3].starts_with("U\t"), "{}", lines[3]);
    assert_eq!(lines[4], "noop");
    assert_eq!(
        std::fs::read(dir.join("hello.txt")).unwrap(),
        b"hello\n",
        "unedit puts the base copy back"
    );
    assert!(
        !dir.join("CVS").join("Base").join("hello.txt").exists(),
        "the base is consumed"
    );
    assert!(is_readonly(&dir.join("hello.txt")));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn commit_forgets_the_base_for_the_files_it_accepts() {
    let (out, lines, dir, _) = working_session(
        "commit-base",
        &["commit", "-m", "change", "hello.txt"],
        b"Checked-in ./\n/repo/mod/hello.txt\n/hello.txt/1.2//\nok\n",
        |dir| {
            let base = dir.join("CVS").join("Base");
            std::fs::create_dir_all(&base).unwrap();
            std::fs::write(base.join("hello.txt"), b"hello\n").unwrap();
            std::fs::write(dir.join("CVS").join("Baserev"), "Bhello.txt/1.1/\n").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines.last().map(String::as_str), Some("ci"));
    assert!(!dir.join("CVS").join("Base").join("hello.txt").exists());
    let baserev = std::fs::read_to_string(dir.join("CVS").join("Baserev")).unwrap_or_default();
    assert!(!baserev.contains("Bhello.txt/"), "{}", baserev);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn help_names_edit_unedit_and_watch_with_the_reference_text() {
    let commands = Command::new(cvs()).arg("--help-commands").output().unwrap();
    let commands = text(&commands.stdout);
    for line in [
        "        edit         Get ready to edit a watched file",
        "        unedit       Undo an edit command",
        "        watch        Set watches",
    ] {
        assert!(
            commands.contains(line),
            "{} missing from:\n{}",
            line,
            commands
        );
    }

    let synonyms = Command::new(cvs()).arg("--help-synonyms").output().unwrap();
    let synonyms = text(&synonyms.stdout);
    for name in ["edit", "unedit", "watch"] {
        assert!(
            !synonyms.contains(&format!("\n        {:<14}", name)),
            "{} has no synonyms:\n{}",
            name,
            synonyms
        );
    }

    let edit_help = Command::new(cvs()).args(["-H", "edit"]).output().unwrap();
    assert_eq!(
        text(&edit_help.stdout).trim_end_matches('\n'),
        "Usage: cvs edit [-lR] [-a <action>]... [<file>]...\n\
         -l\tLocal directory only, not recursive.\n\
         -R\tProcess directories recursively (default).\n\
         -a\tSpecify action to register for temporary watch, one of:\n\
         \x20 \t`edit', `unedit', `commit', `all', or `none' (defaults to `all').\n\
         (Specify the --help global option for a list of other help options.)"
    );

    let unedit_help = Command::new(cvs()).args(["-H", "unedit"]).output().unwrap();
    assert_eq!(
        text(&unedit_help.stdout).trim_end_matches('\n'),
        "Usage: cvs unedit [-lR] [<file>]...\n\
         -l\tLocal directory only, not recursive.\n\
         -R\tProcess directories recursively (default).\n\
         (Specify the --help global option for a list of other help options.)"
    );

    let watch_help = Command::new(cvs()).args(["-H", "watch"]).output().unwrap();
    assert_eq!(
        text(&watch_help.stdout).trim_end_matches('\n'),
        "Usage: cvs watch {on|off|add|remove} [-lR] [-a <action>]... [<path>]...\n\
         on/off: Turn on/off read-only checkouts of files.\n\
         add/remove: Add or remove notification on actions.\n\
         -l (on/off/add/remove): Local directory only, not recursive.\n\
         -R (on/off/add/remove): Process directories recursively (default).\n\
         -a (add/remove): Specify what actions, one of: `edit', `unedit',\n\
         \x20                `commit', `all', or `none' (defaults to `all').\n\
         (Specify the --help global option for a list of other help options.)"
    );
}

/// A working copy with one subfolder that carries its own `CVS/Tag` and, optionally, an
/// `CVS/Entries.Static` marker. The top folder has no files, so only the subfolder's block
/// can add lines to the wire.
fn working_copy_with_markers(dir: &Path, tag: Option<&str>, static_dir: bool) {
    std::fs::remove_file(dir.join("hello.txt")).unwrap();
    std::fs::write(dir.join("CVS").join("Entries"), "D/sub////\nD\n").unwrap();
    versioned_subfolder(dir, "sub", "mod/sub", "D\n");
    if let Some(tag) = tag {
        std::fs::write(dir.join("sub").join("CVS").join("Tag"), tag).unwrap();
    }
    if static_dir {
        std::fs::write(dir.join("sub").join("CVS").join("Entries.Static"), "").unwrap();
    }
}

#[test]
fn update_tells_the_server_a_subfolders_sticky_tag_and_static_flag() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session_with(
        "update-sticky",
        &["update"],
        "Sticky Static-directory",
        b"ok\n",
        |dir| working_copy_with_markers(dir, Some("TBRANCH\r\n"), true),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Directory sub",
            "/repo/mod/sub",
            "Static-directory",
            "Sticky TBRANCH",
            "Directory .",
            "/repo/mod",
            "update"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn update_says_nothing_about_markers_the_server_cannot_read() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session_with(
        "update-markers-unsupported",
        &["update"],
        "Case Utf8",
        b"ok\n",
        |dir| working_copy_with_markers(dir, Some("TBRANCH\r\n"), true),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Directory sub",
            "/repo/mod/sub",
            "Directory .",
            "/repo/mod",
            "update"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_empty_tag_file_sends_no_sticky_request() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session_with(
        "update-empty-tag",
        &["update"],
        "Sticky Static-directory",
        b"ok\n",
        |dir| working_copy_with_markers(dir, Some("\r\n"), false),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Directory sub",
            "/repo/mod/sub",
            "Directory .",
            "/repo/mod",
            "update"
        ]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn init_sends_the_repository_path_and_nothing_else() {
    let (out, lines) = repo_query("init", &["init"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines, ["init /repo"]);
}

#[test]
fn init_refuses_arguments_before_any_connection() {
    let dir = scratch("init-args");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args(["-d", ":pserver:ann@127.0.0.1:1:/repo", "init", "extra"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("Usage: cvs init"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn history_sends_the_report_flag_and_the_timezone() {
    let (out, lines) = repo_query("history-c", &["history", "-c"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "UseUnchanged",
            "Argument -c",
            "Argument -z",
            "Argument +0000",
            "history"
        ]
    );
}

#[test]
fn history_sends_a_date_as_its_own_argument() {
    let (out, lines) = repo_query("history-d", &["history", "-a", "-D", "2020-01-01"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        vec![
            "UseUnchanged".to_string(),
            "Argument -a".to_string(),
            "Argument -o".to_string(),
            "Argument -D".to_string(),
            format!("Argument {}", local_midnight(2020, 1, 1)),
            "Argument -z".to_string(),
            "Argument +0000".to_string(),
            "history".to_string(),
        ]
    );
}

#[test]
fn history_sends_files_before_modules_and_users() {
    let (out, lines) = repo_query(
        "history-order",
        &[
            "history", "-n", "m1", "-f", "a.txt", "-p", "sub", "-u", "ann",
        ],
        b"ok\n",
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "UseUnchanged",
            "Argument -o",
            "Argument -f",
            "Argument a.txt",
            "Argument -p",
            "Argument sub",
            "Argument -n",
            "Argument m1",
            "Argument -u",
            "Argument ann",
            "Argument -z",
            "Argument +0000",
            "history"
        ]
    );
}

#[test]
fn history_refuses_two_report_types() {
    let dir = scratch("history-two-reports");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args([
            "-d",
            ":pserver:ann@127.0.0.1:1:/repo",
            "history",
            "-c",
            "-o",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("Only one report type allowed from: \"-Tcomxe\"."),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn history_drops_a_revision_when_a_later_date_overrides_it() {
    let (out, lines) = repo_query(
        "history-date-wins",
        &["history", "-r", "1.1", "-D", "2020-01-01"],
        b"ok\n",
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("date overriding rev/tag/backto"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        lines,
        vec![
            "UseUnchanged".to_string(),
            "Argument -o".to_string(),
            "Argument -D".to_string(),
            format!("Argument {}", local_midnight(2020, 1, 1)),
            "Argument -z".to_string(),
            "Argument +0000".to_string(),
            "history".to_string(),
        ]
    );
}

#[test]
fn history_without_a_report_asks_for_checked_out_modules() {
    let (out, lines) = repo_query("history-default", &["history"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "UseUnchanged",
            "Argument -o",
            "Argument -z",
            "Argument +0000",
            "history"
        ]
    );
}

#[test]
fn rdiff_sends_one_revision_as_two_arguments() {
    let (out, lines) = repo_query("rdiff-one", &["rdiff", "-r", "1.1", "mod"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "UseUnchanged",
            "Argument -r",
            "Argument 1.1",
            "Argument --",
            "Argument mod",
            "rdiff"
        ]
    );
}

#[test]
fn rdiff_sends_two_revisions_and_the_diff_format_before_them() {
    let (out, lines) = repo_query(
        "rdiff-two",
        &["rdiff", "-u", "-f", "-r", "1.1", "-r", "1.2", "mod"],
        b"ok\n",
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "UseUnchanged",
            "Argument -f",
            "Argument -u",
            "Argument -r",
            "Argument 1.1",
            "Argument -r",
            "Argument 1.2",
            "Argument --",
            "Argument mod",
            "rdiff"
        ]
    );
}

#[test]
fn rdiff_sends_a_date_as_its_own_argument() {
    let (out, lines) = repo_query("rdiff-date", &["rdiff", "-D", "2020-01-01", "mod"], b"ok\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        vec![
            "UseUnchanged".to_string(),
            "Argument -D".to_string(),
            format!("Argument {}", local_midnight(2020, 1, 1)),
            "Argument --".to_string(),
            "Argument mod".to_string(),
            "rdiff".to_string(),
        ]
    );
}

#[test]
fn rdiff_needs_a_revision_or_a_date() {
    let dir = scratch("rdiff-no-rev");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args(["-d", ":pserver:ann@127.0.0.1:1:/repo", "rdiff", "mod"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("must specify at least one revision/date!"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rdiff_refuses_a_top_two_report_beside_a_revision() {
    let dir = scratch("rdiff-t-rev");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args([
            "-d",
            ":pserver:ann@127.0.0.1:1:/repo",
            "rdiff",
            "-t",
            "-r",
            "1.1",
            "mod",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("must not specify revisions/dates with -t option!"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rdiff_without_a_module_prints_the_usage() {
    let dir = scratch("rdiff-no-module");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args(["-d", ":pserver:ann@127.0.0.1:1:/repo", "rdiff", "-r", "1.1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("Usage: cvs rdiff"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn admin_sends_the_message_attached_then_the_files() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "admin-m",
        &["admin", "-m", "1.1:message", "hello.txt"],
        b"M RCS file: /repo/mod/hello.txt,v\nok\n",
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines,
        [
            "Argument -m1.1:message",
            "Argument --",
            "Directory .",
            "/repo/mod",
            "Entry /hello.txt/1.1///",
            "Unchanged hello.txt",
            "Argument hello.txt",
            "admin"
        ]
    );
    assert!(
        text(&out.stdout).contains("RCS file: /repo/mod/hello.txt,v"),
        "{}",
        text(&out.stdout)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn admin_refuses_strict_and_nonstrict_locking_together() {
    let (out, dir) = failing_command("admin-lu", &["admin", "-L", "-U", "hello.txt"], |_| {});
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("-U and -L are incompatible"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn admin_sends_a_multiline_description_with_argumentx() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) = working_session(
        "admin-t",
        &["admin", "-t-first line\nsecond line", "hello.txt"],
        b"ok\n",
        |dir| record_current_time(dir, "hello.txt", "1.1"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines[0], "Argument -t-first line");
    assert_eq!(lines[1], "Argumentx second line");
    assert_eq!(lines[2], "Argument --");
    assert!(lines.contains(&"Argument hello.txt".to_string()));
    assert_eq!(lines.last().map(String::as_str), Some("admin"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn export_sends_the_export_request_and_writes_no_cvs_folder() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let password = scrambled_secret();
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
        assert!(line(&mut reader).starts_with("Root /repo"));
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(b"Valid-requests Root Valid-responses expand-modules\nok\n")
            .unwrap();
        assert_eq!(line(&mut reader), "UseUnchanged");

        assert_eq!(line(&mut reader), "Argument mod");
        assert_eq!(line(&mut reader), "Directory .");
        assert_eq!(line(&mut reader), "/repo");
        assert_eq!(line(&mut reader), "expand-modules");
        writer.write_all(b"Module-expansion mod\nok\n").unwrap();

        assert_eq!(line(&mut reader), "Argument -N");
        assert_eq!(line(&mut reader), "Argument -r");
        assert_eq!(line(&mut reader), "Argument T1");
        assert_eq!(line(&mut reader), "Argument --");
        assert_eq!(line(&mut reader), "Argument mod");
        assert_eq!(line(&mut reader), "Directory .");
        assert_eq!(line(&mut reader), "/repo");
        assert_eq!(line(&mut reader), "export");

        writer
            .write_all(
                b"E cvs server: Exporting mod\n\
                  M U mod/hello.txt\n\
                  Created mod/\n/repo/mod/hello.txt\n/hello.txt/1.1///\nu=rw,g=rw,o=rw\n3\none",
            )
            .unwrap();
        writer.write_all(b"ok\n").unwrap();
        writer.flush().unwrap();
    });

    let dir = scratch("export");
    let passfile = dir.join(".cvspass");
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let out = Command::new(cvs())
        .env("CVS_PASSFILE", &passfile)
        .current_dir(&dir)
        .args(["-d", &root, "export", "-r", "T1", "mod"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(
        std::fs::read(dir.join("mod").join("hello.txt")).unwrap(),
        b"one"
    );
    assert!(
        !dir.join("mod").join("CVS").exists(),
        "export must not create a CVS folder"
    );
    server.join().unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn export_refuses_to_run_inside_a_working_copy() {
    let dir = scratch("export-in-working-copy");
    // The reference refuses when the folder it is about to write already holds a `CVS`
    // entry (checkout.c:553-557); for `export mod` that is `./mod`, not the current folder.
    std::fs::create_dir_all(dir.join("mod").join("CVS")).unwrap();
    std::fs::write(
        dir.join("mod").join("CVS").join("Root"),
        ":pserver:ann@127.0.0.1:1:/repo\n",
    )
    .unwrap();
    std::fs::write(dir.join("mod").join("CVS").join("Repository"), "mod\n").unwrap();
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args([
            "-d",
            ":pserver:ann@127.0.0.1:1:/repo",
            "export",
            "-r",
            "T1",
            "mod",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("cannot export into a working directory"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn official_aliases_reach_their_commands() {
    for (alias, usage) in [
        ("ann", "Usage: cvs annotate"),
        ("blame", "Usage: cvs annotate"),
        ("get", "Usage:\n  cvs checkout"),
        ("com", "Usage: cvs commit"),
        ("lo", "Usage: cvs log"),
        ("logon", "Usage: cvs login"),
        ("lgn", "Usage: cvs login"),
        ("stat", "Usage: cvs status"),
        ("upd", "Usage: cvs update"),
        ("ve", "Usage: cvs version"),
        ("ver", "Usage: cvs version"),
    ] {
        let out = Command::new(cvs()).arg("-H").arg(alias).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", alias);
        assert!(
            text(&out.stdout).starts_with(usage),
            "{} showed:\n{}",
            alias,
            text(&out.stdout)
        );
    }

    let synonyms = Command::new(cvs()).arg("--help-synonyms").output().unwrap();
    let synonyms = text(&synonyms.stdout);
    for line in [
        "annotate      ann blame",
        "checkout      co get",
        "commit        ci com",
        "log           lo",
        "login         logon lgn",
        "status        st stat",
        "update        up upd",
        "version       ve ver",
    ] {
        assert!(
            synonyms.contains(line),
            "{} missing from:\n{}",
            line,
            synonyms
        );
    }
}

#[test]
fn an_unknown_command_is_named_as_such() {
    let out = Command::new(cvs()).arg("frobnicate").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("unknown command: `frobnicate'"),
        "{}",
        text(&out.stderr)
    );
}

/// A fake pserver for `release`. Its report is the reference's child `cvs -n -q update`
/// (release.c:135-146), so the first connection carries that request and a second one carries
/// `Argument`/`release` after the user answers.
fn release_session(
    name: &str,
    args: &[&str],
    scan_reply: &'static [u8],
    reply: &'static [u8],
    input: &[u8],
    prepare: impl FnOnce(&Path),
) -> (std::process::Output, Vec<String>, Vec<String>, PathBuf) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let server = std::thread::spawn(move || {
        let password = scrambled_secret();
        let mut scan_lines = Vec::new();
        let mut release_lines = Vec::new();

        // The dry-run update: `cvs -n -q update`, with the global options it implies.
        let Some(stream) = accept_within(&listener) else {
            return (scan_lines, release_lines);
        };
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
        writer.write_all(b"I LOVE YOU\n").unwrap();
        assert!(line(&mut reader).starts_with("Root /repo"));
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(b"Valid-requests Root Valid-responses Global_option Case Utf8\nok\n")
            .unwrap();
        assert_eq!(line(&mut reader), "Global_option -n");
        assert_eq!(line(&mut reader), "Global_option -q");
        assert_eq!(line(&mut reader), "UseUnchanged");
        assert_eq!(line(&mut reader), "Case");
        assert_eq!(line(&mut reader), "Utf8");
        loop {
            let text = line(&mut reader);
            let done = text == "update";
            scan_lines.push(text);
            if done {
                break;
            }
        }
        writer.write_all(scan_reply).unwrap();
        writer.flush().unwrap();
        drop(writer);

        // A "no" answer ends the command; the reference sends no release request then.
        let Some(stream) = accept_for(&listener, std::time::Duration::from_secs(2)) else {
            return (scan_lines, release_lines);
        };
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
        writer.write_all(b"I LOVE YOU\n").unwrap();
        assert!(line(&mut reader).starts_with("Root /repo"));
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(b"Valid-requests Root Valid-responses Case Utf8\nok\n")
            .unwrap();
        assert_eq!(line(&mut reader), "UseUnchanged");
        assert_eq!(line(&mut reader), "Case");
        assert_eq!(line(&mut reader), "Utf8");
        loop {
            let text = line(&mut reader);
            let done = text == "release";
            release_lines.push(text);
            if done {
                break;
            }
        }
        writer.write_all(reply).unwrap();
        writer.flush().unwrap();
        (scan_lines, release_lines)
    });

    let dir = working_copy(name, &root);
    prepare(&dir);

    let pass_dir = scratch(&format!("{}-pass", name));
    let passfile = pass_dir.join(".cvspass");
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let mut command = Command::new(cvs());
    command
        .env("CVS_PASSFILE", &passfile)
        .current_dir(&dir)
        .args(["-d", &root])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    let (scan_lines, release_lines) = server.join().unwrap();
    std::fs::remove_dir_all(&pass_dir).ok();
    (out, scan_lines, release_lines, dir)
}

#[test]
fn release_reports_altered_files_and_sends_the_release_request() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, scan, lines, dir) = release_session(
        "release-altered",
        &["release", "."],
        b"M M hello.txt\nok\n",
        b"ok\n",
        b"y\n",
        |dir| {
            // The dry-run update must see one local change to report `M hello.txt`.
            std::fs::write(dir.join("hello.txt"), "changed\n").unwrap();
        },
    );
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(scan.last().map(String::as_str), Some("update"));
    assert_eq!(
        scan[0], "Argument --",
        "the update request starts with the separator"
    );
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("M hello.txt"),
        "the update report should name the altered file:\n{}",
        stdout
    );
    assert!(
        stdout.contains("You have [1] altered files in this repository."),
        "{}",
        stdout
    );
    assert!(
        stdout.contains("Are you sure you want to release directory `.': "),
        "{}",
        stdout
    );
    assert_eq!(lines, ["Argument .", "release"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn release_declined_by_the_user_sends_no_release_request() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // The reference runs its dry-run update before it asks (release.c:193-235), so the scan
    // connection still happens; a "no" answer must stop before the release request.
    let (out, scan, release, dir) = release_session(
        "release-declined",
        &["release", "."],
        b"ok\n",
        b"ok\n",
        b"n\n",
        |_| {},
    );
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(scan.last().map(String::as_str), Some("update"));
    assert!(
        release.is_empty(),
        "a declined release must not send the request: {:?}",
        release
    );
    assert!(
        text(&out.stderr).contains("** `release' aborted by user choice."),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn release_d_deletes_the_folder_it_released() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, scan, lines, dir) = release_session(
        "release-delete",
        &["release", "-d", "sub"],
        b"M M one.txt\nok\n",
        b"ok\n",
        b"y\n",
        |dir| {
            versioned_subfolder(
                dir,
                "sub",
                "mod/sub",
                "/one.txt/1.1/Tue Nov 24 09:53:00 2026//\nD\n",
            );
            std::fs::write(dir.join("sub").join("one.txt"), "changed\n").unwrap();
        },
    );
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(scan.last().map(String::as_str), Some("update"));
    assert!(
        text(&out.stdout)
            .contains("Are you sure you want to release (and delete) directory `sub': "),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(lines, ["Argument sub", "release"]);
    assert!(
        !dir.join("sub").exists(),
        "the released folder should be gone"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn release_without_a_directory_prints_the_usage() {
    let dir = scratch("release-usage");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args(["-d", ":pserver:ann@127.0.0.1:1:/repo", "release"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("Usage: cvs release [-d] directories..."),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

struct ImportWire {
    lines: Vec<String>,
    files: Vec<(String, Vec<u8>)>,
}

/// Run one `import` against a fake pserver that reads the whole upload, mode lines and file
/// contents included, and answers `ok`.
fn run_import(
    name: &str,
    args: &[&str],
    capabilities: &'static str,
    reply: &'static [u8],
    prepare: impl FnOnce(&Path),
) -> (std::process::Output, ImportWire, PathBuf) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let server = std::thread::spawn(move || {
        let mut wire = ImportWire {
            lines: Vec::new(),
            files: Vec::new(),
        };
        let Some(stream) = accept_within(&listener) else {
            return wire;
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let password = scrambled_secret();
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
        assert!(line(&mut reader).starts_with("Root /repo"));
        let valid = line(&mut reader);
        assert!(valid.starts_with("Valid-responses "));
        assert!(
            valid.split_whitespace().any(|name| name == "MT"),
            "the client must advertise MT for the server to send a merge hint: {}",
            valid
        );
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(
                format!("Valid-requests Root Valid-responses {}\nok\n", capabilities).as_bytes(),
            )
            .unwrap();
        assert_eq!(line(&mut reader), "UseUnchanged");
        if capabilities.contains("Case") {
            assert_eq!(line(&mut reader), "Case");
        }
        if capabilities.contains("Utf8") {
            assert_eq!(line(&mut reader), "Utf8");
        }

        loop {
            let text = line(&mut reader);
            if text == "import" {
                wire.lines.push(text);
                break;
            }
            if let Some(file) = text.strip_prefix("Modified ") {
                let _mode = line(&mut reader);
                let length: usize = line(&mut reader).parse().unwrap();
                let mut content = vec![0u8; length];
                reader.read_exact(&mut content).unwrap();
                wire.lines.push(format!("Modified {}", file));
                wire.files.push((file.to_string(), content));
            } else {
                wire.lines.push(text);
            }
        }
        writer.write_all(reply).unwrap();
        writer.flush().unwrap();
        wire
    });

    let dir = scratch(name);
    prepare(&dir);
    let pass_dir = scratch(&format!("{}-pass", name));
    let home = scratch(&format!("{}-home", name));
    let passfile = pass_dir.join(".cvspass");
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let out = Command::new(cvs())
        .env("CVS_PASSFILE", &passfile)
        // The ignore list reads the home folder and CVSIGNORE; a test must not inherit the
        // machine's own `.cvsignore`.
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("CVSIGNORE")
        .current_dir(&dir)
        .args(["-d", &root])
        .args(args)
        .output()
        .unwrap();
    let wire = server.join().unwrap();
    std::fs::remove_dir_all(&pass_dir).ok();
    std::fs::remove_dir_all(&home).ok();
    (out, wire, dir)
}

#[test]
fn import_uploads_each_file_under_its_folder() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, wire, dir) = run_import(
        "import-basic",
        &[
            "import",
            "-b",
            "1.2.2",
            "-m",
            "first import",
            "mod",
            "vendor",
            "start",
        ],
        "",
        b"ok\n",
        |dir| {
            std::fs::write(dir.join("hello.txt"), b"one\r\n").unwrap();
            std::fs::create_dir_all(dir.join("sub")).unwrap();
            std::fs::write(dir.join("sub").join("two.txt"), b"two\n").unwrap();
        },
    );
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(
        wire.lines,
        [
            "Argument -b",
            "Argument 1.2.2",
            "Argument -m",
            "Argument first import",
            "Argumentx ",
            "Argument mod",
            "Argument vendor",
            "Argument start",
            "Directory .",
            "/repo/mod",
            "Modified hello.txt",
            "Directory sub",
            "/repo/mod/sub",
            "Modified two.txt",
            "Directory .",
            "/repo/mod",
            "import"
        ]
    );
    assert_eq!(
        wire.files,
        vec![
            ("hello.txt".to_string(), b"one\n".to_vec()),
            ("two.txt".to_string(), b"two\n".to_vec()),
        ]
    );
    assert!(
        !dir.join("CVS").exists(),
        "import must not create a working copy"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn import_skips_ignored_files_and_folders() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, wire, dir) = run_import(
        "import-ignored",
        &["import", "-m", "import", "mod", "vendor", "start"],
        "",
        b"ok\n",
        |dir| {
            std::fs::write(dir.join(".cvsignore"), "secret.txt\ntemp\n").unwrap();
            std::fs::write(dir.join("hello.txt"), b"one\n").unwrap();
            std::fs::write(dir.join("secret.txt"), b"hidden\n").unwrap();
            std::fs::write(dir.join("build.o"), b"object\n").unwrap();
            std::fs::create_dir_all(dir.join("temp")).unwrap();
            std::fs::write(dir.join("temp").join("x.txt"), b"x\n").unwrap();
        },
    );
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    for skipped in ["secret.txt", "build.o", "temp"] {
        assert!(
            !wire.lines.iter().any(|line| line.contains(skipped))
                && !wire.files.iter().any(|(name, _)| name == skipped),
            "{} should have been ignored:\n{:?}",
            skipped,
            wire.lines
        );
    }
    assert!(wire.files.iter().any(|(name, _)| name == "hello.txt"));
    assert!(
        wire.files.iter().any(|(name, _)| name == ".cvsignore"),
        "the .cvsignore file itself is imported"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn import_k_b_sends_the_keyword_flag_and_keeps_binary_bytes() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, wire, dir) = run_import(
        "import-kb",
        &[
            "import", "-k", "b", "-m", "import", "mod", "vendor", "start",
        ],
        "Kopt",
        b"ok\n",
        |dir| {
            std::fs::write(dir.join("data.bin"), b"a\r\nb").unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let keyword = wire
        .lines
        .iter()
        .position(|line| line == "Argument -k")
        .expect("the keyword option is named");
    assert_eq!(wire.lines[keyword + 1], "Argument b");
    let kopt = wire
        .lines
        .iter()
        .position(|line| line == "Kopt -kb")
        .expect("each file names its keyword mode");
    assert_eq!(wire.lines[kopt + 1], "Modified data.bin");
    assert_eq!(wire.files[0].1, b"a\r\nb");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn import_without_a_vendor_or_release_tag_prints_the_usage() {
    let dir = scratch("import-usage");
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args([
            "-d",
            ":pserver:ann@127.0.0.1:1:/repo",
            "import",
            "-m",
            "x",
            "mod",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr)
            .contains("Usage: cvs import [-d] [-k subst] [-I ign] [-m msg] [-b branch]"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn import_without_a_message_runs_the_editor_and_aborts_on_eof() {
    // The reference opens the server before it runs the editor (import.c:243-251). An
    // unchanged message then asks the a/c/e/! question; EOF there is fatal (logmsg.c:343-351).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let Some(stream) = accept_within(&listener) else {
            return;
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let password = scrambled_secret();
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
        assert!(line(&mut reader).starts_with("Root /repo"));
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(b"Valid-requests Root Valid-responses\nok\n")
            .unwrap();
        writer.flush().unwrap();
    });

    let dir = scratch("import-no-message");
    let pass_dir = scratch("import-no-message-pass");
    let passfile = pass_dir.join(".cvspass");
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();
    // An editor that exits without touching the message file.
    let editor = if cfg!(windows) {
        "cmd /c exit 0"
    } else {
        "true"
    };
    let out = Command::new(cvs())
        .env("CVS_PASSFILE", &passfile)
        .env("CVSEDITOR", editor)
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args(["-d", &root, "import", "mod", "vendor", "start"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("cvs [import aborted]: aborting"),
        "{}",
        stderr
    );
    assert!(
        text(&out.stdout).contains("Log message unchanged or not specified"),
        "{}",
        text(&out.stdout)
    );
    server.join().unwrap();
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&pass_dir).ok();
}

#[test]
fn import_d_records_each_files_own_time() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, wire, dir) = run_import(
        "import-dated",
        &["import", "-d", "-m", "import", "mod", "vendor", "start"],
        "Checkin-time",
        b"ok\n",
        |dir| {
            let file = dir.join("hello.txt");
            std::fs::write(&file, b"one\n").unwrap();
            let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
            codec::set_file_mtime(&file, time).unwrap();
        },
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let at = wire
        .lines
        .iter()
        .position(|line| line.starts_with("Checkin-time "))
        .expect("the file time is named");
    assert_eq!(wire.lines[at], "Checkin-time 1 Jan 2020 00:00:00 -0000");
    assert_eq!(wire.lines[at + 1], "Modified hello.txt");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn z_compresses_the_stream_after_the_handshake() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let Some(stream) = accept_within(&listener) else {
            return Vec::new();
        };
        // Read one byte at a time: the compressed stream starts right after the handshake,
        // and a buffered reader would swallow its first bytes.
        let mut reader = BufReader::with_capacity(1, stream.try_clone().unwrap());
        let mut writer = stream;
        let password = scrambled_secret();
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
        assert!(line(&mut reader).starts_with("Root /repo"));
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(b"Valid-requests Root Valid-responses Gzip-stream\nok\n")
            .unwrap();
        // CVS starts compression after the plain behaviour requests, the way the
        // reference client does, and the stream is zlib, despite the request's name.
        assert_eq!(line(&mut reader), "UseUnchanged");
        assert_eq!(line(&mut reader), "Gzip-stream 9");

        let inner = reader.into_inner();
        let mut reader = BufReader::new(ZlibDecoder::new(inner));
        let mut writer = ZlibEncoder::new(writer, Compression::new(9));
        let mut lines = Vec::new();
        loop {
            let text = line(&mut reader);
            lines.push(text.clone());
            if text == "noop" {
                break;
            }
        }
        writer.write_all(b"ok\n").unwrap();
        writer.flush().unwrap();
        lines
    });

    let dir = scratch("z-compressed");
    let passfile = dir.join(".cvspass");
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let out = Command::new(cvs())
        .env("CVS_PASSFILE", &passfile)
        .current_dir(&dir)
        .args(["-z9", "-d", &root, "noop"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(server.join().unwrap(), ["noop"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn z_with_a_server_that_cannot_compress_warns_and_stays_plain() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, lines, dir, _) =
        working_session_with("z-plain", &["-z9", "noop"], "Case Utf8", b"ok\n", |_| {});
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert!(
        text(&out.stderr).contains("server doesn't support gzip-file-contents"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(lines, ["noop"], "the stream must stay uncompressed");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn diff_ends_quietly_at_the_servers_empty_error() {
    let (out, lines, dir, _) = working_session(
        "diff-empty-error",
        &["diff", "hello.txt"],
        b"M Index: hello.txt\nM ===\nerror  \n",
        |_| {},
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "Index: hello.txt\n===\n");
    // The reference prints the empty message as a blank stderr line and ends the command;
    // this server uses that instead of `ok` when the files differ.
    assert_eq!(text(&out.stderr), "\n");
    assert_eq!(lines.last().map(String::as_str), Some("diff"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn z_outside_the_range_is_refused_before_any_connection() {
    for level in ["10", "-1", "fast"] {
        let out = Command::new(cvs())
            .args(["-z", level, "noop"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", level);
        assert!(
            text(&out.stderr).contains("gzip compression level must be between 0 and 9"),
            "{}: {}",
            level,
            text(&out.stderr)
        );
    }
}

#[test]
fn import_refuses_a_module_outside_the_repository() {
    let dir = scratch("import-outside");
    for module in ["../evil", "/absolute"] {
        let out = Command::new(cvs())
            .current_dir(&dir)
            .env_remove("CVSROOT")
            .args([
                "-d",
                ":pserver:ann@127.0.0.1:1:/repo",
                "import",
                "-m",
                "x",
                module,
                "vendor",
                "start",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", module);
        assert!(
            text(&out.stderr).contains("not relative within the repository"),
            "{}: {}",
            module,
            text(&out.stderr)
        );
    }

    // `a/../b` has no leading backreference (`pathname_levels` is 0), so the reference accepts
    // it (subr.c:187-217, import.c:193) and only the transport can fail.
    let out = Command::new(cvs())
        .current_dir(&dir)
        .env_remove("CVSROOT")
        .args([
            "-d",
            ":pserver:ann@127.0.0.1:1:/repo",
            "import",
            "-m",
            "x",
            "a/../b",
            "vendor",
            "start",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        !stderr.contains("not relative within the repository"),
        "{}",
        stderr
    );
    assert!(
        stderr.contains("connect to 127.0.0.1:1 failed"),
        "{}",
        stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// An editor that leaves `edited message` in the message file, and a copy of what it was
/// given, so a test can see the template the client put there.
fn message_editor(dir: &Path) -> (String, PathBuf) {
    let seen = dir.join("seen.txt");
    #[cfg(windows)]
    let editor = {
        let script = dir.join("editor.cmd");
        std::fs::write(
            &script,
            "@echo off\r\ncopy /y \"%~1\" \"%~dp0seen.txt\" >nul\r\n> \"%~1\" echo edited message\r\n",
        )
        .unwrap();
        format!("cmd /c {}", script.display())
    };
    #[cfg(unix)]
    let editor = {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("editor.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncp \"$1\" \"$(dirname \"$0\")/seen.txt\"\nprintf 'edited message\\n' > \"$1\"\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        format!("sh {}", script.display())
    };
    (editor, seen)
}

#[test]
fn commit_without_m_starts_the_editor_and_sends_what_it_wrote() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = format!(":pserver:ann@127.0.0.1:{}:/repo", port);
    let server = std::thread::spawn(move || {
        let Some(stream) = accept_within(&listener) else {
            return Vec::new();
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let password = scrambled_secret();
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
        assert!(line(&mut reader).starts_with("Root /repo"));
        assert!(line(&mut reader).starts_with("Valid-responses "));
        assert_eq!(line(&mut reader), "valid-requests");
        writer
            .write_all(b"Valid-requests Root Valid-responses\nok\n")
            .unwrap();
        assert_eq!(line(&mut reader), "UseUnchanged");
        let mut lines = Vec::new();
        loop {
            let text = line(&mut reader);
            let done = text == "ci";
            lines.push(text);
            if done {
                break;
            }
        }
        writer.write_all(b"ok\n").unwrap();
        writer.flush().unwrap();
        lines
    });

    let dir = working_copy("commit-editor", &root);
    std::fs::write(dir.join("CVS").join("Template"), "bug id:\n").unwrap();
    let (editor, seen) = message_editor(&dir);
    let pass_dir = scratch("commit-editor-pass");
    let passfile = pass_dir.join(".cvspass");
    let mut pass = root.as_bytes().to_vec();
    pass.push(b' ');
    pass.extend_from_slice(&scramble(b"secret"));
    pass.push(b'\n');
    std::fs::write(&passfile, pass).unwrap();

    let out = Command::new(cvs())
        .env("CVS_PASSFILE", &passfile)
        .env("EDITOR", &editor)
        .env_remove("VISUAL")
        .env_remove("CVSEDITOR")
        .current_dir(&dir)
        .args(["-d", &root, "commit"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    let seen = std::fs::read_to_string(&seen)
        .unwrap()
        .replace("\r\n", "\n");
    assert!(
        seen.starts_with("bug id:\n"),
        "the template should open the editor text:\n{}",
        seen
    );
    assert!(
        seen.contains("CVS: Enter Log."),
        "the instructions should follow the template:\n{}",
        seen
    );
    let lines = server.join().unwrap();
    let message = lines
        .iter()
        .position(|line| line == "Argument -m")
        .expect("the message option is sent");
    assert_eq!(lines[message + 1], "Argument edited message");
    assert_eq!(lines.last().map(String::as_str), Some("ci"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&pass_dir).ok();
}

#[test]
fn import_prints_the_merge_command_a_conflicting_import_suggests() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (out, _, dir) = run_import(
        "import-merge",
        &["import", "-m", "import", "mod", "vendor", "start"],
        "",
        b"M N mod/hello.txt\n\
          MT +importmergecmd\n\
          MT conflicts 2\n\
          MT mergetag1 1.1\n\
          MT mergetag2 1.1.1.1\n\
          MT repository /repo/mod\n\
          MT -importmergecmd\n\
          ok\n",
        |dir| {
            std::fs::write(dir.join("hello.txt"), b"one\n").unwrap();
        },
    );
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("2 conflicts created by this import."),
        "{}",
        stdout
    );
    assert!(
        stdout.contains("Use the following command to help the merge:"),
        "{}",
        stdout
    );
    assert!(
        stdout.contains("checkout -j1.1 -j1.1.1.1 /repo/mod"),
        "{}",
        stdout
    );
    std::fs::remove_dir_all(&dir).ok();
}
