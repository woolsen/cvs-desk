//! Moving between folders: the address bar, the up/back/forward history, and the
//! small pure helpers that decide what a typed path means.

use std::path::{Component, Path, PathBuf};

use crate::app::dialogs::Dialog;
use crate::app::files::display_name;
use crate::app::{App, OutputKind};

/// How many folders the history keeps. A working session cannot exhaust this; the
/// list would otherwise grow with every step for as long as the window is open.
const HISTORY_LIMIT: usize = 100;

/// The folders visited, with a cursor. Back and forward move the cursor; going to a
/// new folder after moving back drops the forward trail, as a browser does.
#[derive(Clone, Debug)]
pub(super) struct History {
    dirs: Vec<PathBuf>,
    at: usize,
}

impl History {
    pub(super) fn new(dir: PathBuf) -> Self {
        Self {
            dirs: vec![dir],
            at: 0,
        }
    }

    pub(super) fn push(&mut self, dir: PathBuf) {
        if self.dirs.get(self.at) == Some(&dir) {
            return;
        }
        self.dirs.truncate(self.at + 1);
        self.dirs.push(dir);
        self.at = self.dirs.len() - 1;
        if self.dirs.len() > HISTORY_LIMIT {
            let drop = self.dirs.len() - HISTORY_LIMIT;
            self.dirs.drain(..drop);
            self.at -= drop;
        }
    }

    pub(super) fn can_back(&self) -> bool {
        self.at > 0
    }

    pub(super) fn can_forward(&self) -> bool {
        self.at + 1 < self.dirs.len()
    }

    pub(super) fn back(&mut self) -> Option<PathBuf> {
        if !self.can_back() {
            return None;
        }
        self.at -= 1;
        Some(self.dirs[self.at].clone())
    }

    pub(super) fn forward(&mut self) -> Option<PathBuf> {
        if !self.can_forward() {
            return None;
        }
        self.at += 1;
        Some(self.dirs[self.at].clone())
    }
}

impl App {
    /// Go to `dir` and remember it. The folder must already be known to exist; the
    /// callers check, so a typo in the address bar never lands in the history.
    pub(super) fn go_to(&mut self, dir: PathBuf) {
        if dir == self.dir {
            self.reload();
            return;
        }
        self.history.push(dir.clone());
        self.enter(dir);
    }

    pub(super) fn go_back(&mut self) {
        if let Some(dir) = self.history.back() {
            if dir.is_dir() {
                self.enter(dir);
            } else {
                log::warn!(
                    target: "cvsdesk::folder",
                    "phase=open result=missing source=back"
                );
                self.status = format!("That folder is gone: {}", dir.display());
            }
        }
    }

    pub(super) fn go_forward(&mut self) {
        if let Some(dir) = self.history.forward() {
            if dir.is_dir() {
                self.enter(dir);
            } else {
                log::warn!(
                    target: "cvsdesk::folder",
                    "phase=open result=missing source=forward"
                );
                self.status = format!("That folder is gone: {}", dir.display());
            }
        }
    }

    /// The button is called "Up" rather than "Parent" because that is what the
    /// folder pane used to call it; the behaviour is the same.
    pub(super) fn go_up(&mut self) {
        let Some(parent) = self
            .dir
            .parent()
            .filter(|p| p.is_dir())
            .map(Path::to_path_buf)
        else {
            self.status = "This is the top folder".into();
            return;
        };
        self.go_to(parent);
    }

    pub(super) fn can_go_up(&self) -> bool {
        self.dir.parent().is_some_and(|p| p.is_dir())
    }

    /// Read the address bar: a folder path, absolute or relative to the current one.
    /// A path that is not a folder leaves the list where it was and says why. Returns
    /// true when the bar may be closed, false so the user can correct a typo.
    pub(super) fn submit_address(&mut self) -> bool {
        match folder_for(&self.dir, &self.address) {
            Some(dir) => {
                self.address = dir.display().to_string();
                self.address_editing = false;
                self.go_to(dir);
                true
            }
            None => {
                let typed = self.address.trim().trim_matches('"').trim();
                log::debug!(
                    target: "cvsdesk::folder",
                    "phase=open result=missing source=address empty={}",
                    typed.is_empty()
                );
                self.status = if typed.is_empty() {
                    "Type a folder path in the address bar".into()
                } else {
                    format!("No such folder: {}", typed)
                };
                false
            }
        }
    }

    /// Opens the one selected folder, as a double click on its row does. A folder that
    /// is only a server name or a missing `D` record gets its empty control-file
    /// skeleton first.
    pub(super) fn open_selected_folder(&mut self) {
        if self.selected.len() != 1 {
            return;
        }
        let Some(&index) = self.selected.iter().next() else {
            return;
        };
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if !row.is_dir {
            return;
        }
        let name = row.name.clone();
        let path = self.dir.join(&name);
        if path.is_dir() {
            self.go_to(path);
        } else {
            self.ensure_and_open_dir(display_name(&name, true).to_string());
        }
    }

