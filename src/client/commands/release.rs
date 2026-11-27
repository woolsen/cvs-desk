//! `cvs release`: give up a working copy, and say so in the repository's history.
//!
//! The reference asks a child `cvs update -n` what is still altered, prints that report, and
//! only then asks whether to go on (`release.c:124-244`). This client runs the same dry-run
//! update through its own `update` command, on its own connection. The report is therefore
//! the reference's: the server compares the contents of a file whose timestamp moved against
//! the repository revision (`No_Difference`), so a file that was only touched is not
//! "altered"; `U`/`P` name files the repository has moved past; `?` honours `.cvsignore`;
//! and an update that fails (a conflict, for one) makes the client refuse to release the
//! folder (`release.c:218-229`).
//!
//! `-Q` (not `-q`) suppresses the report and the question and releases straight away
//! (`release.c:193`). The global `-n` may not contact the server at all here: the report is
//! made from the working copy and the `release` request is not sent, so nothing is deleted
//! either (the reference would still talk to the server, but delete nothing).
//!
//! One reference step is skipped: it runs `unedit` first so that watches are cleared. This
//! client does not record which files were marked with `cvs edit`, and running `unedit` over
//! every file could put edits back and write them all read-only. The release itself is
//! unaffected.

use std::path::Path;

use crate::client::cli;
use crate::client::commands::targets::{self, Target};
use crate::client::commands::update;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::ignore::Ignore;
use crate::client::protocol;
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::dir;
use crate::client::work::entries::Entry;

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    /// `-d`: delete the folder once the server has accepted the release.
    delete: bool,
    names: Vec<String>,
}

/// What stopped the option scan, when something did.
#[derive(Debug, PartialEq, Eq)]
enum ParseError {
    /// An option this command does not know: the reference prints the usage and exits 1.
    Usage,
    /// `-q`/`-Q` was written after the command name: the reference reports it and exits 1.
    QuietAfterCommand,
}

/// Read the command's own options. Unlike the reference's `getopt "+Qdq"`, which stops at the
/// first folder, this client takes options wherever they are written: `release sub -d` and
/// `release -d sub` mean the same thing. Clusters such as `-dd` are accepted too.
fn parse_args(args: &[String]) -> std::result::Result<Options, ParseError> {
    let mut opts = Options::default();
    let mut names_only = false;
    for arg in args {
        if names_only {
            opts.names.push(arg.clone());
            continue;
        }
        if arg == "--" {
            names_only = true;
            continue;
        }
        if arg == "-" || !arg.starts_with('-') {
            opts.names.push(arg.clone());
            continue;
        }
        for letter in arg[1..].chars() {
            match letter {
                'd' => opts.delete = true,
                'q' | 'Q' => return Err(ParseError::QuietAfterCommand),
                _ => return Err(ParseError::Usage),
            }
        }
    }
    Ok(opts)
}

