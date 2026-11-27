//! `cvs edit`: make a watched file writable and tell the server it is being edited.
//!
//! There is no `edit` request on the wire. The client names the folder, then sends one
//! `Notify` per file with the raw notification line, and finishes with `noop`; the server
//! hands the notice to the file's watchers. The local side effect is the read-only bit: `edit`
//! makes the file writable so the user can work on it.
//!
//! The local half happens first, as it does in the reference (`edit.c:312-362,456-461`): the
//! notice goes into `CVS/Notify`, the file is made writable, and a writable copy is kept in
//! `CVS/Base` for `unedit`. Only then is the server contacted. If it cannot be reached the
//! notice stays queued and the next `edit`/`unedit` sends it.
//!
//! `-c` is the careful form (a CVSNT extension). It first asks the server who is already
//! editing (`editors`) and stops with `files being edited!` when the answer names anyone;
//! only a clean answer goes on to the notification and the writable bit.

use std::collections::BTreeMap;
use std::path::Path;

use crate::client::cli;
use crate::client::commands::notify::{self, FileRef};
use crate::client::commands::targets::{self, Target};
use crate::client::commands::watch;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;
use crate::client::work::base;
use crate::client::work::dir;

#[derive(Debug)]
struct Options {
    /// `-c`: check with the server that nobody else is editing before going ahead.
    check: bool,
    recursive: bool,
    /// `-a` was named at least once; without it all three temporary watches are set.
    action_given: bool,
    tedit: bool,
    tunedit: bool,
    tcommit: bool,
    names: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            check: false,
            recursive: true,
            action_given: false,
            tedit: false,
            tunedit: false,
            tcommit: false,
            names: Vec::new(),
        }
    }
}

impl Options {
    /// The watches field of the notification, always in the protocol's fixed E, U, C order
    /// (`edit.c:321-327`). `none` really means none.
    fn watches(&self) -> String {
        let mut letters = String::new();
        if self.tedit {
            letters.push('E');
        }
        if self.tunedit {
            letters.push('U');
        }
        if self.tcommit {
            letters.push('C');
        }
        letters
    }

    fn apply_action(&mut self, action: &str) {
        self.action_given = true;
        match action {
            "edit" => self.tedit = true,
            "unedit" => self.tunedit = true,
            "commit" => self.tcommit = true,
            "all" => {
                self.tedit = true;
                self.tunedit = true;
                self.tcommit = true;
            }
            "none" => {
                self.tedit = false;
                self.tunedit = false;
                self.tcommit = false;
            }
            _ => {}
        }
    }
}

/// One parsed command line, or the usage the reference prints instead.
enum Parsed {
    Options(Options),
    /// The usage, with the complaint line that goes before it when there is one.
    Usage(Option<String>),
}

/// Like `add` and `remove`, `edit` stops taking options at the first name. Short options may
/// be clustered the way getopt accepts them (`-lR`, `-aedit`). `-c` and `-f` are opposites;
/// the last one wins.
fn parse_args(args: &[String]) -> Parsed {
    let mut opts = Options::default();
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
        let cluster: Vec<char> = arg[1..].chars().collect();
        let mut j = 0;
        while j < cluster.len() {
            match cluster[j] {
                'c' => {
                    opts.check = true;
                    j += 1;
                }
                'f' => {
                    // `-f` is the default state; it only undoes an earlier `-c`.
                    opts.check = false;
                    j += 1;
                }
                'l' => {
                    opts.recursive = false;
                    j += 1;
                }
                'R' => {
                    opts.recursive = true;
                    j += 1;
                }
                'a' => {
                    // The value is the rest of the cluster (`-aedit`) or the next argument.
                    let value: String = if j + 1 < cluster.len() {
                        cluster[j + 1..].iter().collect()
                    } else {
                        i += 1;
                        match args.get(i) {
                            Some(value) => value.clone(),
                            None => {
                                return Parsed::Usage(Some(
                                    "edit: option requires an argument -- a".to_string(),
                                ))
                            }
                        }
                    };
                    if watch::action_letters(&value).is_none() {
                        return Parsed::Usage(None);
                    }
                    opts.apply_action(&value);
                    j = cluster.len();
                }
                other => return Parsed::Usage(Some(format!("edit: invalid option -- {}", other))),
            }
        }
        i += 1;
    }
    if !opts.action_given {
        // The reference watches every action when no `-a` is named.
        opts.apply_action("all");
        opts.action_given = false;
    }
    Parsed::Options(opts)
}