    /// Put a folder on screen without touching the history. Back, forward and the
    /// address bar all end here; the address text always names what is shown.
    fn enter(&mut self, dir: PathBuf) {
        // The Tags window acts on the scope of the folder it was opened in. Leaving that
        // folder must close it, or its buttons would reach into the new one.
        if matches!(&self.dialog, Some(Dialog::Tags(_))) {
            self.dialog = None;
            self.tag_waiting = None;
        }
        self.dir = dir;
        // Kept in memory so Save in Preferences carries the folder on screen; the
        // last-folder line is written to the settings file when the window closes.
        self.prefs.last_dir = self.dir.display().to_string();
        self.address = self.dir.display().to_string();
        self.address_editing = false;
        // A search belongs to the folder it was typed in; a new folder would otherwise
        // look empty whenever its names do not match the old text. Reload view keeps it.
        self.search.clear();
        self.search_open = false;
        self.focus_search = false;
        self.reload();
        self.output = OutputKind::Console;
    }
}

/// What the address bar means: a folder path, absolute or relative to `base`.
/// Quotation marks are stripped because Explorer's "Copy as path" adds them.
fn folder_for(base: &Path, text: &str) -> Option<PathBuf> {
    let text = text.trim().trim_matches('"').trim();
    if text.is_empty() {
        return None;
    }
    let path = PathBuf::from(text);
    let path = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };
    let path = normalize(&path);
    path.is_dir().then_some(path)
}

/// Removes `.` and resolves `..` without touching the disk, so a typed
/// `D:\a\b\..\c` is shown as `D:\a\c` instead of keeping the `..` around.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history() -> History {
        History::new(PathBuf::from("a"))
    }

    #[test]
    fn history_walks_back_and_forth() {
        let mut h = history();
        h.push(PathBuf::from("b"));
        h.push(PathBuf::from("c"));
        assert!(h.can_back());
        assert!(!h.can_forward());
        assert_eq!(h.back(), Some(PathBuf::from("b")));
        assert_eq!(h.back(), Some(PathBuf::from("a")));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some(PathBuf::from("b")));
        assert_eq!(h.forward(), Some(PathBuf::from("c")));
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn a_new_folder_after_back_drops_the_forward_trail() {
        let mut h = history();
        h.push(PathBuf::from("b"));
        h.push(PathBuf::from("c"));
        h.back();
        h.push(PathBuf::from("d"));
        assert!(!h.can_forward());
        assert_eq!(h.back(), Some(PathBuf::from("b")));
        assert_eq!(h.forward(), Some(PathBuf::from("d")));
    }

    #[test]
    fn going_to_the_same_folder_is_not_a_history_step() {
        let mut h = history();
        h.push(PathBuf::from("a"));
        assert!(!h.can_back());
        assert!(!h.can_forward());
    }

    #[test]
    fn history_does_not_grow_without_bound() {
        let mut h = history();
        for i in 0..HISTORY_LIMIT + 20 {
            h.push(PathBuf::from(format!("folder-{i}")));
        }
        assert!(h.can_back());
        let mut steps = 1;
        while h.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, HISTORY_LIMIT);
    }

    #[test]
    fn a_typed_relative_folder_lands_under_the_current_one() {
        let root = std::env::temp_dir().join(format!("cvsdesk-nav-rel-{}", std::process::id()));
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        assert_eq!(folder_for(&root, "sub"), Some(sub.clone()));
        assert_eq!(folder_for(&root, "."), Some(root.clone()));
        assert_eq!(folder_for(&root, "sub/.."), Some(root.clone()));
        assert_eq!(folder_for(&root, "not-there"), None);
        assert_eq!(folder_for(&root, "  "), None);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_quoted_absolute_folder_is_understood() {
        let root = std::env::temp_dir().join(format!("cvsdesk-nav-quote-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let quoted = format!("\"{}\"", root.display());
        assert_eq!(folder_for(&root, &quoted), Some(root.clone()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn normalize_removes_dot_and_parent_steps() {
        assert_eq!(normalize(Path::new("a/b/../c/./d")), PathBuf::from("a/c/d"));
    }

    #[cfg(windows)]
    #[test]
    fn normalize_keeps_a_drive_and_its_root() {
        assert_eq!(
            normalize(Path::new(r"D:\work\src\..\bin")),
            PathBuf::from(r"D:\work\bin")
        );
        assert_eq!(normalize(Path::new(r"D:\..")), PathBuf::from(r"D:\"));
    }
}