/// Run `cvs release`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = match parse_args(args) {
        Ok(opts) => opts,
        Err(ParseError::QuietAfterCommand) => {
            return Err(Error::plain(
                "release",
                "-q or -Q must be specified before \"release\"",
            ))
        }
        Err(ParseError::Usage) => {
            usage(sink);
            return Ok(1);
        }
    };
    if opts.names.is_empty() {
        usage(sink);
        return Ok(1);
    }

    // `-Q` alone is `really_quiet`; `-q` still prints the report and asks (`release.c:193`).
    let really_quiet = root.options.quiet >= 2;
    // The global `-n` may not change a disk, and this client also refuses to talk to the
    // server for a dry run: a release is not something to rehearse against the repository.
    let dry_run = root.options.no_execute;
    let mut code = 0;

    for name in &opts.names {
        let folder = cwd.join(name);
        if !folder.is_dir() {
            if !really_quiet {
                sink.err(format!("cvs release: no such directory: {}", name));
            }
            continue;
        }
        if !folder.join("CVS").is_dir() {
            if !really_quiet {
                sink.err(format!("cvs release: no repository directory: {}", name));
            }
            continue;
        }

        if !really_quiet {
            // A conflict already recorded in `CVS/Entries` makes the reference's dry-run
            // update fail before it prints the count, so no folder with one is released
            // (`release.c:218-229`).
            let conflicts = local_conflicts(&folder)?;
            if !conflicts.is_empty() {
                for path in conflicts {
                    sink.out(format!("C {}", path));
                }
                sink.err(unable_to_release(name));
                continue;
            }

            let altered = if dry_run {
                // No request may be made, so the report is the local one. It still prefers
                // the contents of `CVS/Base` over the recorded time, and it filters `?`
                // through the same ignore rules; only `U` cannot be known offline.
                report_local(&folder, sink)?
            } else {
                match scan_folder(&folder, name, &root, cancel, sink)? {
                    Scan::Ready(altered) => altered,
                    Scan::Refused => continue,
                }
            };
            sink.out(format!(
                "You have [{}] altered files in this repository.",
                altered
            ));
            if !sink.confirm(format!(
                "Are you sure you want to release {}directory `{}': ",
                if opts.delete { "(and delete) " } else { "" },
                name
            )) {
                sink.err("** `release' aborted by user choice.".to_string());
                continue;
            }
        }

        // A dry run stops here: no `release` request, no `remove_dir_all`.
        if dry_run {
            continue;
        }

        let mut session = protocol::open(root.clone(), "release", cancel, sink)?;
        let answer = request_release(&mut session, name);
        session.close();
        if let Err(error) = answer {
            if error.is_interrupted() {
                return Err(error);
            }
            // The reference adds the failed response to its exit code and goes on with the
            // next folder. Nothing is deleted for this one: the release did not succeed.
            sink.err(error.text().to_string());
            code = 1;
            continue;
        }

        if opts.delete {
            // The reference deletes before it reads the answer; deleting after `ok` is
            // safer and is the only place here that touches the disk (`release.c:288-296`).
            if let Err(e) = std::fs::remove_dir_all(&folder) {
                sink.err(format!(
                    "cvs release: deletion of directory {} failed: {}",
                    name, e
                ));
            }
        }
    }
    Ok(code)
}

fn usage(sink: &mut dyn Sink) {
    if let Some(text) = cli::command_help("release") {
        for line in text.lines() {
            sink.err(line.to_string());
        }
    }
}

fn unable_to_release(name: &str) -> String {
    // The reference prints the child's wait status; this client has a plain exit code.
    format!("cvs release: unable to release `{}' (1)", name)
}

/// What the dry-run update said about one folder.
enum Scan {
    /// No conflict; this many report lines count as altered.
    Ready(usize),
    /// The update failed or reported a conflict: the folder must not be released.
    Refused,
}

/// Run the report as `cvs -n update` does, over the same connection machinery the `update`
/// command uses, and count the lines the reference counts (`MARCZ`).
fn scan_folder(
    folder: &Path,
    name: &str,
    root: &Root,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<Scan> {
    // The reference's child is `cvs -n -q update` (`release.c:135-146`): the scan carries
    // `-n` and `-q`, while `-Q` never gets here (it skips the report entirely).
    let mut scan_root = root.clone();
    scan_root.options.quiet = scan_root.options.quiet.max(1);
    let (result, altered, conflict) = {
        let mut counter = CountingSink::new(sink);
        let result = update::run(
            folder,
            scan_root,
            &["-n".to_string()],
            root.options.readonly,
            cancel,
            &mut counter,
        );
        (result, counter.altered, counter.conflict)
    };
    match result {
        Ok(_) if !conflict => Ok(Scan::Ready(altered)),
        Ok(_) => {
            sink.err(unable_to_release(name));
            Ok(Scan::Refused)
        }
        Err(error) if error.is_interrupted() => Err(error),
        Err(error) => {
            // The child `cvs update` in the reference prints its own complaint and release
            // only says it cannot proceed (`release.c:218-229`).
            sink.err(error.text().to_string());
            sink.err(unable_to_release(name));
            Ok(Scan::Refused)
        }
    }
}

/// Send one `Argument`/`release` and read until `ok`. Unlike `targets::read_text`, an empty
/// `error` is still a failure: the reference treats any `error` response as non-zero
/// (`client.c:3493-3494`).
fn request_release(session: &mut protocol::Session, name: &str) -> Result<()> {
    session.argument(name)?;
    session.send("release")?;
    session.flush()?;
    loop {
        match session.read_response()? {
            protocol::Response::Ok => return Ok(()),
            protocol::Response::Error { message, .. } => {
                return Err(Error::aborted(
                    "release",
                    if message.trim().is_empty() {
                        "the server reported an error".to_string()
                    } else {
                        message
                    },
                ))
            }
            protocol::Response::E(text) => session.err(text),
            protocol::Response::M(text) => session.out(text),
            _ => {}
        }
    }
}

/// The conflict files `CVS/Entries` records. A conflict date is `Result of merge` followed by
/// `+` and the file's time; a clean merge has no `+` (`entries.c:376-377`, `update.c:1930-1941`).
fn local_conflicts(folder: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for target in targets::collect(folder, true, &[])?.values() {
        for entry in target.entries.files() {
            if is_conflicted(entry) {
                found.push(dir::display_path(&target.dir.update_dir, &entry.name));
            }
        }
    }
    Ok(found)
}

fn is_conflicted(entry: &Entry) -> bool {
    entry.date.trim().starts_with("Result of merge+")
}

/// Count the report lines the reference counts, `MARCZ`, while passing every line through.
/// A `C` line, or the update client's "move away" collision, also marks the folder as
/// conflicted so `release` can refuse it.
struct CountingSink<'a> {
    inner: &'a mut dyn Sink,
    altered: usize,
    conflict: bool,
}