/// Run `cvs edit`.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let opts = match parse_args(args) {
        Parsed::Options(opts) => opts,
        Parsed::Usage(complaint) => {
            if let Some(line) = complaint {
                sink.err(line);
            }
            if let Some(text) = cli::command_help("edit") {
                sink.err(text.to_string());
            }
            return Ok(1);
        }
    };
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "edit",
            "there is no version here; run 'cvs checkout' first",
        ));
    }
    notify::check_fields("edit", cwd, &[])?;
    let selection = notify::select_files(cwd, opts.recursive, &opts.names)?;
    notify::check_fields("edit", cwd, &selection.files)?;
    // `-n` makes no change of its own, and the reference says nothing about files it would
    // not touch; the warnings belong with the rest of the local half. A notice left by an
    // earlier offline run is still sent, as the reference does even under `noexec`.
    let no_execute = root.options.no_execute;
    if !no_execute {
        for name in &selection.missing {
            sink.err(format!("cvs edit: no such file {}; ignored", name));
        }
    }
    let quiet = root.options.quiet >= 2;
    let watches = opts.watches();
    let existing: Vec<FileRef> = selection
        .files
        .iter()
        .filter(|(key, name)| {
            selection
                .targets
                .get(key)
                .is_some_and(|target| target.dir.local.join(name).is_file())
        })
        .cloned()
        .collect();

    if opts.check {
        // The check is the only part of the reference's careful form that needs the server,
        // and it must run before any local change. The notifications go out on the same
        // connection: the reference does not open a second one, and a test server or a
        // server that limits connections should not have to see two.
        let mut session = protocol::open(root, "edit", cancel, sink)?;
        let check_names = crate::client::work::glob::expand_args(cwd, &opts.names);
        let clear = check_edited(
            &mut session,
            &selection.targets,
            &existing,
            &check_names,
            quiet,
            cwd,
        )?;
        if !clear {
            session.close();
            return Err(Error::aborted("edit", "Files being edited!"));
        }
        if !no_execute {
            // The local half has no connection of its own to write to, so its warnings go
            // through the open session's sink.
            let mut bridge = SessionSink {
                session: &mut session,
            };
            apply_local_edits(
                &selection.targets,
                &selection.files,
                &watches,
                cwd,
                &mut bridge,
            )?;
        }
        notify::send_pending(&mut session, &selection.targets)?;
        session.close();
        return Ok(0);
    }

    if !no_execute {
        apply_local_edits(&selection.targets, &selection.files, &watches, cwd, sink)?;
    }
    let mut session = protocol::open(root, "edit", cancel, sink)?;
    notify::send_pending(&mut session, &selection.targets)?;
    session.close();
    Ok(0)
}

/// A sink that sends the local half's warnings through the session that is already open.
/// `-c` has to keep its connection while it makes the edit, so the ordinary sink is still
/// borrowed by the session.
struct SessionSink<'a, 'b> {
    session: &'a mut protocol::Session<'b>,
}

impl Sink for SessionSink<'_, '_> {
    fn out(&mut self, line: String) {
        self.session.out(line);
    }

    fn err(&mut self, line: String) {
        self.session.err(line);
    }

    fn confirm(&mut self, text: String) -> bool {
        self.session.confirm(text)
    }
}

/// The reference's local half, in its order: queue the notice, make the working file
/// writable, then keep the writable copy (`edit.c:312-362`). A file that is gone is only
/// reported, as `edit.c:306-310` does.
fn apply_local_edits(
    targets: &BTreeMap<String, Target>,
    files: &[FileRef],
    watches: &str,
    cwd: &Path,
    sink: &mut dyn Sink,
) -> Result<()> {
    for (key, name) in files {
        let target = targets.get(key).ok_or_else(|| {
            Error::aborted(
                "edit",
                format!("the folder '{}' is missing from the selection", key),
            )
        })?;
        let local = target.dir.local.join(name);
        if !local.is_file() {
            sink.err(format!(
                "cvs edit: no such file {}; ignored",
                dir::display_path(&target.dir.update_dir, name)
            ));
            continue;
        }
        notify::queue("edit", &target.dir.local, 'E', name, watches, cwd)?;
        if let Err(error) = dir::clear_readonly(&local) {
            // The reference warns and carries on: the server notice and the base are still
            // worth having, and the next command can retry the bit.
            sink.err(format!(
                "cvs edit: cannot change mode of file {}: {}",
                dir::display_path(&target.dir.update_dir, name),
                error
            ));
        }
        let revision = target
            .entries
            .file(name)
            .map(|entry| entry.revision.clone());
        base::save(&target.dir.local, name, revision.as_deref())?;
    }
    Ok(())
}

