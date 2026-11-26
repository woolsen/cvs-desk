//! Working folders: where `CVS/` lives, what `CVS/Repository` says, and how a path the
//! server sends becomes a path on disk.
//!
//! The server names a file by its directory (`mod/d/`) and its entry line (`/one.txt/…`).
//! The local path is the invocation folder, then the directory, then the entry's name.
//! `CVS/Repository` is the repository path relative to the root, which is what CVS writes
//! into every folder it creates.

use std::path::{Path, PathBuf};

use crate::client::error::Result;
use crate::client::root::Root;
use crate::client::work::codec::{self, Codec};
use crate::client::work::entries::{Entries, Entry};

pub struct WorkingDir {
    pub local: PathBuf,
    /// Relative to the invocation folder, with `/` separators; empty for `.`.
    pub update_dir: String,
    /// Relative to the root's repository path, e.g. `mod3/d`.
    pub repository: String,
    pub codec: Codec,
}

impl WorkingDir {
    pub fn new(local: PathBuf, update_dir: String, repository: String, codec: Codec) -> Self {
        Self {
            local,
            update_dir,
            repository,
            codec,
        }
    }

    pub fn cvs_dir(&self) -> PathBuf {
        self.local.join("CVS")
    }

    pub fn entries(&self) -> Result<Entries> {
        Entries::read(&self.local)
    }

    pub fn write_entries(&self, entries: &Entries) -> Result<()> {
        entries.write(&self.local, self.codec)
    }

    /// Create the folder and, when it has no CVS folder yet, its Root and Repository.
    pub fn ensure(&self, root: &Root) -> Result<()> {
        std::fs::create_dir_all(&self.local)?;
        let cvs = self.cvs_dir();
        if !cvs.is_dir() {
            std::fs::create_dir_all(&cvs)?;
        }
        let root_file = cvs.join("Root");
        if !root_file.exists() {
            std::fs::write(&root_file, encode_line(&root.key, self.codec)?)?;
        }
        let repository_file = cvs.join("Repository");
        if !repository_file.exists() {
            std::fs::write(&repository_file, encode_line(&self.repository, self.codec)?)?;
        }
        Ok(())
    }

    /// The folder's entries, creating an empty index when the folder is new.
    pub fn ensure_entries(&self) -> Result<Entries> {
        let path = self.cvs_dir().join("Entries");
        if path.exists() {
            self.entries()
        } else {
            Ok(Entries::default())
        }
    }
}

fn encode_line(text: &str, encoding: Codec) -> Result<Vec<u8>> {
    let mut bytes = encoding.encode(text)?;
    bytes.extend_from_slice(codec::newline().as_bytes());
    Ok(bytes)
}

/// `C:\repo` + `C:/repo/mod3/d` → `mod3/d`, the way `CVS/Repository` records it.
pub fn relative_repository(root_path: &str, absolute: &str) -> String {
    let root = normalize(root_path);
    let root = root.trim_end_matches('/');
    let repo = normalize(absolute);
    if let Some(rest) = strip_prefix(&repo, root) {
        return rest.trim_start_matches('/').to_string();
    }
    repo.trim_start_matches('/').to_string()
}

/// The repository path to put in a `Directory` request: the root plus the relative path.
pub fn absolute_repository(root_path: &str, relative: &str) -> String {
    let root = root_path.trim_end_matches(['/', '\\']);
    let relative = relative.trim_start_matches(['/', '\\']);
    if relative.is_empty() {
        root.to_string()
    } else {
        format!("{}/{}", normalize(root), normalize(relative))
    }
}

fn normalize(text: &str) -> String {
    text.replace('\\', "/")
}

fn strip_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if let Some(rest) = text.strip_prefix(prefix) {
        return Some(rest);
    }
    #[cfg(windows)]
    {
        let lower_text = text.to_ascii_lowercase();
        let lower_prefix = prefix.to_ascii_lowercase();
        if lower_text.starts_with(&lower_prefix) {
            return Some(&text[prefix.len()..]);
        }
    }
    None
}

/// The local path for a name in an update directory.
pub fn local_path(base: &Path, update_dir: &str, name: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in update_dir.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        path.push(part);
    }
    path.push(name);
    path
}

/// How cvs prints a file: `d/one.txt`, or `one.txt` at the top.
pub fn display_path(update_dir: &str, name: &str) -> String {
    let dir = update_dir.trim_matches('/');
    if dir.is_empty() || dir == "." {
        name.to_string()
    } else {
        format!("{}/{}", dir, name)
    }
}

