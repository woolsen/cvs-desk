//! The `Notify` request `edit` and `unedit` send.
//!
//! Neither command has a request of its own on the wire. The client names the folder, says
//! `Notify <file>` and then writes one raw notification line, which the server passes on to
//! the file's watchers. The line is a tab-separated record: the type (`E` for edit, `U` for
//! unedit), the UTC time, the machine, the invocation folder, and the watches the edit asked
//! for. The command ends with `noop`, which is what the server answers.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::Utc;

use crate::client::commands::targets::{self, Target};
use crate::client::error::{Error, Result};
use crate::client::protocol;
use crate::client::work::dir;
use crate::client::work::entries::Entry;

/// One file a `Notify` line is about.
pub struct Notice {
    /// The folder key the file lives in, as the protocol numbers it (empty for the
    /// invocation folder).
    pub dir: String,
    pub name: String,
    /// `E` (edit) or `U` (unedit).
    pub kind: char,
    /// The watch letters the edit asked for, `EUC` for a plain `edit` and empty for `unedit`.
    pub watches: String,
}

/// The files a command names, in the order the blocks send them. A folder that was selected
/// as a whole contributes every entry; a folder named file by file only those names. An entry
/// marked for removal has no working file left to edit, so it is skipped.
pub fn selected(targets: &BTreeMap<String, Target>) -> Vec<(&Target, &Entry)> {
    let mut out = Vec::new();
    for target in targets.values() {
        for entry in target.entries.files() {
            if entry.is_removed() {
                continue;
            }
            if !target.selected && !target.filter.iter().any(|name| name == &entry.name) {
                continue;
            }
            out.push((target, entry));
        }
    }
    out
}

/// Send the folder blocks and the `Notify` pairs, then `noop`. A folder is named once, before
/// the first file it holds.
pub fn send(
    session: &mut protocol::Session,
    targets: &BTreeMap<String, Target>,
    notices: &[Notice],
    cwd: &Path,
) -> Result<()> {
    let mut current: Option<&str> = None;
    for notice in notices {
        if current != Some(notice.dir.as_str()) {
            let target = targets.get(&notice.dir).ok_or_else(|| {
                Error::aborted(
                    &session.command,
                    format!("the folder '{}' is missing from the request", notice.dir),
                )
            })?;
            let absolute =
                dir::absolute_repository(session.root.protocol_root(), &target.dir.repository);
            session.directory(
                &targets::display_dir(&notice.dir),
                &absolute,
                &target.dir.local,
            )?;
            current = Some(notice.dir.as_str());
        }
        session.send(&format!("Notify {}", notice.name))?;
        session.send(&line(notice.kind, &notice.watches, cwd))?;
    }
    session.send("noop")?;
    session.flush()
}

/// The raw notification line, e.g. `E\tWed Nov 25 02:52:33 2026 GMT\tHOST\tC:\work\tEUC`.
pub fn line(kind: char, watches: &str, cwd: &Path) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        kind,
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
}