/// `-c`: send the file blocks and one `editors` request per file, reading each answer before
/// the next name (the server resets its argument list after every request). Returns true when
/// nobody else is editing; false has already sent the closing `noop` and leaves the stop to
/// the caller.
fn check_edited(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
    files: &[FileRef],
    names: &[String],
    quiet: bool,
    cwd: &Path,
) -> Result<bool> {
    // CVSNT's check_edits sends `--` before the file names (`send_arg ("--")`).
    targets::begin_files(session, names)?;
    let _ = targets::send_blocks_watch(session, targets)?;
    // `send_files` returns the server to the invocation folder before the names go out
    // (`client.c:5634-5643`). Without this, files in more than one folder would all be
    // looked up below the last folder block instead of below the invocation folder.
    if let Some(repository) = dir::read_repository(cwd) {
        let absolute = dir::absolute_repository(session.root.protocol_root(), &repository);
        session.directory(".", &absolute, cwd)?;
    }

    let mut edited = false;
    for (key, name) in files {
        let target = targets.get(key).ok_or_else(|| {
            Error::aborted(
                "edit",
                format!("the folder '{}' is missing from the selection", key),
            )
        })?;
        let shown = dir::display_path(&target.dir.update_dir, name);
        session.argument(&shown)?;
        session.send("editors")?;
        session.flush()?;
        loop {
            match session.read_response()? {
                protocol::Response::Ok => break,
                protocol::Response::M(text) => {
                    // `editors` answers one line per editor, starting with the file's
                    // `update_dir/name`. CVSNT's check counts only the file it asked about.
                    let first = text.split('\t').next().unwrap_or("");
                    if !first.eq_ignore_ascii_case(&shown) {
                        continue;
                    }
                    edited = true;
                    if !quiet {
                        session.out(text);
                    }
                }
                protocol::Response::E(text) => session.err(text),
                protocol::Response::Notified
                | protocol::Response::Trace(_)
                | protocol::Response::Checksum(_) => {}
                protocol::Response::Error { message, .. } => {
                    return Err(Error::aborted(
                        "edit",
                        if message.trim().is_empty() {
                            "the server reported an error".to_string()
                        } else {
                            message
                        },
                    ))
                }
                other => {
                    return Err(Error::aborted(
                        "edit",
                        format!("unexpected answer: {:?}", other),
                    ))
                }
            }
        }
    }
    if edited {
        // The reference has already queued its closing `noop` when it gives up. Read its
        // answer too: a server that is left with an unread reply does not always notice the
        // pipe close on Windows.
        session.send("noop")?;
        session.flush()?;
        let _ = session.read_to_ok();
        return Ok(false);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        match parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>()) {
            Parsed::Options(opts) => Ok(opts),
            Parsed::Usage(line) => Err(Error::new(line.unwrap_or_else(|| "usage".to_string()))),
        }
    }

    #[test]
    fn the_flags_are_read_until_the_first_name() {
        let opts = parse(&["-c", "-l", "-R", "a.txt"]).unwrap();
        assert!(opts.check);
        assert!(opts.recursive, "-R after -l turns recursion back on");
        assert_eq!(opts.names, vec!["a.txt"]);
    }

    #[test]
    fn the_last_of_c_and_f_wins() {
        let opts = parse(&["-c", "-f"]).unwrap();
        assert!(!opts.check);

        let opts = parse(&["-f", "-c"]).unwrap();
        assert!(opts.check);
    }

    #[test]
    fn short_options_may_be_clustered() {
        let opts = parse(&["-lR", "-aedit", "a.txt"]).unwrap();
        assert!(opts.recursive);
        assert_eq!(opts.watches(), "E");
        assert_eq!(opts.names, vec!["a.txt"]);

        let opts = parse(&["-lRa", "unedit", "a.txt"]).unwrap();
        assert!(opts.recursive);
        assert_eq!(opts.watches(), "U");
    }

    #[test]
    fn actions_keep_the_fixed_e_u_c_order() {
        let opts = parse(&["-a", "unedit", "-a", "edit", "a.txt"]).unwrap();
        assert_eq!(opts.watches(), "EU");
    }

    #[test]
    fn none_clears_the_actions_named_before_it() {
        let opts = parse(&["-a", "all", "-a", "none", "a.txt"]).unwrap();
        assert_eq!(opts.watches(), "");
        let opts = parse(&["-a", "edit", "-a", "none", "-a", "unedit", "a.txt"]).unwrap();
        assert_eq!(opts.watches(), "U");
    }

    #[test]
    fn no_action_means_all_three_watches() {
        let opts = parse(&["a.txt"]).unwrap();
        assert_eq!(opts.watches(), "EUC");
    }

    #[test]
    fn a_name_then_a_dash_word_is_a_name() {
        let opts = parse(&["a.txt", "-c"]).unwrap();
        assert!(!opts.check);
        assert_eq!(opts.names, vec!["a.txt", "-c"]);
    }

    #[test]
    fn an_unknown_option_or_action_is_refused() {
        assert!(parse(&["-Z"])
            .unwrap_err()
            .text()
            .contains("invalid option"));
        assert!(!parse(&["-Z"]).unwrap_err().text().contains("-- -Z"));
        assert!(parse(&["-a", "bogus"])
            .unwrap_err()
            .text()
            .contains("usage"));
    }

    #[test]
    fn a_missing_action_is_refused() {
        let error = parse(&["-a"]).unwrap_err();
        assert!(
            error.text().contains("option requires an argument -- a"),
            "{}",
            error
        );
    }

    #[test]
    fn a_read_only_file_gets_a_writable_base_and_a_queued_notice() {
        use crate::client::work::codec::{self, Codec};
        use crate::client::work::dir::WorkingDir;
        use crate::client::work::entries::Entries;

        let folder = std::env::temp_dir().join(format!(
            "cvsdesk-edit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(folder.join("CVS")).unwrap();
        std::fs::write(folder.join("a.txt"), b"base\n").unwrap();
        dir::set_readonly(&folder.join("a.txt"), true).unwrap();

        let mut entries = Entries::default();
        entries.set_file(crate::client::work::entries::Entry::new(
            "a.txt", "1.1", "", "", "",
        ));
        entries.write(&folder, Codec::Utf8).unwrap();
        let dir = WorkingDir::new(
            folder.clone(),
            String::new(),
            "mod".to_string(),
            Codec::Utf8,
        );
        let mut targets = BTreeMap::new();
        targets.insert(
            String::new(),
            Target {
                dir,
                entries,
                filter: Vec::new(),
                selected: true,
                loose: Vec::new(),
            },
        );
        let files: Vec<FileRef> = vec![(String::new(), "a.txt".to_string())];
        let mut sink = crate::client::engine::test_sink::RecordingSink::default();
        apply_local_edits(&targets, &files, "EUC", &folder, &mut sink).unwrap();

        assert!(!std::fs::metadata(folder.join("a.txt"))
            .unwrap()
            .permissions()
            .readonly());
        let saved = folder.join("CVS").join("Base").join("a.txt");
        assert_eq!(std::fs::read(&saved).unwrap(), b"base\n");
        assert!(
            !std::fs::metadata(&saved).unwrap().permissions().readonly(),
            "the base is a writable copy"
        );
        let codec = codec::for_dir(&folder);
        let notice = codec.decode(&std::fs::read(folder.join("CVS").join("Notify")).unwrap());
        assert!(notice.starts_with("Ea.txt\t"), "{}", notice);
        assert!(notice.trim_end().ends_with("\tEUC"), "{}", notice);
        let baserev = codec.decode(&std::fs::read(folder.join("CVS").join("Baserev")).unwrap());
        assert!(baserev.contains("Ba.txt/1.1/"), "{}", baserev);
        std::fs::remove_dir_all(&folder).ok();
    }
}
