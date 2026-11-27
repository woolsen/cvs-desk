//! The `Notify` request `edit` and `unedit` send, and the `CVS/Notify` queue behind it.
//!
//! Neither command has a request of its own on the wire. The client names the folder, says
//! `Notify <file>` and then writes one raw notification line, which the server passes on to
//! the file's watchers. The line is a tab-separated record: the type (`E` for edit, `U` for
//! unedit), the UTC time, the machine, the invocation folder, and the watches the edit asked
//! for. The command ends with `noop`, which is what the server answers.
//!
//! The line is written to `CVS/Notify` *before* the server is asked, exactly as the reference
//! does (`edit.c:312-362`, `506-514`). If the connection cannot be made the notice stays in
//! that file and the next `edit`/`unedit` sends it (`edit.c:239-283`, `995-1049`). The server
//! answers each notice with `Notified`, and the queue is cleared only after its closing `ok`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::client::commands::targets::{self, Target};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::work::codec;
use crate::client::work::dir::{self, WorkingDir};
use crate::client::work::entries::Entries;

/// One file a command will visit: the folder key and the bare name.
pub type FileRef = (String, String);

/// What an `edit`/`unedit` command line selected.
pub struct Selection {
    /// The folders the request may name, including folders that only hold an unknown file.
    pub targets: BTreeMap<String, Target>,
    /// Every file to visit, in folder order. A removed entry is included: the reference
    /// visits it and the command decides what a missing working file means.
    pub files: Vec<FileRef>,
    /// Names the reference would report as `no such file …; ignored` instead of visiting.
    pub missing: Vec<String>,
}

/// Work out the files an `edit` or `unedit` visits.
///
/// A name that is on disk but not in `Entries` is still visited by the reference; only the
/// base registration is skipped. A missing named file is visited too, so `unedit` can put
/// back a base whose working file was deleted.
pub fn select_files(cwd: &Path, recursive: bool, names: &[String]) -> Result<Selection> {
    let expanded = crate::client::work::glob::expand_args(cwd, names);
    let mut directories = Vec::new();
    let mut versioned = Vec::new();
    let mut extras: BTreeMap<String, (WorkingDir, Entries, Vec<String>)> = BTreeMap::new();
    let mut missing = Vec::new();

    for name in &expanded {
        let path = cwd.join(name);
        if path.is_dir() {
            directories.push(name.clone());
            continue;
        }
        let parent = path.parent().unwrap_or(cwd);
        let file_name = path
            .file_name()
            .map(|part| part.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());
        if path.is_file() || parent.is_dir() {
            let Some(mut working) = dir::walk(parent, false)?.into_iter().next() else {
                if path.is_file() {
                    // Let `collect` give its "there is no version here" message.
                    versioned.push(name.clone());
                } else {
                    missing.push(name.clone());
                }
                continue;
            };
            // `walk` numbers a folder from itself; the protocol numbers it from the
            // invocation folder, the way `targets::collect` does.
            working.update_dir = path
                .parent()
                .and_then(|part| part.strip_prefix(cwd).ok())
                .map(|part| normalize_relative(&part.to_string_lossy()))
                .unwrap_or_default();
            let entries = working.entries()?;
            if path.is_file() && entries.file(&file_name).is_some() {
                versioned.push(name.clone());
            } else {
                let key = working.update_dir.clone();
                extras
                    .entry(key)
                    .or_insert_with(|| (working, entries, Vec::new()))
                    .2
                    .push(file_name);
            }
        } else {
            missing.push(name.clone());
        }
    }

    let mut wanted = directories;
    wanted.extend(versioned);
    let mut targets = if wanted.is_empty() {
        if expanded.is_empty() {
            targets::collect(cwd, recursive, &[])?
        } else {
            BTreeMap::new()
        }
    } else {
        targets::collect(cwd, recursive, &wanted)?
    };

    let mut extra_names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, (working, entries, names)) in extras {
        if let Some(target) = targets.get_mut(&key) {
            for name in &names {
                if !target.filter.iter().any(|existing| existing == name) {
                    target.filter.push(name.clone());
                }
            }
        } else {
            targets.insert(
                key.clone(),
                Target {
                    dir: working,
                    entries,
                    filter: names.clone(),
                    selected: false,
                    loose: Vec::new(),
                },
            );
        }
        extra_names.insert(key, names);
    }

    let mut files = Vec::new();
    for (key, target) in &targets {
        let mut in_dir = Vec::new();
        if target.selected {
            for entry in target.entries.files() {
                in_dir.push(entry.name.clone());
            }
        } else {
            for name in &target.filter {
                if target.entries.file(name).is_some() {
                    in_dir.push(name.clone());
                }
            }
        }
        if let Some(names) = extra_names.get(key) {
            for name in names {
                if !in_dir.iter().any(|existing| existing == name) {
                    in_dir.push(name.clone());
                }
            }
        }
        for name in in_dir {
            files.push((key.clone(), name));
        }
    }
    Ok(Selection {
        targets,
        files,
        missing,
    })
}

