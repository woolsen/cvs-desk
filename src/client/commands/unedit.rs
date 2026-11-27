//! `cvs unedit`: say that a watched file is no longer being edited.
//!
//! Like `edit` it has no request of its own. The client names the folder, sends one `Notify`
//! per file with a `U` line, and finishes with `noop`; the file itself is made read-only
//! again, which is what marks it unedited for the next checkout.
//!
//! Only a file with a `CVS/Base` copy is touched (`edit.c:485-491`). A file that was never
//! edited is left alone silently: no prompt, no read-only bit, no notice. The "modified?"
//! question compares the working file with that base byte for byte, as `xcmp` does
//! (`edit.c:493-502`), and the base is put back before the server is contacted. The revision
//! recorded in `CVS/Baserev` goes back into `CVS/Entries` (`edit.c:537-575`), so a later
//! update cannot leave the entry describing a different revision than the restored file.
//!
//! The answer to the question comes from the sink, and the file is kept as it is unless the
//! answer starts with `y`. A declined file stays writable and is not reported to the server.

use std::path::Path;

use crate::client::cli;
use crate::client::commands::notify::{self, FileRef};
use crate::client::commands::targets::Target;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::root::Root;
use crate::client::work::base;
use crate::client::work::dir;
use crate::client::work::entries::Entry;

#[derive(Debug)]
struct Options {
    recursive: bool,
    names: Vec<String>,
}

/// One parsed command line, or the usage the reference prints instead.
enum Parsed {
    Options(Options),
    /// The usage, with the complaint line that goes before it when there is one.
    Usage(Option<String>),
}

/// Like `add`, `remove` and `edit`, `unedit` stops taking options at the first name. `-l` and
/// `-R` may be clustered (`-lR`), the way getopt accepts them.
fn parse_args(args: &[String]) -> Parsed {
    let mut opts = Options {
        recursive: true,
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
        for option in arg[1..].chars() {
            match option {
                'l' => opts.recursive = false,
                'R' => opts.recursive = true,
                other => {
                    return Parsed::Usage(Some(format!("unedit: invalid option -- {}", other)))
                }
            }
        }
        i += 1;
    }
    Parsed::Options(opts)
}

/// Run `cvs unedit`.
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
            if let Some(text) = cli::command_help("unedit") {
                sink.err(text.to_string());
            }
            return Ok(1);
        }
    };
    if !cwd.join("CVS").is_dir() {
        return Err(Error::aborted(
            "unedit",
            "there is no version here; run 'cvs checkout' first",
        ));
    }

    notify::check_fields("unedit", cwd, &[])?;
    let selection = notify::select_files(cwd, opts.recursive, &opts.names)?;
    notify::check_fields("unedit", cwd, &selection.files)?;
    if !root.options.no_execute {
        revert_edited(&selection.targets, &selection.files, cwd, sink)?;
    }

    // The reference still opens the server and sends `noop` when nothing was reverted; a
    // notice left by an earlier offline run is sent from here.
    let mut session = protocol::open(root, "unedit", cancel, sink)?;
    notify::send_pending(&mut session, &selection.targets)?;
    session.close();
    Ok(0)
}

/// Put every saved base back, queue its `U` notice, and make the file read-only again. The
/// base is the only evidence that a file was taken with `edit`; without it the file is not
/// this command's business.
fn revert_edited(
    targets: &std::collections::BTreeMap<String, Target>,
    files: &[FileRef],
    cwd: &Path,
    sink: &mut dyn Sink,
) -> Result<()> {
    for (key, name) in files {
        let target = targets.get(key).ok_or_else(|| {
            Error::aborted(
                "unedit",
                format!("the folder '{}' is missing from the selection", key),
            )
        })?;
        let folder = &target.dir.local;
        let base_file = folder.join("CVS").join("Base").join(name);
        if !base_file.is_file() {
            // Never `cvs edit`'d, or already unedited: silent, as `edit.c:485-491` says.
            continue;
        }
        let local = folder.join(name);
        let shown = dir::display_path(&target.dir.update_dir, name);
        if !same_content(&local, &base_file) {
            if !sink.confirm(format!("{} has been modified; revert changes? ", shown)) {
                continue;
            }
        }

        let revision = base::revision(folder, name);
        if !base::restore(folder, name)? {
            continue;
        }
        notify::queue("unedit", folder, 'U', name, "", cwd)?;

        // `Register` needs both an entry and a recorded revision. A file the server never
        // knew (`edit` on an unknown name) has a base but no Baserev line; the reference
        // leaves it alone because there is no entry to update.
        let entry = target.dir.entries()?.file(name).cloned();
        match (revision, entry) {
            (Some(revision), Some(entry)) => {
                // The entry describes the revision the working file now has.
                let mut entries = target.dir.entries()?;
                entries.set_file(Entry { revision, ..entry });
                target.dir.write_entries(&entries)?;
                dir::set_readonly(&local, true)?;
            }
            (None, Some(_)) => {
                // `CVS/Baserev` is corrupt or was lost (`edit.c:550-568`): the file cannot be
                // tied to a revision, so the reference removes it and asks for an update.
                sink.err(format!(
                    "cvs unedit: {} not mentioned in CVS/Baserev",
                    shown
                ));
                dir::clear_readonly(&local).ok();
                match std::fs::remove_file(&local) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        sink.err(format!("cvs unedit: cannot remove {}: {}", shown, error))
                    }
                }
                sink.err("cvs unedit: run update to complete the unedit".to_string());
            }
            _ => {
                // No entry: nothing to register; just take the bit back.
                dir::set_readonly(&local, true)?;
            }
        }
    }
    Ok(())
}

