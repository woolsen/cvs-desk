//! Running cvs, polling its output, and the actions the menus call.

use std::collections::HashSet;
use std::sync::mpsc::channel;

use cvsdesk::cvs::{Cmd, CommandRequest, Msg};
use cvsdesk::cvs_client::work::codec;
use cvsdesk::cvs_client::work::entries::Entries;
use cvsdesk::entries::{self, State};
use cvsdesk::graph::revision_nodes;

use crate::app::dialogs::tags::TagsState;
use crate::app::dialogs::Dialog;
use crate::app::{App, OutputKind, TagWait, Trouble};

impl App {
    // -- running commands ---------------------------------------------------

    pub(super) fn start(&mut self, mut req: CommandRequest, kind: OutputKind) -> bool {
        if self.runner.running {
            self.runner
                .lines
                .push("Error : cvs is already running !".into());
            self.output = OutputKind::Console;
            self.trouble = Some(Trouble::Busy);
            return false;
        }
        self.trouble = None;
        self.stop_requested = false;
        self.graph_waiting = None;
        self.checkout_waiting = None;
        req.prune = self.prefs.prune;
        let password = req.password.clone();
        let root = self.root_for(&req);
        let globals = self.prefs.globals();
        let args = req.to_args(&root, &globals);
        let line = req.to_line("cvs", &root, &globals);
        self.pending.clear();
        self.captured.clear();
        self.output = kind;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let dir = self.dir.clone();
        let refresh = req.refresh || req.cmd.changes_working_copy();
        let started = self.runner.start(&dir, args, password, line, move |m| {
            let _ = tx.send(m);
        });
        self.refresh_after_command = started && refresh;
        started
    }

    /// The root one command runs against: the one the request named, else the folder's
    /// own `CVS/Root`. An empty string means cvs runs without `-d`.
    pub(super) fn root_for(&self, req: &CommandRequest) -> String {
        req.root.clone().unwrap_or_else(|| self.root.clone())
    }