fn normalize_relative(text: &str) -> String {
    text.replace('\\', "/")
        .trim_matches('/')
        .trim_start_matches("./")
        .to_string()
}

/// The reference refuses a notification whose machine or folder holds one of the field
/// separators the file-attribute database uses (`edit.c:445-452`). A tab or newline in a file
/// name would break the `Notify` line just as badly.
pub fn check_fields(command: &str, cwd: &Path, files: &[FileRef]) -> Result<()> {
    const BAD: [char; 7] = ['+', ',', '>', ';', '=', '\t', '\n'];
    let host = hostname();
    if host.contains(BAD) {
        return Err(Error::plain(
            command,
            format!(
                "host name ({}) contains an invalid character (+,>;=\\t\\n)",
                host
            ),
        ));
    }
    let folder = cwd.display().to_string();
    if folder.contains(BAD) {
        return Err(Error::plain(
            command,
            format!(
                "current directory ({}) contains an invalid character (+,>;=\\t\\n)",
                folder
            ),
        ));
    }
    for (_, name) in files {
        if name.contains(['\t', '\n']) {
            return Err(Error::plain(
                command,
                format!(
                    "file name ({}) contains an invalid character (\\t\\n)",
                    name
                ),
            ));
        }
    }
    Ok(())
}

fn queue_path(folder: &Path) -> PathBuf {
    folder.join("CVS").join("Notify")
}

/// Append one notice to `folder/CVS/Notify`, before any server contact.
pub fn queue(
    command: &str,
    folder: &Path,
    kind: char,
    name: &str,
    watches: &str,
    cwd: &Path,
) -> Result<()> {
    use std::io::Write;
    let path = queue_path(folder);
    let mut text = format!("{}{}\t{}", kind, name, tail(watches, cwd));
    text.push_str(codec::newline());
    let bytes = codec::for_dir(folder).encode(&text)?;
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) => {
            return Err(Error::plain(
                command,
                format!("cannot open {}: {}", path.display(), error),
            ))
        }
    };
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(())
}

/// One line read back from `CVS/Notify`.
struct Queued {
    kind: char,
    name: String,
    /// Everything after the name and its tab: time, machine, folder, watches.
    tail: String,
}

fn read_queue(folder: &Path) -> Vec<Queued> {
    let Ok(bytes) = std::fs::read(queue_path(folder)) else {
        return Vec::new();
    };
    let text = codec::for_dir(folder).decode(&bytes);
    text.lines()
        .filter_map(|line| {
            let kind = line.chars().next()?;
            let rest = &line[kind.len_utf8()..];
            let (name, tail) = rest.split_once('\t')?;
            if name.is_empty() {
                return None;
            }
            Some(Queued {
                kind,
                name: name.to_string(),
                tail: tail.to_string(),
            })
        })
        .collect()
}

/// Send every notice queued in the selected folders, then `noop`. The queue file is left in
/// place until the server's `ok`, which is what makes an offline `edit`/`unedit` recoverable.
pub fn send_pending(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
) -> Result<()> {
    let mut sent = Vec::new();
    for (key, target) in targets {
        let path = queue_path(&target.dir.local);
        let queued = read_queue(&target.dir.local);
        if queued.is_empty() && !path.exists() {
            continue;
        }
        if !queued.is_empty() {
            let absolute =
                dir::absolute_repository(session.root.protocol_root(), &target.dir.repository);
            session.directory(&targets::display_dir(key), &absolute, &target.dir.local)?;
            for notice in &queued {
                session.send(&format!("Notify {}", notice.name))?;
                session.send(&format!("{}\t{}", notice.kind, notice.tail))?;
            }
        }
        sent.push(target.dir.local.clone());
    }
    session.send("noop")?;
    session.flush()?;
    // A server `error` means the notices were not accepted; keep the files so the next
    // command can try again, as an offline run would.
    targets::read_text(session, false)?;
    for folder in sent {
        match std::fs::remove_file(queue_path(&folder)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                session.err(format!(
                    "cvs {}: cannot remove {}: {}",
                    session.command,
                    queue_path(&folder).display(),
                    error
                ));
            }
        }
    }
    Ok(())
}

/// The bare `Notify` line, e.g. `E\tWed Nov 25 02:52:33 2026 GMT\tHOST\tC:\work\tEUC`.
pub fn line(kind: char, watches: &str, cwd: &Path) -> String {
    format!("{}\t{}", kind, tail(watches, cwd))
}