/// `xcmp`: are the working file and the base the same bytes? A missing or unreadable working
/// file counts as changed, which makes the command ask before putting the base back.
fn same_content(local: &Path, base: &Path) -> bool {
    match (std::fs::read(local), std::fs::read(base)) {
        (Ok(working), Ok(saved)) => working == saved,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::work::codec::Codec;
    use crate::client::work::dir::WorkingDir;
    use crate::client::work::entries::Entries;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Result<Options> {
        match parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>()) {
            Parsed::Options(opts) => Ok(opts),
            Parsed::Usage(line) => Err(Error::new(line.unwrap_or_else(|| "usage".to_string()))),
        }
    }

    #[test]
    fn the_recursion_flags_are_read_and_r_is_the_default_again() {
        let opts = parse(&["-l", "-R", "a.txt"]).unwrap();
        assert!(opts.recursive);
        assert_eq!(opts.names, vec!["a.txt"]);
    }

    #[test]
    fn clustered_recursion_flags_are_accepted() {
        let opts = parse(&["-lR", "a.txt"]).unwrap();
        assert!(opts.recursive);
        let opts = parse(&["-Rl", "a.txt"]).unwrap();
        assert!(!opts.recursive);
    }

    #[test]
    fn a_name_then_a_dash_word_is_a_name() {
        let opts = parse(&["a.txt", "-l"]).unwrap();
        assert!(opts.recursive);
        assert_eq!(opts.names, vec!["a.txt", "-l"]);
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let error = parse(&["-c"]).unwrap_err();
        assert!(error.text().contains("invalid option"), "{}", error);
        assert!(!error.text().contains("-- -c"), "{}", error);
    }

    #[test]
    fn a_double_dash_ends_the_options() {
        let opts = parse(&["--", "-l"]).unwrap();
        assert_eq!(opts.names, vec!["-l"]);
    }

    fn scratch(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!(
            "cvsdesk-unedit-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(folder.join("CVS")).unwrap();
        folder
    }

    fn targets(folder: &std::path::Path, entry: Entry) -> BTreeMap<String, Target> {
        let mut entries = Entries::default();
        entries.set_file(entry);
        entries.write(folder, Codec::Utf8).unwrap();
        let dir = WorkingDir::new(
            folder.to_path_buf(),
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
        targets
    }

    fn files(name: &str) -> Vec<FileRef> {
        vec![(String::new(), name.to_string())]
    }

    /// A sink whose answer to every question is yes.
    struct YesSink(Vec<String>);

    impl Sink for YesSink {
        fn out(&mut self, _line: String) {}
        fn err(&mut self, _line: String) {}
        fn confirm(&mut self, text: String) -> bool {
            self.0.push(text);
            true
        }
    }

    #[test]
    fn a_file_without_a_base_is_left_alone_silently() {
        let folder = scratch("no-base");
        std::fs::write(folder.join("hello.txt"), b"hello\n").unwrap();
        let entry = Entry::new("hello.txt", "1.2", "Thu Nov 26 10:00:00 2026", "", "");
        let targets = targets(&folder, entry);
        let mut sink = YesSink(Vec::new());
        revert_edited(&targets, &files("hello.txt"), &folder, &mut sink).unwrap();
        assert!(
            !std::fs::metadata(folder.join("hello.txt"))
                .unwrap()
                .permissions()
                .readonly(),
            "an unedited file keeps its writable bit"
        );
        assert!(!folder.join("CVS").join("Notify").exists());
        assert!(!folder.join("CVS").join("Base").join("hello.txt").exists());
        assert!(sink.0.is_empty(), "no question is asked");
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn equal_content_is_restored_without_a_question_and_the_revision_returns() {
        let folder = scratch("restore");
        let base = folder.join("CVS").join("Base");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("hello.txt"), b"hello\n").unwrap();
        std::fs::write(folder.join("CVS").join("Baserev"), "Bhello.txt/1.1/\n").unwrap();
        std::fs::write(folder.join("hello.txt"), b"hello\n").unwrap();
        let entry = Entry::new("hello.txt", "1.2", "Thu Nov 26 10:00:00 2026", "", "");
        let targets = targets(&folder, entry);
        let mut sink = YesSink(Vec::new());
        revert_edited(&targets, &files("hello.txt"), &folder, &mut sink).unwrap();

        assert!(sink.0.is_empty(), "equal content asks nothing");
        assert!(std::fs::metadata(folder.join("hello.txt"))
            .unwrap()
            .permissions()
            .readonly());
        assert!(!base.join("hello.txt").exists(), "the base is consumed");
        let entries = Entries::read(&folder).unwrap();
        assert_eq!(entries.file("hello.txt").unwrap().revision, "1.1");
        assert_eq!(
            entries.file("hello.txt").unwrap().date,
            "Thu Nov 26 10:00:00 2026"
        );
        let notice = std::fs::read_to_string(folder.join("CVS").join("Notify")).unwrap();
        assert!(notice.starts_with("Uhello.txt\t"), "{}", notice);
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn an_unknown_file_with_a_base_is_kept_because_there_is_no_entry() {
        // `edit` on a name the server does not know keeps a base but writes no Baserev line.
        // The reference has nothing to register, so `unedit` only takes the writable bit
        // back; it must not treat the missing line as a corrupt Baserev and delete the file.
        let folder = scratch("unknown");
        let base = folder.join("CVS").join("Base");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("new.txt"), b"mine\n").unwrap();
        std::fs::write(folder.join("new.txt"), b"mine\n").unwrap();
        let target = Target {
            dir: WorkingDir::new(
                folder.clone(),
                String::new(),
                "mod".to_string(),
                Codec::Utf8,
            ),
            entries: Entries::default(),
            filter: Vec::new(),
            selected: true,
            loose: Vec::new(),
        };
        let mut targets = BTreeMap::new();
        targets.insert(String::new(), target);
        let mut sink = YesSink(Vec::new());
        revert_edited(&targets, &files("new.txt"), &folder, &mut sink).unwrap();

        assert_eq!(std::fs::read(folder.join("new.txt")).unwrap(), b"mine\n");
        assert!(std::fs::metadata(folder.join("new.txt"))
            .unwrap()
            .permissions()
            .readonly());
        assert!(!base.join("new.txt").exists(), "the base is consumed");
        let notice = crate::client::work::codec::for_dir(&folder)
            .decode(&std::fs::read(folder.join("CVS").join("Notify")).unwrap());
        assert!(notice.starts_with("Unew.txt\t"), "{}", notice);
        assert!(sink.0.is_empty(), "no question is asked");
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_missing_baserev_removes_the_file_and_asks_for_an_update() {
        // `edit.c:550-568`: a base with no Baserev line cannot be tied to a revision, so the
        // restored file is removed and the user is told to run update.
        let folder = scratch("no-baserev");
        let base = folder.join("CVS").join("Base");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("hello.txt"), b"hello\n").unwrap();
        std::fs::write(folder.join("hello.txt"), b"hello\n").unwrap();
        let entry = Entry::new("hello.txt", "1.2", "Thu Nov 26 10:00:00 2026", "", "");
        let targets = targets(&folder, entry);
        let mut sink = crate::client::engine::test_sink::RecordingSink::default();
        revert_edited(&targets, &files("hello.txt"), &folder, &mut sink).unwrap();

        assert!(!folder.join("hello.txt").exists());
        assert!(!base.join("hello.txt").exists());
        let notice = std::fs::read_to_string(folder.join("CVS").join("Notify")).unwrap();
        assert!(notice.starts_with("Uhello.txt\t"), "{}", notice);
        assert!(
            sink.lines
                .iter()
                .any(|(_, line)| line.contains("hello.txt not mentioned in CVS/Baserev")),
            "{:?}",
            sink.lines
        );
        assert!(
            sink.lines
                .iter()
                .any(|(_, line)| line.contains("run update to complete the unedit")),
            "{:?}",
            sink.lines
        );
        assert_eq!(
            Entries::read(&folder)
                .unwrap()
                .file("hello.txt")
                .unwrap()
                .revision,
            "1.2",
            "the entry is left for update to repair"
        );
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_declined_revert_keeps_the_changes_and_the_base() {
        let folder = scratch("declined");
        let base = folder.join("CVS").join("Base");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("hello.txt"), b"hello\n").unwrap();
        std::fs::write(folder.join("CVS").join("Baserev"), "Bhello.txt/1.1/\n").unwrap();
        std::fs::write(folder.join("hello.txt"), b"changed\n").unwrap();
        let entry = Entry::new("hello.txt", "1.2", "Thu Nov 26 10:00:00 2026", "", "");
        let targets = targets(&folder, entry);
        let mut sink = crate::client::engine::test_sink::RecordingSink::default();
        revert_edited(&targets, &files("hello.txt"), &folder, &mut sink).unwrap();

        assert_eq!(
            std::fs::read(folder.join("hello.txt")).unwrap(),
            b"changed\n"
        );
        assert!(base.join("hello.txt").exists());
        assert!(!folder.join("CVS").join("Notify").exists());
        assert_eq!(
            Entries::read(&folder)
                .unwrap()
                .file("hello.txt")
                .unwrap()
                .revision,
            "1.2"
        );
        assert!(
            sink.lines
                .iter()
                .any(|(_, line)| line.contains("hello.txt has been modified; revert changes? ")),
            "{:?}",
            sink.lines
        );
        std::fs::remove_dir_all(&folder).ok();
    }
}
