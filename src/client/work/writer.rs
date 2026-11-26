//! Writing what the server sends into a working copy.
//!
//! `checkout` and `update` receive the same shapes — `Created`, `Updated`, `Merged`, `Copy-file`,
//! `Removed`, sticky tags — so the rules live here once:
//!
//! * a folder gets `CVS/Root`, `CVS/Repository` and its `CVS/Entries`;
//! * a text file is written with the platform's line endings; `-kb`/`-ku` files byte for byte;
//! * the entry's date is the file's own time in UTC, or `Result of merge+…` after a conflict;
//! * a child folder is added to its parent's `Entries` as `D/name////`.
//!
//! `dry_run` is the global `-n` ("do not execute anything that will change the disk"): the
//! server's report is still read, but nothing on disk moves.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::client::error::{Error, Result};
use crate::client::protocol::FileKind;
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::dir::{self, WorkingDir};
use crate::client::work::entries::{Entries, Entry};

struct DirState {
    working: WorkingDir,
    entries: Entries,
}

pub struct WorkWriter {
    cwd: PathBuf,
    root: Root,
    destination: Option<String>,
    modules: Vec<String>,
    no_cvs: bool,
    readonly: bool,
    dry_run: bool,
    dirs: BTreeMap<String, DirState>,
}

impl WorkWriter {
    pub fn new(cwd: &Path, root: Root) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            root,
            destination: None,
            modules: Vec::new(),
            no_cvs: false,
            readonly: false,
            dry_run: false,
            dirs: BTreeMap::new(),
        }
    }

    pub fn with_destination(mut self, destination: Option<String>) -> Self {
        self.destination = destination;
        self
    }

    pub fn with_modules(mut self, modules: &[String]) -> Self {
        self.modules = modules.to_vec();
        self
    }

    pub fn no_cvs(mut self, no_cvs: bool) -> Self {
        self.no_cvs = no_cvs;
        self
    }

    pub fn readonly(mut self, readonly: bool) -> Self {
        self.readonly = readonly;
        self
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// The local folder for a server update directory, honouring `-d`.
    pub fn local_for(&self, update_dir: &str) -> PathBuf {
        let mut parts = parts_of(update_dir);
        if let Some(destination) = &self.destination {
            if let Some(first) = parts.first() {
                if self.modules.iter().any(|module| module == first) {
                    parts[0] = destination;
                }
            }
        }
        let mut path = self.cwd.clone();
        for part in parts {
            path.push(part);
        }
        path
    }

    /// Make sure a server-named folder and all its parents exist, and record each child in
    /// its parent's `Entries` as a `D/…` line.
    pub fn ensure_dir(&mut self, update_dir: &str, repository: &str) -> Result<()> {
        let parts = parts_of(update_dir);
        let relative = dir::relative_repository(self.root.protocol_root(), repository);
        if parts.is_empty() {
            // `.` is the folder the command runs in. Its state is keyed by the empty
            // string, the same key the file responses use; a non-empty key here made a
            // top-level update and commit look the folder up as `.` and panic.
            let local = self.local_for(update_dir);
            let relative_here = dir::read_repository(&local).unwrap_or(relative);
            self.ensure_state(String::new(), relative_here)?;
            return Ok(());
        }
        let rel_parts: Vec<&str> = relative
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        for depth in 0..parts.len() {
            let key = parts[..=depth].join("/");
            let relative_here = if rel_parts.len() >= depth + 1 {
                rel_parts[..=depth].join("/")
            } else {
                relative.clone()
            };
            self.ensure_state(key.clone(), relative_here)?;
            if depth > 0 {
                let parent = parts[..depth].join("/");
                if let Some(state) = self.dirs.get_mut(&parent) {
                    state.entries.set_directory(parts[depth]);
                }
            }
        }
        Ok(())
    }

    /// Record one folder of the working copy, reading its entries the first time it is
    /// named. `key` is the folder's update directory, empty for the command's own folder.
    fn ensure_state(&mut self, key: String, relative: String) -> Result<()> {
        if self.dirs.contains_key(&key) {
            return Ok(());
        }
        let local = self.local_for(&key);
        let codec = codec::for_dir(&local);
        let working = WorkingDir::new(local, key.clone(), relative, codec);
        let entries = if !self.dry_run && working.cvs_dir().join("Entries").exists() {
            Entries::read(&working.local)?
        } else {
            Entries::default()
        };
        if self.dry_run {
            // Nothing is created with -n.
        } else if self.no_cvs {
            std::fs::create_dir_all(&working.local)?;
        } else {
            working.ensure(&self.root)?;
        }
        self.dirs.insert(key, DirState { working, entries });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn write_file(
        &mut self,
        kind: FileKind,
        update_dir: &str,
        repository: &str,
        entry_line: &str,
        mode: &str,
        content: &[u8],
        mtime: Option<std::time::SystemTime>,
    ) -> Result<()> {
        let entry = Entry::parse(entry_line).ok_or_else(|| {
            Error::aborted(
                "cvs",
                format!(
                    "the server sent an entry this client cannot read: {}",
                    entry_line
                ),
            )
        })?;
        self.ensure_dir(update_dir, repository)?;
        if self.dry_run {
            return Ok(());
        }
        let key = dir_key(update_dir);
        let state = self.dirs.get_mut(&key).unwrap();
        let local = state.working.local.join(&entry.name);

        let readonly = self.readonly || dir::mode_is_readonly(mode);
        let text = !entry.is_binary() && !entry.is_unicode();
        dir::write_file(&local, content, mode, mtime, text)?;
        if readonly {
            dir::set_readonly(&local, true)?;
        }

        let time = mtime
            .or_else(|| codec::file_mtime(&local))
            .unwrap_or_else(std::time::SystemTime::now);
        let mut new_entry = entry.clone();
        new_entry.date = merge_date(&entry.date, time, kind);
        state.entries.set_file(new_entry);
        Ok(())
    }

    /// `Patched`: the body is a context diff to apply to the working file.
    #[allow(clippy::too_many_arguments)]
    pub fn write_patch(
        &mut self,
        update_dir: &str,
        repository: &str,
        entry_line: &str,
        mode: &str,
        patch: &[u8],
        mtime: Option<std::time::SystemTime>,
    ) -> Result<()> {
        let entry = Entry::parse(entry_line).ok_or_else(|| {
            Error::aborted(
                "cvs",
                format!(
                    "the server sent an entry this client cannot read: {}",
                    entry_line
                ),
            )
        })?;
        self.ensure_dir(update_dir, repository)?;
        if self.dry_run {
            return Ok(());
        }
        let key = dir_key(update_dir);
        let local = self.dirs.get(&key).unwrap().working.local.join(&entry.name);
        let original = std::fs::read(&local)?;
        let normalized = if entry.is_binary() || entry.is_unicode() {
            original
        } else {
            crate::client::work::codec::to_repository_newlines(&original)
        };
        let patched = crate::client::work::patch::apply_context_diff(
            &normalized,
            &String::from_utf8_lossy(patch),
        )?;
        self.write_file(
            FileKind::Updated,
            update_dir,
            repository,
            entry_line,
            mode,
            &patched,
            mtime,
        )
    }

    /// `Checked-in`: the server changed the entry (revision or sticky tag) without sending
    /// the file. Used by `update -A` and by a successful commit.
    pub fn record_entry(
        &mut self,
        update_dir: &str,
        repository: &str,
        entry_line: &str,
    ) -> Result<()> {
        let entry = Entry::parse(entry_line).ok_or_else(|| {
            Error::aborted(
                "cvs",
                format!(
                    "the server sent an entry this client cannot read: {}",
                    entry_line
                ),
            )
        })?;
        self.ensure_dir(update_dir, repository)?;
        if self.dry_run {
            return Ok(());
        }
        let key = dir_key(update_dir);
        let state = self.dirs.get_mut(&key).unwrap();
        let local = state.working.local.join(&entry.name);
        let mut new_entry = entry.clone();
        if new_entry.date.is_empty() {
            let time = codec::file_mtime(&local).unwrap_or_else(std::time::SystemTime::now);
            new_entry.date = codec::asctime(time);
        }
        state.entries.set_file(new_entry);
        Ok(())
    }

    /// `Copy-file <dir>/` + repository + backup name: save the working file before a merge
    /// overwrites it.
    pub fn copy_file(&mut self, update_dir: &str, repository: &str, name: &str) -> Result<()> {
        self.ensure_dir(update_dir, repository)?;
        if self.dry_run {
            return Ok(());
        }
        let key = dir_key(update_dir);
        let state = self.dirs.get(&key).unwrap();
        let source = state.working.local.join(base_name(repository));
        let target = state.working.local.join(name);
        if source.exists() {
            std::fs::copy(&source, &target)?;
        }
        Ok(())
    }

    pub fn remove_file(&mut self, update_dir: &str, name: &str) -> Result<()> {
        self.ensure_dir(update_dir, "")?;
        if self.dry_run {
            return Ok(());
        }
        let key = dir_key(update_dir);
        if let Some(state) = self.dirs.get(&key) {
            let local = state.working.local.join(name);
            match std::fs::remove_file(&local) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// `Remove-entry <name>`: drop the file's line from `CVS/Entries`.
    pub fn remove_entry(&mut self, update_dir: &str, name: &str) -> Result<()> {
        self.ensure_dir(update_dir, "")?;
        if self.dry_run {
            return Ok(());
        }
        let key = dir_key(update_dir);
        if let Some(state) = self.dirs.get_mut(&key) {
            state.entries.remove_file(name);
        }
        Ok(())
    }

    /// Save the repository's commit-message template as `CVS/Template`. The next `commit`
    /// without `-m` puts it at the top of the editor's text. The response names only the
    /// folder, so the folder's own `Root`/`Repository` files are left to the file responses
    /// that accompany it.
    pub fn write_template(&mut self, update_dir: &str, content: &[u8]) -> Result<()> {
        if self.dry_run || self.no_cvs {
            return Ok(());
        }
        let cvs = self.local_for(update_dir).join("CVS");
        std::fs::create_dir_all(&cvs)?;
        std::fs::write(cvs.join("Template"), content)?;
        Ok(())
    }

    pub fn set_static_directory(&mut self, update_dir: &str, repository: &str) -> Result<()> {
        self.ensure_dir(update_dir, repository)?;
        if self.dry_run || self.no_cvs {
            return Ok(());
        }
        let local = self.local_for(update_dir);
        std::fs::create_dir_all(local.join("CVS"))?;
        let _ = std::fs::write(local.join("CVS").join("Entries.Static"), b"");
        Ok(())
    }

    pub fn clear_static_directory(&mut self, update_dir: &str, repository: &str) -> Result<()> {
        self.ensure_dir(update_dir, repository)?;
        if self.dry_run || self.no_cvs {
            return Ok(());
        }
        let _ = std::fs::remove_file(
            self.local_for(update_dir)
                .join("CVS")
                .join("Entries.Static"),
        );
        Ok(())
    }

    pub fn set_sticky(&mut self, update_dir: &str, repository: &str, tag: &str) -> Result<()> {
        self.ensure_dir(update_dir, repository)?;
        if self.dry_run || self.no_cvs {
            return Ok(());
        }
        std::fs::write(
            self.local_for(update_dir).join("CVS").join("Tag"),
            format!("{}{}", tag, codec::newline()),
        )?;
        Ok(())
    }

    pub fn clear_sticky(&mut self, update_dir: &str, repository: &str) -> Result<()> {
        self.ensure_dir(update_dir, repository)?;
        if self.dry_run || self.no_cvs {
            return Ok(());
        }
        let _ = std::fs::remove_file(self.local_for(update_dir).join("CVS").join("Tag"));
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        if self.no_cvs || self.dry_run {
            return Ok(());
        }
        for state in self.dirs.values() {
            state.working.write_entries(&state.entries)?;
        }
        Ok(())
    }
}

/// What CVS writes in the entry's date field after a merge.
///
/// The server's `+=` is "conflict, with the time to be filled in"; `+` is a clean merge.
/// Both end up as `Result of merge` plus the file's own `asctime`, which is what the
/// reference client writes and what an update reader looks for. An ordinary update writes
/// just the file's time.
fn merge_date(server_date: &str, time: std::time::SystemTime, _kind: FileKind) -> String {
    let stamp = codec::asctime(time);
    // `+=` is the protocol's "conflict, time to be filled in". A lone `+` is a clean merge
    // and takes the ordinary date, the same as an update without a merge.
    if server_date.starts_with("+=") {
        format!("Result of merge+{}", stamp)
    } else {
        stamp
    }
}

fn base_name(repository: &str) -> &str {
    repository
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(repository)
}

/// The key a folder's state is stored under. `.` and `./` are the command's own folder,
/// which is the empty key.
fn dir_key(update_dir: &str) -> String {
    parts_of(update_dir).join("/")
}

/// `mod3/d/` → `["mod3", "d"]`.
fn parts_of(update_dir: &str) -> Vec<&str> {
    update_dir
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-writer-{}-{}-{}",
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
    fn a_created_file_lands_in_its_folder_with_an_entry() {
        let base = scratch("create");
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        let mut writer = WorkWriter::new(&base, root);
        writer
            .write_file(
                FileKind::Created,
                "mod/",
                "/repo/mod/a.txt",
                "/a.txt/1.1///",
                "u=rw,g=rw,o=rw",
                b"hello\n",
                None,
            )
            .unwrap();
        writer.finish().unwrap();
        assert!(base.join("mod").join("a.txt").is_file());
        let entries =
            std::fs::read_to_string(base.join("mod").join("CVS").join("Entries")).unwrap();
        assert!(entries.contains("/a.txt/1.1/"), "{}", entries);
        assert!(base.join("mod").join("CVS").join("Repository").is_file());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let base = scratch("dry");
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        let mut writer = WorkWriter::new(&base, root).dry_run(true);
        writer
            .write_file(
                FileKind::Updated,
                "mod/",
                "/repo/mod/a.txt",
                "/a.txt/1.2///",
                "u=rw,g=rw,o=rw",
                b"hello\n",
                None,
            )
            .unwrap();
        writer.finish().unwrap();
        assert!(!base.join("mod").exists(), "-n must not create anything");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_top_level_update_reaches_the_folder_the_command_runs_in() {
        let base = scratch("top-update");
        std::fs::create_dir_all(base.join("CVS")).unwrap();
        std::fs::write(base.join("CVS").join("Repository"), "mod\n").unwrap();
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        let mut writer = WorkWriter::new(&base, root);
        writer
            .write_file(
                FileKind::Updated,
                ".",
                "/repo/mod/a.txt",
                "/a.txt/1.2/Tue Nov 24 09:52:53 2026//",
                "u=rw,g=rw,o=rw",
                b"new\n",
                None,
            )
            .unwrap();
        writer.finish().unwrap();
        let text = std::fs::read_to_string(base.join("a.txt"))
            .unwrap()
            .replace("\r\n", "\n");
        assert_eq!(text, "new\n");
        let entries = std::fs::read_to_string(base.join("CVS").join("Entries")).unwrap();
        assert!(entries.contains("/a.txt/1.2/"), "{}", entries);
        let repository = std::fs::read_to_string(base.join("CVS").join("Repository")).unwrap();
        assert_eq!(repository.trim(), "mod");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_top_level_checked_in_records_the_entry() {
        let base = scratch("top-checked-in");
        std::fs::create_dir_all(base.join("CVS")).unwrap();
        std::fs::write(base.join("CVS").join("Repository"), "mod\n").unwrap();
        std::fs::write(
            base.join("CVS").join("Entries"),
            "/a.txt/1.1/Tue Nov 24 09:52:53 2026//\nD\n",
        )
        .unwrap();
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        let mut writer = WorkWriter::new(&base, root);
        writer
            .record_entry(
                ".",
                "/repo/mod/a.txt",
                "/a.txt/1.2/Tue Nov 24 09:52:53 2026//",
            )
            .unwrap();
        writer.finish().unwrap();
        let entries = std::fs::read_to_string(base.join("CVS").join("Entries")).unwrap();
        assert!(entries.contains("/a.txt/1.2/"), "{}", entries);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_conflict_date_becomes_result_of_merge() {
        let stamp = codec::asctime(std::time::SystemTime::now());
        let date = merge_date("+=", std::time::SystemTime::now(), FileKind::Merged);
        assert_eq!(date, format!("Result of merge+{}", stamp));
        let date = merge_date("", std::time::SystemTime::now(), FileKind::Updated);
        assert_eq!(date, stamp);
        let date = merge_date("+", std::time::SystemTime::now(), FileKind::Merged);
        assert_eq!(date, stamp, "a clean merge keeps the ordinary date");
    }
}

#[cfg(test)]
mod template_tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-writer-{}-{}-{}",
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
    fn a_template_is_written_into_the_folders_cvs_folder() {
        let base = scratch("template");
        let root = Root::parse(":pserver:ann@host:/repo").unwrap();
        let mut writer = WorkWriter::new(&base, root);
        writer.write_template("mod/", b"bug id:\n").unwrap();
        writer.finish().unwrap();
        assert_eq!(
            std::fs::read(base.join("mod").join("CVS").join("Template")).unwrap(),
            b"bug id:\n"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