impl<'a> CountingSink<'a> {
    fn new(inner: &'a mut dyn Sink) -> Self {
        Self {
            inner,
            altered: 0,
            conflict: false,
        }
    }
}

impl Sink for CountingSink<'_> {
    fn out(&mut self, line: String) {
        if line
            .chars()
            .next()
            .is_some_and(|letter| "MARCZ".contains(letter))
        {
            self.altered += 1;
        }
        if line.starts_with("C ") || line == "C" {
            self.conflict = true;
        }
        self.inner.out(line);
    }

    fn err(&mut self, line: String) {
        if line.contains("move away ") && line.contains("it is in the way") {
            self.conflict = true;
        }
        self.inner.err(line);
    }

    fn prompt(&mut self, text: String) {
        self.inner.prompt(text);
    }

    fn ask(&mut self, text: String) -> String {
        self.inner.ask(text)
    }

    fn confirm(&mut self, text: String) -> bool {
        self.inner.confirm(text)
    }

    fn secret(&mut self, text: String) -> Option<String> {
        self.inner.secret(text)
    }
}

/// The report a dry run can make without a server, in `update`'s order: known files first,
/// then the files CVS does not know about, minus the ignored ones.
fn report_local(folder: &Path, sink: &mut dyn Sink) -> Result<usize> {
    let targets = targets::collect(folder, true, &[])?;
    let ignore = Ignore::new(&[]);
    let mut altered = 0;
    for target in targets.values() {
        for entry in target.entries.files() {
            let Some(letter) = local_letter(target, entry) else {
                continue;
            };
            sink.out(format!(
                "{} {}",
                letter,
                dir::display_path(&target.dir.update_dir, &entry.name)
            ));
            if "MARCZ".contains(letter) {
                altered += 1;
            }
        }
        let folder_ignore = ignore.enter(&target.dir.local);
        for name in targets::unknown_files(&target.dir.local, &target.entries) {
            if folder_ignore.is_ignored(&name) {
                continue;
            }
            sink.out(format!(
                "? {}",
                dir::display_path(&target.dir.update_dir, &name)
            ));
        }
    }
    Ok(altered)
}

/// The letter `update -n` would print for one entry, as far as the working copy alone shows.
///
/// `U` needs the repository's revision, so a dry run cannot print it. `M` prefers a content
/// comparison when `CVS/Base` holds a copy; only a file without a base falls back to the
/// recorded time, because no request may settle the question.
fn local_letter(target: &Target, entry: &Entry) -> Option<char> {
    // CVS writes `-` followed by the revision for a removed-but-uncommitted file; older
    // entries can be just `-1` (`classify.c:208`, `remove.c:258-261`).
    if entry.revision.starts_with('-') {
        return Some('R');
    }
    if is_conflicted(entry) {
        return Some('C');
    }
    // A clean merge is still a local change waiting for a commit (`update.c:696-709`).
    if entry.date.trim().starts_with("Result of merge") {
        return Some('M');
    }
    if entry.is_added() {
        return Some('A');
    }
    if entry.is_unfinished() {
        return Some('M');
    }
    if locally_changed(target, entry) {
        Some('M')
    } else {
        None
    }
}