    /// The connection kind named by the folder's own root, for the About window and the
    /// right side of the menu bar. `None` means the folder is not a working copy.
    pub(super) fn folder_kind(&self) -> Option<&'static str> {
        if self.root.trim().is_empty() {
            None
        } else {
            Some(entries::parse_cvsroot(&self.root).0.label())
        }
    }

    /// The root Checkout, Import and Login open with: the working copy's root when there
    /// is one, otherwise the root used last, otherwise the old single-root setting.
    pub(super) fn default_connection(&self) -> String {
        if !self.root.trim().is_empty() {
            return self.root.clone();
        }
        if let Some(root) = self.prefs.recent_roots.first() {
            return root.clone();
        }
        self.prefs.cvsroot.trim().to_string()
    }

    pub(super) fn open_commit_dialog(&mut self, text: String) {
        if self.selection_is_all_remote() {
            self.status = "The selected rows are only on the server; check them out first".into();
            return;
        }
        self.open_dialog(Dialog::Commit {
            text,
            file: String::new(),
            force: false,
        });
    }

    pub(super) fn open_diff_dialog(&mut self) {
        if self.selection_is_all_remote() {
            self.status = "The selected rows are only on the server; check them out first".into();
            return;
        }
        self.open_dialog(Dialog::Diff {
            unified: self.prefs.unified,
            ignore_case: self.prefs.ignore_case,
            no_whitespace: self.prefs.no_whitespace,
            no_blank_lines: self.prefs.no_blank_lines,
            extra: self.prefs.diff_options.clone(),
        });
    }

    pub(super) fn cvs_command(&mut self, cmd: Cmd) {
        if self.selection_is_all_remote() {
            self.status = "The selected rows are only on the server; check them out first".into();
            return;
        }
        let files = self.selected_local_names();
        let mut req = CommandRequest::new(cmd.clone());
        req.files = files;
        if cmd == Cmd::QueryUpdate {
            req.query = true;
        }
        if cmd == Cmd::Checkout {
            req.readonly = true;
        }
        let kind = match cmd {
            Cmd::Log => OutputKind::Log,
            Cmd::Diff | Cmd::Rdiff => OutputKind::Diff,
            Cmd::Status | Cmd::WatchOn | Cmd::WatchOff => OutputKind::Status,
            Cmd::Annotate => OutputKind::Annotate,
            Cmd::Editors | Cmd::Watchers => OutputKind::Editors,
            Cmd::History => OutputKind::History,
            _ => OutputKind::Console,
        };
        self.start(req, kind);
    }

    /// Update this folder and everything below it, whatever is selected. cvs updates
    /// recursively when the command line names no files, so the menu item exists for
    /// the common case of wanting the whole folder without clearing the selection.
    pub(super) fn update_whole_folder(&mut self) {
        self.start(CommandRequest::new(Cmd::Update), OutputKind::Console);
    }

    /// Read one file's log and open the graph window when the reply arrives.
    pub(super) fn request_graph(&mut self, file: String) {
        if self.runner.running {
            self.runner
                .lines
                .push("Error : cvs is already running !".into());
            self.output = OutputKind::Console;
            self.trouble = Some(Trouble::Busy);
            return;
        }
        let mut req = CommandRequest::new(Cmd::Log);
        req.files = vec![file.clone()];
        self.start(req, OutputKind::Log);
        self.graph_waiting = Some(file);
    }

    /// The Query > Graph entry: it needs exactly one selected file.
    pub(super) fn graph_selected(&mut self) {
        if self.selected.len() != 1 {
            self.status = "Select one file to see its graph".into();
            return;
        }
        let Some(&index) = self.selected.iter().next() else {
            return;
        };
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if row.is_dir {
            self.status = "Graph needs a file, not a folder".into();
            return;
        }
        if row.origin == cvsdesk::entries::Origin::Remote {
            self.status = "The selected file is only on the server; check it out first".into();
            return;
        }
        self.request_graph(row.name.clone());
    }

    /// Open the Tags window for the selected items, or for the current folder when
    /// nothing is selected. The window appears at once and its list follows the reply.
    pub(super) fn open_tags(&mut self) {
        if self.runner.running {
            self.status = "Stop cvs before managing tags".into();
            return;
        }
        if !self.has_cvs {
            self.status = "This folder is not a CVS working copy".into();
            return;
        }
        if self.selection_is_all_remote() {
            self.status = "The selected rows are only on the server; check them out first".into();
            return;
        }
        let scope = self.selected_local_names();
        let label = if scope.is_empty() {
            "This folder and its subfolders".to_string()
        } else {
            format!("{} selected item(s)", scope.len())
        };
        self.dialog = Some(Dialog::Tags(Box::new(TagsState::loading(scope, label))));
        self.request_tags();
    }

    /// Ask for the tag list. `poll` parses the captured reply into the open window.
    pub(super) fn request_tags(&mut self) {
        if self.runner.running {
            if let Some(Dialog::Tags(state)) = self.dialog.as_mut() {
                state.loading = false;
                state.error = "Another cvs command is running".into();
            }
            return;
        }
        let scope = match self.dialog.as_ref() {
            Some(Dialog::Tags(state)) => state.scope.clone(),
            _ => return,
        };
        let mut req = CommandRequest::new(Cmd::Log);
        req.files = scope;
        req.extra = vec!["-h".into()];
        self.tag_waiting = Some(TagWait::Query);
        let previous = self.output;
        self.start(req, OutputKind::Log);
        // Reading tags is a background detail; leave the pane the user was reading on screen.
        self.output = previous;
        if let Some(Dialog::Tags(state)) = self.dialog.as_mut() {
            state.loading = true;
            state.error.clear();
        }
    }

    pub(super) fn poll(&mut self) {
        self.poll_load();
        self.poll_remote();
        let mut msgs = Vec::new();
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                msgs.push(m);
            }
        }
        let mut finished = false;
        let mut exit_code: Option<i32> = None;
        for m in msgs {
            match m {
                Msg::Line(l) => {
                    self.runner.lines.push(l.clone());
                    self.pending.push(l);
                }
                Msg::Done(code) => {
                    finished = true;
                    exit_code = Some(code);
                    // A command this program stopped on request is stopped, not failed.
                    if self.stop_requested {
                        self.stop_requested = false;
                        self.runner.lines.push("Stopped.".into());
                        self.trouble = None;
                    } else if code != 0 {
                        self.runner
                            .lines
                            .push(format!("cvs returned an non-zero error code ({})", code));
                        self.trouble = Some(Trouble::Exit(code));
                    }
                    self.runner.finish(code);
                }
            }
        }
        if finished {
            self.captured = std::mem::take(&mut self.pending);
            self.rx = None;
            // A checkout that worked has created a folder worth showing. Anything else
            // that just ended leaves the window where it is.
            let mut moved = false;
            if let Some(dir) = self.checkout_waiting.take() {
                if exit_code == Some(0) && dir.is_dir() {
                    self.go_to(dir);
                    self.status = format!("Checked out into {}", self.dir.display());
                    moved = true;
                }
            }
            if let Some(file) = self.graph_waiting.take() {
                let nodes = revision_nodes(&self.captured);
                if nodes.is_empty() {
                    self.status = format!("No revisions to graph for {}", file);
                } else {
                    self.open_dialog(Dialog::Graph { file, nodes });
                }
            }
            // Capture the flag before a tag refresh query overwrites it: the create or
            // delete that just ended still needs the file list read again.
            let refresh = std::mem::take(&mut self.refresh_after_command);
            if let Some(wait) = self.tag_waiting.take() {
                let code = exit_code.unwrap_or(-1);
                match wait {
                    TagWait::Query => {
                        let tags = cvsdesk::tags::parse_log(&self.captured);
                        let error = if code == 0 {
                            String::new()
                        } else {
                            first_error(&self.captured)
                        };
                        if let Some(Dialog::Tags(state)) = self.dialog.as_mut() {
                            state.tags = tags;
                            state.loading = false;
                            state.error = error;
                            if let Some(selected) = &state.selected {
                                if !state.tags.iter().any(|tag| &tag.name == selected) {
                                    state.selected = None;
                                    state.confirm = None;
                                }
                            }
                        }
                    }
                    TagWait::AfterWrite => {
                        if code == 0 {
                            if let Some(Dialog::Tags(state)) = self.dialog.as_mut() {
                                state.loading = false;
                                state.status = "Done. Refreshing tags…".to_string();
                            }
                            self.request_tags();
                        } else {
                            // Nothing changed, so there is nothing to read again. Keep
                            // cvs's first error line in the window instead of clearing it
                            // with a refresh query.
                            let error = first_error(&self.captured);
                            if let Some(Dialog::Tags(state)) = self.dialog.as_mut() {
                                state.loading = false;
                                state.error = error;
                                state.status = "The command failed".to_string();
                            }
                        }
                    }
                }
            }
            // Only a command that writes the working copy re-reads it, and then in place:
            // a query leaves the list exactly as it was, and a write keeps its selection,
            // scroll position and rows until the new ones arrive.
            if refresh && !moved {
                self.refresh();
            }
            let bad = !self.captured.is_empty()
                && self
                    .captured
                    .iter()
                    .any(|l| l.contains("No such file") || l.contains("not installed"));
            if bad {
                // CVS is probably not installed; say what the window is showing instead.
                self.output = OutputKind::State;
                self.refresh_state_sheet();
            }
        }
    }

    pub(super) fn stop(&mut self) {
        // A stop the user asked for is not a failure: note it, and keep Msg::Done from
        // reporting the killed process's exit code as a non-zero error.
        let was_running = self.runner.running;
        self.graph_waiting = None;
        self.tag_waiting = None;
        self.checkout_waiting = None;
        if let Some(Dialog::Tags(state)) = self.dialog.as_mut() {
            state.loading = false;
            state.status = "Stopped.".into();
        }
        self.runner.stop();
        self.stop_requested = was_running;
        self.runner.lines.push("Stop requested.".into());
        if was_running {
            self.status = "CVS stopped".into();
        } else {
            self.status = "No command was running".into();
        }
    }

    pub(super) fn open_dialog(&mut self, d: Dialog) {
        // A dialog owns the keyboard now; do not leave a half-typed path behind it.
        self.address_editing = false;
        self.address = self.dir.display().to_string();
        self.dialog = Some(d);
    }

    // -- local actions ------------------------------------------------------

    pub(super) fn open_folder(&mut self, frame: &eframe::Frame) {
        if self.runner.running {
            self.status = "Stop cvs before opening another folder".into();
            return;
        }
        self.address_editing = false;
        self.address = self.dir.display().to_string();
        let picked = rfd::FileDialog::new()
            .set_title("Open folder")
            .set_directory(&self.dir)
            .set_parent(frame)
            .pick_folder();
        if let Some(dir) = picked {
            self.go_to(dir);
            self.runner
                .lines
                .push(format!("Working folder: {}", self.dir.display()));
        }
    }

    pub(super) fn explore(&mut self) {
        let dir = self.dir.display().to_string();
        let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
        self.runner
            .lines
            .push(format!("Opening '{}' in the file browser", dir));
    }

    /// Open the entry's folder in Explorer with the entry itself selected.
    pub(super) fn reveal_in_explorer(&mut self, name: &str) {
        let path = self.dir.join(name);
        let _ = std::process::Command::new("explorer.exe")
            .arg("/select,")
            .arg(&path)
            .spawn();
        self.runner
            .lines
            .push(format!("Showing '{}' in the file browser", path.display()));
    }

    pub(super) fn edit_selected(&mut self) {
        if self.selection_is_all_remote() {
            self.status = "The selected file is only on the server; check it out first".into();
            return;
        }
        let names = self.selected_local_names();
        let viewer = self.prefs.viewer.clone();
        for n in names {
            let path = self.dir.join(&n);
            let _ = std::process::Command::new(&viewer).arg(&path).spawn();
            self.runner
                .lines
                .push(format!("Editing {}", path.display()));
        }
    }

    pub(super) fn checked_out_edit(&mut self) {
        if self.selection_is_all_remote() {
            self.status = "The selected rows are only on the server; check them out first".into();
            return;
        }
        let files = self.selected_local_names();
        let mut req = CommandRequest::new(Cmd::Edit);
        req.files = files;
        req.extra = vec!["-c".into()];
        self.start(req, OutputKind::Console);
    }

    /// Open a folder that is not on disk yet by writing only its CVS control files, then
    /// let the normal folder navigation show it. Its files stay on the server and are
    /// listed as `On server` until an update fetches them.
    pub(super) fn ensure_and_open_dir(&mut self, name: String) {
        if self.runner.running || self.loading.is_some() {
            self.status = "Wait for the running command or folder read to finish".into();
            return;
        }
        if !self.has_cvs || self.repository.trim().is_empty() {
            self.status = "This folder is not a CVS working copy".into();
            return;
        }
        let path = self.dir.join(&name);
        if path.is_dir() {
            self.go_to(path);
            return;
        }
        match cvsdesk::cvs_client::work::skeleton::ensure_child(&self.dir, &name) {
            Ok(_) => {
                self.runner.lines.push(format!(
                    "Created empty folder: {} (files stay on server)",
                    name
                ));
                self.status = format!("Created empty folder: {}", name);
                self.go_to(path);
            }
            Err(error) => {
                self.status = format!("Could not create folder: {}", error);
            }
        }
    }

    /// Move the selected entries to the trash. The folder itself is a local delete; a
    /// folder row is a `D/` record in CVS/Entries, and cvs has no command that takes one
    /// out, so the record goes with it.
    pub(super) fn move_to_trash(&mut self) {
        if self.selection_is_all_remote() {
            self.status =
                "The selected rows are only on the server; nothing local to delete".into();
            return;
        }
        let names = self.selected_local_names();
        if names.is_empty() {
            self.runner
                .lines
                .push("Select the files to move to the trash first.".into());
            return;
        }

        let mut index = Entries::read(&self.dir).ok();
        let mut drop_records: Vec<String> = Vec::new();
        let mut records_to_report: Vec<String> = Vec::new();
        let mut deleted: HashSet<String> = HashSet::new();
        for name in &names {
            let is_folder_record = index
                .as_ref()
                .is_some_and(|entries| entries.directories().any(|recorded| recorded == name));
            // A folder entry may record a full path, in which case `join` keeps it as it
            // is; a plain name is resolved against the folder on screen.
            let path = self.dir.join(name);
            if !path.exists() {
                if is_folder_record {
                    // The row is the record, and the record is there; saying "Not found"
                    // would describe the folder, not the thing being deleted.
                    drop_records.push(name.clone());
                    records_to_report.push(name.clone());
                    deleted.insert(name.clone());
                } else {
                    self.runner
                        .lines
                        .push(format!("Not found: {}", path.display()));
                }
                continue;
            }
            match crate::app::trash::move_to_trash(&path) {
                Ok(()) => {
                    self.runner
                        .lines
                        .push(format!("Moved to the trash: {}", path.display()));
                    deleted.insert(name.clone());
                    if is_folder_record {
                        drop_records.push(name.clone());
                    }
                }
                Err(error) => {
                    self.runner.lines.push(format!(
                        "Cannot move to the trash: {} ({})",
                        path.display(),
                        error
                    ));
                }
            }
        }

        if !drop_records.is_empty() {
            if let Some(entries) = index.as_mut() {
                for name in &drop_records {
                    entries.remove_directory(name);
                }
                match entries.write(&self.dir, codec::for_dir(&self.dir)) {
                    Ok(()) => {
                        for name in &records_to_report {
                            self.runner
                                .lines
                                .push(format!("Removed the folder entry: {}", name));
                        }
                    }
                    Err(error) => {
                        self.runner
                            .lines
                            .push(format!("Cannot write CVS/Entries: {}", error));
                        // The records are still there, so their rows must stay too.
                        for name in &drop_records {
                            deleted.remove(name);
                        }
                    }
                }
            }
        }

        self.apply_deleted(&deleted);
    }

    /// Take the deleted rows out of the list in place. Re-reading the whole folder would
    /// lose the selection and scroll position, and this folder can hold thousands of
    /// files; a versioned file stays as Missing, everything else goes away.
    fn apply_deleted(&mut self, deleted: &HashSet<String>) {
        if deleted.is_empty() {
            return;
        }
        let old = std::mem::take(&mut self.rows);
        let mut rows = Vec::with_capacity(old.len());
        let mut new_index: Vec<Option<usize>> = Vec::with_capacity(old.len());
        for mut row in old {
            if deleted.contains(&row.name) {
                if row.is_dir || row.state == State::Unknown {
                    new_index.push(None);
                    continue;
                }
                // Only the states that come from the file's own timestamp change when
                // the file leaves. An added, removed or conflicted entry keeps its state
                // after a reload too, so it must keep it here.
                if matches!(row.state, State::Unchanged | State::Modified) {
                    row.state = State::Missing;
                }
            }
            new_index.push(Some(rows.len()));
            rows.push(row);
        }
        self.rows = rows;
        self.selected = self
            .selected
            .iter()
            .filter_map(|index| new_index.get(*index).copied().flatten())
            .collect();
        self.anchor = self
            .anchor
            .and_then(|index| new_index.get(index).copied().flatten());
        self.order.clear();
        self.order_dirty = true;
        self.counts = self.count_states();
        self.state_dirty = true;
        self.status = format!(
            "{} file(s) in this folder",
            self.rows.iter().filter(|row| !row.is_dir).count()
        );
    }
}

/// The first line of a failed reply, for the small error line inside a dialog.
fn first_error(lines: &[String]) -> String {
    lines
        .iter()
        .find(|line| !line.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "cvs failed".to_string())
}