fn tail(watches: &str, cwd: &Path) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        timestamp(),
        hostname(),
        cwd.display(),
        watches
    )
}

/// The reference writes the time in UTC, in `asctime` shape with a literal `GMT`.
fn timestamp() -> String {
    Utc::now().format("%a %b %e %H:%M:%S %Y GMT").to_string()
}

/// The machine name the notification carries. Windows keeps it in `COMPUTERNAME`; a Unix
/// shell usually exports `HOSTNAME`. The reference falls back to `localhost` when it cannot
/// ask the system, which is what an unset environment looks like here.
fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_line_carries_four_tabs_and_the_watches_last() {
        let text = line('E', "EUC", Path::new("C:\\work"));
        assert_eq!(text.split('\t').collect::<Vec<_>>().len(), 5, "{}", text);
        assert!(text.starts_with("E\t"), "{}", text);
        assert!(text.ends_with("\tEUC"), "{}", text);
    }

    #[test]
    fn an_unedit_line_ends_with_an_empty_watches_field() {
        let text = line('U', "", Path::new("/work"));
        assert!(text.starts_with("U\t"), "{}", text);
        assert!(text.ends_with('\t'), "{}", text);
    }

    #[test]
    fn a_queued_line_reads_back_whole() {
        let folder = std::env::temp_dir().join(format!(
            "cvsdesk-notify-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(folder.join("CVS")).unwrap();
        queue("edit", &folder, 'E', "a.txt", "EUC", Path::new("C:\\work")).unwrap();
        let queued = read_queue(&folder);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, 'E');
        assert_eq!(queued[0].name, "a.txt");
        assert!(queued[0].tail.ends_with("\tEUC"), "{}", queued[0].tail);
        std::fs::remove_dir_all(&folder).ok();
    }

    fn scratch(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!(
            "cvsdesk-select-{}-{}-{}",
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

    #[test]
    fn no_name_selects_every_entry_in_the_folder() {
        let folder = scratch("all");
        std::fs::write(
            folder.join("CVS").join("Entries"),
            "/a.txt/1.1///\n/b.txt/1.2///\n",
        )
        .unwrap();
        let selection = select_files(&folder, true, &[]).unwrap();
        assert_eq!(
            selection.files,
            vec![
                (String::new(), "a.txt".to_string()),
                (String::new(), "b.txt".to_string())
            ]
        );
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_named_file_without_an_entry_is_still_visited() {
        let folder = scratch("unknown");
        std::fs::write(folder.join("CVS").join("Entries"), "/a.txt/1.1///\n").unwrap();
        std::fs::write(folder.join("mine.txt"), b"mine\n").unwrap();
        let selection = select_files(&folder, true, &["mine.txt".to_string()]).unwrap();
        assert_eq!(
            selection.files,
            vec![(String::new(), "mine.txt".to_string())],
            "the reference chmods and notifies an existing name it does not know"
        );
        assert!(selection.missing.is_empty());
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_named_file_that_is_gone_is_visited_so_the_command_can_warn() {
        let folder = scratch("gone");
        std::fs::write(folder.join("CVS").join("Entries"), "/a.txt/1.1///\n").unwrap();
        let selection = select_files(&folder, true, &["gone.txt".to_string()]).unwrap();
        assert_eq!(
            selection.files,
            vec![(String::new(), "gone.txt".to_string())],
            "a missing file is reported by the visitor, not dropped here"
        );
        assert!(selection.missing.is_empty());
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_removed_entry_is_still_visited_for_unedit() {
        // `cvs remove` leaves the `-1` entry and does not touch `CVS/Base`, so an unedit
        // that follows it must still see the name and can put the saved copy back.
        let folder = scratch("removed");
        std::fs::write(
            folder.join("CVS").join("Entries"),
            "/gone.txt/-1/Thu Nov 26 10:00:00 2026//\n",
        )
        .unwrap();
        let selection = select_files(&folder, true, &[]).unwrap();
        assert_eq!(
            selection.files,
            vec![(String::new(), "gone.txt".to_string())]
        );
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_directory_name_brings_its_entries_under_its_key() {
        let folder = scratch("dir");
        let sub = folder.join("d");
        std::fs::create_dir_all(sub.join("CVS")).unwrap();
        std::fs::write(sub.join("CVS").join("Repository"), "mod/d\n").unwrap();
        std::fs::write(sub.join("CVS").join("Entries"), "/one.txt/1.1///\n").unwrap();
        let selection = select_files(&folder, false, &["d".to_string()]).unwrap();
        assert_eq!(
            selection.files,
            vec![("d".to_string(), "one.txt".to_string())]
        );
        std::fs::remove_dir_all(&folder).ok();
    }
}