fn locally_changed(target: &Target, entry: &Entry) -> bool {
    let local = target.dir.local.join(&entry.name);
    if !local.is_file() {
        // A lost file is not a change to release; `update` reports it separately.
        return false;
    }
    let base = target.dir.local.join("CVS").join("Base").join(&entry.name);
    if base.is_file() {
        let (Ok(working), Ok(saved)) = (std::fs::read(&local), std::fs::read(&base)) else {
            return true;
        };
        return working != saved;
    }
    let Some(modified) = codec::file_mtime(&local) else {
        return false;
    };
    !codec::timestamps_equal(&codec::asctime(modified), &entry.date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::engine::test_sink::RecordingSink;
    use crate::client::work::dir::WorkingDir;
    use crate::client::work::entries::Entries;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_cluster_sets_delete_and_keeps_every_name() {
        let opts = parse_args(&args(&["-dd", "one", "two"])).unwrap();
        assert!(opts.delete);
        assert_eq!(opts.names, ["one", "two"]);
    }

    #[test]
    fn options_may_follow_a_folder_name() {
        // This client accepts options anywhere, unlike getopt's "+Qdq".
        let opts = parse_args(&args(&["sub", "-d", "other"])).unwrap();
        assert!(opts.delete);
        assert_eq!(opts.names, ["sub", "other"]);
    }

    #[test]
    fn a_quiet_option_after_the_command_name_is_reported() {
        assert_eq!(
            parse_args(&args(&["-d", "-Q", "sub"])),
            Err(ParseError::QuietAfterCommand)
        );
        assert_eq!(
            parse_args(&args(&["sub", "-Q"])),
            Err(ParseError::QuietAfterCommand)
        );
    }

    #[test]
    fn an_unknown_option_prints_the_usage() {
        assert_eq!(parse_args(&args(&["-x"])), Err(ParseError::Usage));
        assert_eq!(parse_args(&args(&["-dx", "sub"])), Err(ParseError::Usage));
    }

    #[test]
    fn the_double_dash_ends_the_options() {
        let opts = parse_args(&args(&["--", "-d"])).unwrap();
        assert!(!opts.delete);
        assert_eq!(opts.names, ["-d"]);
        let opts = parse_args(&args(&["-d", "--", "-x"])).unwrap();
        assert!(opts.delete);
        assert_eq!(opts.names, ["-x"]);
    }

    #[test]
    fn only_the_reference_letters_count_as_altered() {
        let mut inner = RecordingSink::default();
        {
            let mut counter = CountingSink::new(&mut inner);
            for line in [
                "M a.txt", "U b.txt", "A c.txt", "R d.txt", "C e.txt", "? f.txt",
            ] {
                counter.out(line.to_string());
            }
            assert_eq!(counter.altered, 4);
            assert!(counter.conflict, "a C line is a conflict");
        }
        assert_eq!(inner.lines.len(), 6, "every line is still printed");
    }

    #[test]
    fn a_move_away_complaint_is_a_conflict_too() {
        let mut inner = RecordingSink::default();
        {
            let mut counter = CountingSink::new(&mut inner);
            counter.err("cvs update: move away b.txt; it is in the way".to_string());
            assert!(counter.conflict);
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let folder = std::env::temp_dir().join(format!(
            "cvsdesk-release-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(folder.join("CVS")).unwrap();
        std::fs::write(folder.join("CVS").join("Repository"), "mod\n").unwrap();
        folder
    }

    fn target(folder: &Path, entries: Entries) -> Target {
        Target {
            dir: WorkingDir::new(
                folder.to_path_buf(),
                String::new(),
                "mod".to_string(),
                codec::Codec::Utf8,
            ),
            entries,
            filter: Vec::new(),
            selected: true,
            loose: Vec::new(),
        }
    }

    #[test]
    fn any_dash_revision_is_a_removed_file() {
        let folder = scratch("removed");
        // `-2.4`, not the old `-1`: is_removed() alone would miss it.
        let entries = Entries::parse("/gone.txt/-2.4/date//\n");
        let target = target(&folder, entries);
        let entry = target.entries.file("gone.txt").unwrap();
        assert_eq!(local_letter(&target, entry), Some('R'));
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_conflict_date_is_conflicted_but_a_clean_merge_is_not() {
        let conflict =
            Entry::parse("/a.txt/1.2/Result of merge+Tue Nov 24 09:52:53 1999//").unwrap();
        assert!(is_conflicted(&conflict));
        let clean = Entry::parse("/a.txt/1.2/Result of merge//").unwrap();
        assert!(!is_conflicted(&clean));
        // A clean merge waits for a commit, so it still counts as modified.
        let folder = scratch("clean-merge");
        let target = target(&folder, Entries::parse("/a.txt/1.2/Result of merge//\n"));
        let entry = target.entries.file("a.txt").unwrap();
        assert_eq!(local_letter(&target, entry), Some('M'));
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_touched_file_with_the_same_contents_is_not_modified() {
        let folder = scratch("base-equal");
        std::fs::write(folder.join("a.txt"), b"same\n").unwrap();
        std::fs::create_dir_all(folder.join("CVS").join("Base")).unwrap();
        std::fs::write(folder.join("CVS").join("Base").join("a.txt"), b"same\n").unwrap();
        let entries = Entries::parse("/a.txt/1.2/Tue Nov 24 09:52:53 1999//\n");
        let target = target(&folder, entries);
        let entry = target.entries.file("a.txt").unwrap();
        assert_eq!(local_letter(&target, entry), None);
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_changed_file_with_a_base_copy_is_modified() {
        let folder = scratch("base-changed");
        std::fs::write(folder.join("a.txt"), b"changed\n").unwrap();
        std::fs::create_dir_all(folder.join("CVS").join("Base")).unwrap();
        std::fs::write(folder.join("CVS").join("Base").join("a.txt"), b"base\n").unwrap();
        let entries = Entries::parse("/a.txt/1.2/Tue Nov 24 09:52:53 1999//\n");
        let target = target(&folder, entries);
        let entry = target.entries.file("a.txt").unwrap();
        assert_eq!(local_letter(&target, entry), Some('M'));
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn the_local_report_hides_ignored_unknown_files() {
        let folder = scratch("report");
        std::fs::write(folder.join(".cvsignore"), "*.o\n").unwrap();
        std::fs::write(folder.join("hello.txt"), b"changed\n").unwrap();
        std::fs::write(folder.join("build.o"), b"object\n").unwrap();
        std::fs::write(folder.join("notes.txt"), b"unknown\n").unwrap();
        std::fs::write(
            folder.join("CVS").join("Entries"),
            "/hello.txt/1.2/Tue Nov 24 09:52:53 1999//\n",
        )
        .unwrap();
        let mut sink = RecordingSink::default();
        let altered = report_local(&folder, &mut sink).unwrap();
        assert_eq!(altered, 1);
        let lines: Vec<&str> = sink.lines.iter().map(|(_, line)| line.as_str()).collect();
        assert!(lines.contains(&"M hello.txt"), "{:?}", lines);
        assert!(lines.contains(&"? notes.txt"), "{:?}", lines);
        assert!(
            !lines.iter().any(|line| line.contains("build.o")),
            "the ignore list must hide build output: {:?}",
            lines
        );
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn the_local_conflict_scan_reads_entries() {
        let folder = scratch("conflicts");
        std::fs::write(
            folder.join("CVS").join("Entries"),
            "/bad.txt/1.2/Result of merge+Tue Nov 24 09:52:53 1999//\n\
             /good.txt/1.2/Tue Nov 24 09:52:53 1999//\n",
        )
        .unwrap();
        let found = local_conflicts(&folder).unwrap();
        assert_eq!(found, ["bad.txt"]);
        std::fs::remove_dir_all(&folder).ok();
    }
}