/// The update directory for a folder below the invocation folder (empty for itself).
pub fn update_dir_of(base: &Path, folder: &Path) -> String {
    match folder.strip_prefix(base) {
        Ok(rest) if !rest.as_os_str().is_empty() => normalize(&rest.to_string_lossy()),
        _ => String::new(),
    }
}

/// The marker in a folder's `CVS/Tag`, e.g. `TBRANCH`, `N1.2` or `D2026.11.24.10.00.00`.
///
/// That file is what a sticky tag leaves behind, and its first line — letter included — is
/// exactly what the `Sticky` request carries. An empty file means no tag.
pub fn sticky_tag(folder: &Path) -> Option<String> {
    let bytes = std::fs::read(folder.join("CVS").join("Tag")).ok()?;
    let text = codec::detect(&bytes).decode(&bytes);
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// Does the folder hold `CVS/Entries.Static`? Then it was only partly checked out and the
/// server must not add files to it; the `Static-directory` request says so.
pub fn is_static(folder: &Path) -> bool {
    folder.join("CVS").join("Entries.Static").exists()
}

/// Read a folder's `CVS/Repository`, if it has one.
pub fn read_repository(dir: &Path) -> Option<String> {
    let bytes = std::fs::read(dir.join("CVS").join("Repository")).ok()?;
    let text = codec::detect(&bytes).decode(&bytes);
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// Walk a working folder, parents before children, in `Entries` order.
///
/// Only folders recorded as `D/…` in `Entries` are entered: that is exactly what CVS does,
/// and it keeps deleted folders from reappearing during an update.
pub fn walk(base: &Path, recursive: bool) -> Result<Vec<WorkingDir>> {
    let mut out = Vec::new();
    let mut stack = vec![(base.to_path_buf(), String::new())];
    while let Some((folder, update_dir)) = stack.pop() {
        if !folder.join("CVS").is_dir() {
            continue;
        }
        let Some(repository) = read_repository(&folder) else {
            continue;
        };
        let codec = codec::for_dir(&folder);
        let entries = Entries::read(&folder)?;
        let mut children = Vec::new();
        if recursive {
            for name in entries.directories() {
                let child = folder.join(name);
                if child.is_dir() {
                    let child_update = if update_dir.is_empty() {
                        name.to_string()
                    } else {
                        format!("{}/{}", update_dir, name)
                    };
                    children.push((child, child_update));
                }
            }
        }
        out.push(WorkingDir::new(folder, update_dir, repository, codec));
        // Push in reverse so the first child is visited first.
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    Ok(out)
}

/// Write a file the server sent, with its mode, time and line endings.
///
/// `text` is true for entries without `-kb`/`-ku`: on Windows those files are written with
/// CRLF, exactly as the reference CVSNT client writes them.
pub fn write_file(
    path: &Path,
    content: &[u8],
    mode: &str,
    mtime: Option<std::time::SystemTime>,
    text: bool,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        clear_readonly(path)?;
    }
    let bytes = if text {
        codec::to_platform_newlines(content)
    } else {
        content.to_vec()
    };
    std::fs::write(path, bytes)?;
    if let Some(time) = mtime {
        codec::set_file_mtime(path, time)?;
    }
    if mode_is_readonly(mode) {
        set_readonly(path, true)?;
    }
    Ok(())
}

/// `u=r,g=r,o=r` is read-only; `u=rw,…` is not. Windows has one read-only bit.
pub fn mode_is_readonly(mode: &str) -> bool {
    for part in mode.split(',') {
        let Some(value) = part.trim().strip_prefix("u=") else {
            continue;
        };
        return !value.contains('w');
    }
    false
}

pub fn set_readonly(path: &Path, readonly: bool) -> Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(readonly);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

pub fn clear_readonly(path: &Path) -> Result<()> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if metadata.permissions().readonly() {
        set_readonly(path, false)?;
    }
    Ok(())
}

/// Write the entry for a file just written, with the file's own time.
pub fn record_file(
    dir: &WorkingDir,
    entries: &mut Entries,
    name: &str,
    revision: &str,
    options: &str,
    tag: &str,
    mtime: Option<std::time::SystemTime>,
) -> Result<Entry> {
    let local = dir.local.join(name);
    let time = mtime
        .or_else(|| codec::file_mtime(&local))
        .unwrap_or_else(std::time::SystemTime::now);
    let entry = Entry::new(name, revision, &codec::asctime(time), options, tag);
    entries.set_file(entry.clone());
    Ok(entry)
}

impl std::fmt::Debug for WorkingDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkingDir")
            .field("local", &self.local)
            .field("update_dir", &self.update_dir)
            .field("repository", &self.repository)
            .field("codec", &self.codec)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_below_the_root_loses_only_the_root() {
        assert_eq!(
            relative_repository("/srv/cvs/project", "/srv/cvs/project/docs"),
            "docs"
        );
        assert_eq!(
            relative_repository(r"C:\repo3", "C:/repo3/mod3/d"),
            "mod3/d"
        );
        assert_eq!(relative_repository("/repo", "/repo"), "");
    }

    #[test]
    fn the_absolute_path_is_built_with_a_forward_slash() {
        assert_eq!(absolute_repository("/home/cvs", "mod/d"), "/home/cvs/mod/d");
        assert_eq!(absolute_repository("/home/cvs/", "/mod"), "/home/cvs/mod");
    }

    #[test]
    fn a_name_is_placed_below_the_update_directory() {
        let base = Path::new("C:\\work");
        assert_eq!(local_path(base, "", "a.txt"), Path::new("C:\\work\\a.txt"));
        assert_eq!(
            local_path(base, "d/e", "a.txt"),
            Path::new("C:\\work\\d\\e\\a.txt")
        );
        assert_eq!(display_path("", "a.txt"), "a.txt");
        assert_eq!(display_path(".", "a.txt"), "a.txt");
        assert_eq!(display_path("d/", "a.txt"), "d/a.txt");
    }

    #[test]
    fn a_read_only_mode_is_the_one_without_w() {
        assert!(mode_is_readonly("u=r,g=r,o=r"));
        assert!(!mode_is_readonly("u=rw,g=rw,o=rw"));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-dir-{}-{}-{}",
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
    fn ensuring_a_folder_writes_gbk_root_and_repository() {
        let base = scratch("ensure");
        let dir = WorkingDir::new(
            base.join("知识库"),
            "知识库".to_string(),
            "docs/知识库".to_string(),
            Codec::Gbk,
        );
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        dir.ensure(&root).unwrap();
        let root_bytes = std::fs::read(dir.cvs_dir().join("Root")).unwrap();
        let root_text = std::str::from_utf8(&root_bytes).unwrap();
        assert_eq!(root_text.trim_end(), ":pserver:ann@host:/repo");
        assert!(root_text.ends_with(codec::newline()));
        let repo_bytes = std::fs::read(dir.cvs_dir().join("Repository")).unwrap();
        assert!(std::str::from_utf8(&repo_bytes).is_err(), "should be GBK");
        assert_eq!(Codec::Gbk.decode(&repo_bytes).trim_end(), "docs/知识库");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn walking_follows_entries_and_stops_at_removed_folders() {
        let base = scratch("walk");
        let top = base.join("mod");
        std::fs::create_dir_all(top.join("CVS")).unwrap();
        std::fs::write(top.join("CVS").join("Repository"), "mod\n").unwrap();
        let sub = top.join("d");
        std::fs::create_dir_all(sub.join("CVS")).unwrap();
        std::fs::write(sub.join("CVS").join("Repository"), "mod/d\n").unwrap();
        let gone = top.join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        std::fs::write(top.join("CVS").join("Entries"), "D/d////\nD/gone////\n").unwrap();
        let dirs = walk(&top, true).unwrap();
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[0].update_dir, "");
        assert_eq!(dirs[1].update_dir, "d");
        assert_eq!(dirs[1].repository, "mod/d");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn walking_enters_a_folder_left_in_the_pending_log() {
        // An interrupted update can leave the child's `D` line in `CVS/Entries.Log` only.
        // cvs itself still walks it, so an update or commit from the parent must too.
        let base = scratch("walk-log");
        let top = base.join("mod");
        std::fs::create_dir_all(top.join("CVS")).unwrap();
        std::fs::write(top.join("CVS").join("Repository"), "mod\n").unwrap();
        let sub = top.join("late");
        std::fs::create_dir_all(sub.join("CVS")).unwrap();
        std::fs::write(sub.join("CVS").join("Repository"), "mod/late\n").unwrap();
        std::fs::write(top.join("CVS").join("Entries"), "").unwrap();
        std::fs::write(top.join("CVS").join("Entries.Log"), "A D/late////\r\n").unwrap();

        let dirs = walk(&top, true).unwrap();
        assert_eq!(
            dirs.len(),
            2,
            "{:?}",
            dirs.iter().map(|d| &d.update_dir).collect::<Vec<_>>()
        );
        assert_eq!(dirs[1].update_dir, "late");
        assert_eq!(dirs[1].repository, "mod/late");

        std::fs::remove_dir_all(&base).ok();
    }
}
