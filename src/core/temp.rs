//! The one temporary folder the program owns.
//!
//! A full-file diff has to put each revision on disk before it can read the exact
//! bytes back: a line-based network sink loses a final newline and cannot carry a
//! binary file. Every such checkout goes under `%TEMP%/cvsdesk`, never directly
//! beside other programs' files, so one startup sweep can clean everything this
//! program left behind.
//!
//! A [`Session`] is the unit of cleanup: it creates a private folder, hands it to
//! the version loader, and removes the whole folder when dropped. A crash leaves
//! the folder behind, and [`cleanup_stale`] removes it on the next start.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a crashed session may stay before the next start removes it.
pub const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// A saved commit message is useful longer than a checkout scratch folder.
const MESSAGE_STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The application's own folder inside the system temporary directory.
pub fn root() -> PathBuf {
    std::env::temp_dir().join("cvsdesk")
}

/// A folder for one-shot files such as the message an editor writes. It is created
/// under the same root so the startup sweep can remove what a crash left behind.
pub fn messages_dir() -> PathBuf {
    let dir = root().join("messages");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// One private folder under the application temp root.
pub struct Session {
    dir: PathBuf,
}

impl Session {
    /// Create a fresh session under the standard application root.
    pub fn new() -> io::Result<Self> {
        Self::in_root(&root())
    }

    /// Create a fresh session under `root`. Tests use this to keep their scratch
    /// folders away from the real application root.
    pub fn in_root(root: &Path) -> io::Result<Self> {
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = sessions.join(format!("session-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&dir)?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A named child path under the session, without creating it. Names are reduced
    /// to safe characters, so a revision string can never escape the session folder.
    pub fn path(&self, name: &str) -> PathBuf {
        let safe: String = name
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        let safe = if safe.is_empty() {
            "item".to_string()
        } else {
            safe
        };
        self.dir.join(safe)
    }

    /// A named child folder, created if needed.
    pub fn child(&self, name: &str) -> io::Result<PathBuf> {
        let path = self.path(name);
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Remove session folders older than `max_age`. The current session is simply a
/// fresh folder and is far too young to match; callers do not need to exclude it.
pub fn cleanup_stale_at(root: &Path, max_age: Duration, now: SystemTime) -> io::Result<usize> {
    let sessions = root.join("sessions");
    let entries = match std::fs::read_dir(&sessions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // The folder name carries the creation stamp. A directory's own modified time
        // is not portable in tests (and changes when its children change), so prefer
        // the stamp and fall back to the filesystem for older folders.
        let created = session_timestamp(&path)
            .or_else(|| {
                entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
            })
            .unwrap_or(now);
        let age = now.duration_since(created).unwrap_or_default();
        if age >= max_age && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Remove stale sessions from the application root, and old one-shot message files.
pub fn cleanup_stale() -> io::Result<usize> {
    let root = root();
    let now = SystemTime::now();
    let mut removed = cleanup_stale_at(&root, STALE_AFTER, now)?;
    removed += cleanup_old_files(&root.join("messages"), MESSAGE_STALE_AFTER, now)?;
    Ok(removed)
}

fn cleanup_old_files(dir: &Path, max_age: Duration, now: SystemTime) -> io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(now);
        if now.duration_since(modified).unwrap_or_default() >= max_age
            && std::fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

/// The creation time encoded in a session folder name.
fn session_timestamp(path: &Path) -> Option<SystemTime> {
    let name = path.file_name()?.to_str()?;
    let nanos: u64 = name.rsplit('-').next()?.parse().ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "cvsdesk-temp-{}-{name}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn a_session_lives_under_the_app_root_and_disappears_when_dropped() {
        let root = scratch("session");
        let path = {
            let session = Session::in_root(&root).unwrap();
            let path = session.dir().to_path_buf();
            assert!(path.starts_with(root.join("sessions")));
            assert!(path.is_dir());
            let child = session.child("rev 1.2/x").unwrap();
            assert!(child.starts_with(&path));
            assert!(child.is_dir());
            path
        };
        assert!(!path.exists(), "dropping the session removes its folder");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_child_name_cannot_escape_the_session() {
        let root = scratch("escape");
        let session = Session::in_root(&root).unwrap();
        let child = session.child("../outside").unwrap();
        assert!(child.starts_with(session.dir()));
        assert!(!child.to_string_lossy().contains(".."));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_removes_only_the_old_sessions() {
        let root = scratch("cleanup");
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let now = SystemTime::now();
        let stamp = |age: Duration| {
            now.duration_since(UNIX_EPOCH)
                .unwrap()
                .saturating_sub(age)
                .as_nanos()
        };
        let old = sessions.join(format!(
            "session-1-{}",
            stamp(Duration::from_secs(48 * 60 * 60))
        ));
        let fresh = sessions.join(format!("session-1-{}", stamp(Duration::from_secs(60))));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        let removed = cleanup_stale_at(&root, STALE_AFTER, now).unwrap();
        assert_eq!(removed, 1);
        assert!(!old.exists());
        assert!(fresh.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn old_message_files_are_removed_but_fresh_ones_stay() {
        let root = scratch("messages");
        let dir = root.join("messages");
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("cvsmsg-old.txt");
        let fresh = dir.join("cvsmsg-fresh.txt");
        std::fs::File::create(&old)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60))
            .unwrap();
        std::fs::File::create(&fresh).unwrap();
        let now = SystemTime::now();
        let removed = cleanup_old_files(&dir, MESSAGE_STALE_AFTER, now).unwrap();
        assert_eq!(removed, 1);
        assert!(!old.exists());
        assert!(fresh.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_does_nothing_when_the_sessions_folder_is_missing() {
        let root = scratch("missing");
        let removed = cleanup_stale_at(&root, STALE_AFTER, SystemTime::now()).unwrap();
        assert_eq!(removed, 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
