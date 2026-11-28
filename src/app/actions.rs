//! Running cvs, polling its output, and the actions the menus call.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, TryRecvError};
use std::time::Instant;

use cvsdesk::cvs::{Cmd, CommandRequest, Msg};
use cvsdesk::cvs_client::work::codec;
use cvsdesk::cvs_client::work::entries::Entries;
use cvsdesk::entries::State;
use cvsdesk::history::History;

use crate::app::dialogs::checkout;
use crate::app::dialogs::history::HistoryState;
use crate::app::dialogs::tags::TagsState;
use crate::app::dialogs::Dialog;
use crate::app::{App, HistoryWait, LoginPurpose, OutputKind, TagWait};

impl App {
    // -- running commands ---------------------------------------------------

    pub(super) fn start(&mut self, req: CommandRequest, kind: OutputKind) -> bool {
        self.start_in(self.dir.clone(), req, kind)
    }

    /// Start one command in `dir` instead of the folder the window is showing. File
    /// History keeps the folder it was opened from, so browsing elsewhere must not
    /// change where its refresh or diff preview runs.
    pub(super) fn start_in(
        &mut self,
        dir: PathBuf,
        mut req: CommandRequest,
        kind: OutputKind,
    ) -> bool {
        if self.runner.running {
            log::debug!(
                target: "cvsdesk::command",
                "phase=refused reason=busy from=start"
            );
            self.runner
                .lines
                .push("Error : cvs is already running !".into());
            self.output = OutputKind::Console;
            return false;
        }
        self.cmd_seq += 1;
        let id = self.cmd_seq;
        self.stop_requested = false;
        self.history_waiting = None;
        self.checkout_waiting = None;
        self.pending_checkout = None;
        self.login_purpose = None;
        self.module_list_waiting = false;
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
        let refresh = req.refresh || req.cmd.changes_working_copy();
        let what = if req.raw_args.is_some() {
            "raw"
        } else {
            req.cmd.name()
        };
        log::info!(
            target: "cvsdesk::command",
            "id={} cmd={} phase=start files={} arg_count={} root={} raw={} refresh={}",
            id,
            what,
            req.files.len(),
            args.len(),
            if root.trim().is_empty() { "none" } else { "set" },
            req.raw_args.is_some(),
            refresh
        );
        self.run_started = Some(Instant::now());
        let started = self.runner.start(&dir, args, password, line, move |m| {
            let _ = tx.send(m);
        });
        if !started {
            // No worker was spawned, so there will never be a message on this channel.
            self.rx = None;
            self.run_started = None;
            log::warn!(
                target: "cvsdesk::command",
                "id={} phase=refused reason=empty-command",
                id
            );
        }
        self.running_kind = started.then_some(kind);
        self.refresh_after_command = started && refresh;
        started
    }

    /// The root one command runs against: the one the request named, else the folder's
    /// own `CVS/Root`. An empty string means cvs runs without `-d`.
    pub(super) fn root_for(&self, req: &CommandRequest) -> String {
        req.root.clone().unwrap_or_else(|| self.root.clone())
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
            self.status = "The selected rows are not checked out; check them out first".into();
            return;
        }
        if !self.selected.is_empty() && self.selected_local_names().is_empty() {
            self.status =
                "The selected rows are hidden by the current filter; clear it or select visible rows"
                    .into();
            return;
        }
        if self.selected.is_empty() && self.has_active_filter() {
            self.status = "Select files, or clear the filter to commit this folder".into();
            return;
        }
        self.open_dialog(Dialog::Commit {
            text,
            file: String::new(),
            force: false,
        });
    }

    /// Query > Compare with > Working Copy, and the file menu's Diff: the selection
    /// against the files on disk.
    pub(super) fn diff_working_copy(&mut self) {
        if !self.diff_ready() {
            return;
        }
        let mut req = CommandRequest::new(Cmd::Diff);
        req.files = self.selected_local_names();
        req.extra = self.prefs.diff_flags();
        self.start(req, OutputKind::Diff);
    }

    /// Query > Compare with > Revision or Tag. cvs reads a revision and a tag the same
    /// way in `-r`, so one entry covers "what changed since 1.4" and "what changed since
    /// the release".
    pub(super) fn diff_with(&mut self, revision: &str) {
        let revision = revision.trim();
        if revision.is_empty() {
            self.status = "Enter a revision or tag to compare with".into();
            return;
        }
        if !self.diff_ready() {
            return;
        }
        let req = compare_request(
            self.selected_local_names(),
            self.prefs.diff_flags(),
            revision,
        );
        self.start(req, OutputKind::Diff);
    }

    /// The checks both comparisons share: there must be something checked out, and the
    /// filter must not be hiding all of it.
    fn diff_ready(&mut self) -> bool {
        if self.selection_is_all_remote() {
            self.status = "The selected rows are not checked out; check them out first".into();
            return false;
        }
        if !self.selected.is_empty() && self.selected_local_names().is_empty() {
            self.status =
                "The selected rows are hidden by the current filter; clear it or select visible rows"
                    .into();
            return false;
        }
        if self.selected.is_empty() && self.has_active_filter() {
            self.status = "Select files, or clear the filter to diff this folder".into();
            return false;
        }
        true
    }

    pub(super) fn cvs_command(&mut self, cmd: Cmd) {
        if self.selection_is_all_remote() {
            self.status = "The selected rows are not checked out; check them out first".into();
            return;
        }
        if !self.selected.is_empty() && self.selected_local_names().is_empty() {
            self.status =
                "The selected rows are hidden by the current filter; clear it or select visible rows"
                    .into();
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

    /// Read one file's log and open or refresh its window when the reply arrives. The
    /// bottom pane keeps whatever the user was reading.
    pub(super) fn request_history(
        &mut self,
        dir: PathBuf,
        root: String,
        file: String,
        working: String,
        working_state: State,
        refresh: bool,
        open_diff: bool,
    ) {
        if root.trim().is_empty() {
            self.status = "This folder is not a CVS working copy".into();
            return;
        }
        if self.runner.running {
            log::debug!(
                target: "cvsdesk::command",
                "phase=refused reason=busy from=history"
            );
            self.runner
                .lines
                .push("Error : cvs is already running !".into());
            self.output = OutputKind::Console;
            return;
        }
        let mut req = CommandRequest::new(Cmd::Log);
        req.files = vec![file.clone()];
        req.root = Some(root.clone());
        let previous = self.output;
        if !self.start_in(dir.clone(), req, OutputKind::Log) {
            return;
        }
        // The reply belongs to the File History window, not to a pane.
        self.running_kind = None;
        self.output = previous;
        if refresh {
            if let Some(Dialog::History(state)) = self.dialog.as_mut() {
                state.set_error(String::new());
                state.set_loading(true);
            }
        }
        self.status = if open_diff {
            format!("Reading diff of {}", file)
        } else {
            format!("Reading history of {}", file)
        };
        self.history_waiting = Some(HistoryWait {
            dir,
            root,
            file,
            working,
            working_state,
            refresh,
            open_diff,
        });
    }

    /// The Query > File History entry: it needs exactly one selected file.
    pub(super) fn history_selected(&mut self) {
        if self.selected.len() != 1 {
            self.status = "Select one file to see its history".into();
            return;
        }
        let Some(&index) = self.selected.iter().next() else {
            return;
        };
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if row.is_dir {
            self.status = "File History needs a file, not a folder".into();
            return;
        }
        if row.origin == cvsdesk::entries::Origin::Remote {
            self.status = "The selected file is not checked out; check it out first".into();
            return;
        }
        self.request_history(
            self.dir.clone(),
            self.root.clone(),
            row.name.clone(),
            row.revision.clone(),
            row.state,
            false,
            false,
        );
    }

    /// The file menu's Diff: one local file, read its log, then open the Diff window
    /// with the working revision compared against the file on disk. The File History
    /// list stays closed, so the user lands on the comparison at once.
    pub(super) fn diff_selected_file(&mut self) {
        if self.selection_is_all_remote() {
            self.status = "The selected rows are not checked out; check them out first".into();
            return;
        }
        if self.selected.len() != 1 {
            self.status = "Diff compares one file at a time".into();
            return;
        }
        let Some(&index) = self.selected.iter().next() else {
            return;
        };
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if row.is_dir {
            self.status = "Diff needs a file, not a folder".into();
            return;
        }
        if row.origin == cvsdesk::entries::Origin::Remote {
            self.status = "The selected file is not checked out; check it out first".into();
            return;
        }
        if !self
            .selected_local_names()
            .iter()
            .any(|name| name == &row.name)
        {
            self.status =
                "The selected row is hidden by the current filter; clear it or select a visible row"
                    .into();
            return;
        }
        self.request_history(
            self.dir.clone(),
            self.root.clone(),
            row.name.clone(),
            row.revision.clone(),
            row.state,
            false,
            true,
        );
    }

    /// Open the Tags window for the selected items, or for the current folder when
    /// nothing is selected. The window appears at once and its list follows the reply.
    pub(super) fn open_tags(&mut self) {
        if self.runner.running {
            log::debug!(
                target: "cvsdesk::command",
                "phase=refused reason=busy from=tags-open"
            );
            self.status = "Stop cvs before managing tags".into();
            return;
        }
        if !self.has_cvs {
            self.status = "This folder is not a CVS working copy".into();
            return;
        }
        if self.selection_is_all_remote() {
            self.status = "The selected rows are not checked out; check them out first".into();
            return;
        }
        let scope = self.selected_local_names();
        if !self.selected.is_empty() && scope.is_empty() {
            self.status =
                "The selected rows are hidden by the current filter; clear it or select visible rows"
                    .into();
            return;
        }
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
            log::debug!(
                target: "cvsdesk::command",
                "phase=refused reason=busy from=tags-refresh"
            );
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
        self.running_kind = None;
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
        let mut vanished = false;
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(m) => msgs.push(m),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        vanished = true;
                        break;
                    }
                }
            }
        }
        // A worker that died without a `Done` must not leave the window saying "cvs
        // running" forever. Report it as a failed command and let the usual finish path
        // clear every window that was waiting on it.
        if vanished && !msgs.iter().any(|m| matches!(m, Msg::Done(_))) {
            log::error!(
                target: "cvsdesk::command",
                "id={} phase=worker result=vanished",
                self.cmd_seq
            );
            msgs.push(Msg::Line(
                "The cvs worker stopped without reporting a result.".into(),
            ));
            msgs.push(Msg::Done(1));
        }
        let mut finished = false;
        let mut exit_code: Option<i32> = None;
        let mut stopped = false;
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
                        stopped = true;
                        self.stop_requested = false;
                        self.runner.lines.push("Stopped.".into());
                    } else if code != 0 {
                        self.runner
                            .lines
                            .push(format!("cvs returned an non-zero error code ({})", code));
                    }
                    self.runner.finish(code);
                }
            }
        }
        if finished {
            let kind = self.running_kind.take();
            let output = std::mem::take(&mut self.pending);
            // A history or tag read runs for another window, not for the pane: its reply
            // must not replace what the reader was looking at.
            let background = self.history_waiting.is_some() || self.tag_waiting.is_some();
            self.captured = output.clone();
            if !background {
                if let Some(kind) = kind {
                    self.pane_output.insert(kind, output);
                }
            }
            self.rx = None;
            // Capture the finished command's own flags before a follow-up command (a login
            // that resumes a checkout, or a tag query) replaces them.
            let refresh = std::mem::take(&mut self.refresh_after_command);
            let elapsed = self
                .run_started
                .take()
                .map(|started| started.elapsed().as_millis())
                .unwrap_or(0);
            let login_purpose = self.login_purpose.take();
            let module_list = std::mem::take(&mut self.module_list_waiting);
            let checkout_dir = self.checkout_waiting.take();
            let mut moved = false;
            let mut checkout_failed = false;
            if let Some(purpose) = login_purpose {
                let ok = exit_code == Some(0) && !stopped;
                match purpose {
                    LoginPurpose::Checkout => {
                        if ok {
                            if let Some(pending) = self.pending_checkout.take() {
                                if let Some(pending) = checkout::start_pending(self, pending) {
                                    checkout::reopen(
                                        self,
                                        pending,
                                        "Could not start the checkout. See the Console."
                                            .to_string(),
                                    );
                                    checkout_failed = true;
                                    self.status = "Could not start the checkout".into();
                                }
                            }
                        } else {
                            let message = if stopped {
                                "Stopped.".to_string()
                            } else {
                                command_error(&self.captured)
                            };
                            let lower = message.to_ascii_lowercase();
                            let save_failure = lower.contains("home directory")
                                || lower.contains("cvspass")
                                || lower.contains("permission denied")
                                || lower.contains("access is denied");
                            if let Some(pending) = self.pending_checkout.take() {
                                let warning = if stopped {
                                    "Login stopped. The checkout has not run.".to_string()
                                } else if save_failure {
                                    format!(
                                        "The server accepted the password, but cvs could not save it in .cvspass. Check that your home folder is writable, then try again. ({message})"
                                    )
                                } else {
                                    format!("Login failed: {message}")
                                };
                                checkout::reopen(self, pending, warning);
                            }
                            checkout_failed = true;
                            self.status = if stopped {
                                "Login stopped".into()
                            } else {
                                "Login failed — see the Checkout window".into()
                            };
                        }
                    }
                    LoginPurpose::Modules { root } => {
                        if ok {
                            if !checkout::start_module_list(self, root) {
                                checkout::warn_dialog(
                                    self,
                                    "Could not start the module list. See the Console.",
                                );
                            }
                        } else {
                            let message = if stopped {
                                "Stopped.".to_string()
                            } else {
                                command_error(&self.captured)
                            };
                            let warning = if stopped {
                                "Login stopped. The module list has not run.".to_string()
                            } else {
                                format!("Login failed: {message}")
                            };
                            checkout::warn_dialog(self, warning);
                            self.status = if stopped {
                                "Login stopped".into()
                            } else {
                                "Login failed — see the Checkout window".into()
                            };
                        }
                    }
                }
            }
            // A checkout that worked has created a folder worth showing. Anything else
            // that just ended leaves the window where it is.
            if let Some(dir) = checkout_dir {
                let ok = exit_code == Some(0) && dir.is_dir();
                log::info!(
                    target: "cvsdesk::command",
                    "id={} checkout result={} code={}",
                    self.cmd_seq,
                    if ok { "ok" } else { "failed" },
                    exit_code.unwrap_or(-1)
                );
                if ok {
                    let last = self
                        .pending_checkout
                        .as_ref()
                        .map(|pending| pending.memory());
                    if let Some(last) = last {
                        self.last_checkout = Some(last);
                    }
                    self.pending_checkout = None;
                    self.go_to(dir);
                    self.status = format!("Checked out into {}", self.dir.display());
                    moved = true;
                } else {
                    checkout_failed = true;
                    let message = if stopped {
                        "Stopped.".to_string()
                    } else {
                        command_error(&self.captured)
                    };
                    if let Some(pending) = self.pending_checkout.take() {
                        let lower = message.to_ascii_lowercase();
                        let needs_password = lower.contains("no saved password")
                            || lower.contains("authorization failed")
                            || lower.contains("authentication failed");
                        let warning = if stopped {
                            "Checkout stopped. The folder may be partial; click Checkout to retry."
                                .to_string()
                        } else if needs_password {
                            format!(
                                "No saved password for this root. Enter the password above and click Checkout again. ({message})"
                            )
                        } else {
                            format!("Checkout failed: {message}")
                        };
                        checkout::reopen(self, pending, warning);
                        self.status = if stopped {
                            "Checkout stopped".into()
                        } else {
                            "Checkout failed — see the Checkout window".into()
                        };
                    }
                }
            }
            if module_list {
                if stopped {
                    checkout::warn_dialog(self, "Module list stopped.");
                    self.status = "Module list stopped".into();
                } else if exit_code == Some(0) {
                    checkout::warn_dialog(self, "");
                    self.status = "Module list is in the Console".into();
                } else {
                    let message = command_error(&self.captured);
                    checkout::warn_dialog(self, format!("Could not list modules: {message}"));
                    self.status = "Could not list modules — see the Console".into();
                }
            }
            if let Some(wait) = self.history_waiting.take() {
                let history = History::parse(&wait.file, &self.captured);
                let error = first_error(&self.captured);
                log::info!(
                    target: "cvsdesk::history",
                    "id={} revisions={} tags={} error={}",
                    self.cmd_seq,
                    history.revisions.len(),
                    history.tags.len(),
                    !error.is_empty()
                );
                if history.revisions.is_empty() {
                    let message = if error.is_empty() {
                        "No revisions returned".to_string()
                    } else {
                        error
                    };
                    if wait.refresh {
                        if let Some(Dialog::History(state)) = self.dialog.as_mut() {
                            if state.file() == wait.file {
                                state.set_error(message);
                            }
                        }
                    } else {
                        self.status = format!("No history for {}: {}", wait.file, message);
                    }
                } else if wait.refresh {
                    if let Some(Dialog::History(state)) = self.dialog.as_mut() {
                        if state.file() == wait.file {
                            state.replace_history(history, wait.working, wait.working_state);
                        }
                    }
                } else {
                    let mut state = HistoryState::new(
                        wait.dir,
                        wait.root,
                        wait.file,
                        wait.working,
                        wait.working_state,
                        history,
                    );
                    if wait.open_diff {
                        state.open_local_diff();
                        self.open_dialog(Dialog::Diff(Box::new(state)));
                    } else {
                        self.open_dialog(Dialog::History(Box::new(state)));
                    }
                }
            }
            // The create or delete that just ended still needs the file list read again;
            // `refresh` was captured before any tag query could replace the flag.
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
                        log::info!(
                            target: "cvsdesk::tags",
                            "id={} query tags={} code={} error={}",
                            self.cmd_seq,
                            tags.len(),
                            code,
                            !error.is_empty()
                        );
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
                            log::info!(
                                target: "cvsdesk::tags",
                                "id={} phase=write result=ok",
                                self.cmd_seq
                            );
                            if let Some(Dialog::Tags(state)) = self.dialog.as_mut() {
                                state.loading = false;
                                state.status = "Done. Refreshing tags…".to_string();
                            }
                            self.request_tags();
                        } else {
                            log::warn!(
                                target: "cvsdesk::tags",
                                "id={} phase=write result=failed code={}",
                                self.cmd_seq,
                                code
                            );
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
            if refresh && !moved && !checkout_failed && !module_list {
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
            log::info!(
                target: "cvsdesk::command",
                "id={} phase=end code={} elapsed_ms={} lines={} stopped={}",
                self.cmd_seq,
                exit_code.unwrap_or(-1),
                elapsed,
                self.captured.len(),
                stopped
            );
        }
    }

    pub(super) fn stop(&mut self) {
        // A stop the user asked for is not a failure: note it, and keep Msg::Done from
        // reporting the killed process's exit code as a non-zero error.
        let was_running = self.runner.running;
        log::info!(
            target: "cvsdesk::command",
            "id={} phase=stop-requested running={}",
            self.cmd_seq,
            was_running
        );
        let was_history = self.history_waiting.is_some();
        self.history_waiting = None;
        self.tag_waiting = None;
        if !was_running {
            // Nothing will report a result, so no follow-up can use these.
            self.checkout_waiting = None;
            self.pending_checkout = None;
            self.login_purpose = None;
            self.module_list_waiting = false;
        }
        if let Some(Dialog::Tags(state)) = self.dialog.as_mut() {
            state.loading = false;
            state.status = "Stopped.".into();
        }
        if was_history {
            if let Some(Dialog::History(state)) = self.dialog.as_mut() {
                state.set_error("Stopped.".into());
            }
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
            log::debug!(
                target: "cvsdesk::command",
                "phase=refused reason=busy from=open-folder"
            );
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
                .push(format!("Folder: {}", self.dir.display()));
        }
    }

    pub(super) fn explore(&mut self) {
        let dir = self.dir.display().to_string();
        match std::process::Command::new("explorer.exe").arg(&dir).spawn() {
            Ok(_) => self
                .runner
                .lines
                .push(format!("Opening '{}' in the file browser", dir)),
            Err(error) => {
                log::warn!(
                    target: "cvsdesk::ui",
                    "action=explore result=error {}",
                    cvsdesk::diag::io_error(&error)
                );
                self.status = "Could not open the folder in Explorer".into();
            }
        }
    }

    /// Show the folder the diagnostics log is written to. The menu item exists so a user
    /// asked for the log does not have to know where `%LOCALAPPDATA%` is.
    pub(super) fn open_log_folder(&mut self) {
        let dir = cvsdesk::diag::log_dir();
        let _ = std::fs::create_dir_all(&dir);
        match std::process::Command::new("explorer.exe").arg(&dir).spawn() {
            Ok(_) => log::info!(target: "cvsdesk::ui", "action=open-log-folder result=ok"),
            Err(error) => log::warn!(
                target: "cvsdesk::ui",
                "action=open-log-folder result=error {}",
                cvsdesk::diag::io_error(&error)
            ),
        }
    }

    /// Open the entry's folder in Explorer with the entry itself selected.
    pub(super) fn reveal_in_explorer(&mut self, name: &str) {
        let path = self.dir.join(name);
        match std::process::Command::new("explorer.exe")
            .arg("/select,")
            .arg(&path)
            .spawn()
        {
            Ok(_) => self
                .runner
                .lines
                .push(format!("Showing '{}' in the file browser", path.display())),
            Err(error) => {
                log::warn!(
                    target: "cvsdesk::ui",
                    "action=reveal result=error {}",
                    cvsdesk::diag::io_error(&error)
                );
                self.status = "Could not show the file in Explorer".into();
            }
        }
    }

    pub(super) fn edit_selected(&mut self) {
        if self.selection_is_all_remote() {
            self.status = "The selected file is not checked out; check it out first".into();
            return;
        }
        let names = self.selected_local_names();
        let viewer = self.prefs.viewer.clone();
        for n in names {
            let path = self.dir.join(&n);
            match std::process::Command::new(&viewer).arg(&path).spawn() {
                Ok(_) => self
                    .runner
                    .lines
                    .push(format!("Editing {}", path.display())),
                Err(error) => {
                    // One line for the action, not one per selected file, and no path
                    // or viewer name: the OS kind is the diagnosis.
                    log::warn!(
                        target: "cvsdesk::ui",
                        "action=edit result=error {}",
                        cvsdesk::diag::io_error(&error)
                    );
                    self.status = "Could not start the viewer".into();
                    break;
                }
            }
        }
    }

    pub(super) fn checked_out_edit(&mut self) {
        if self.selection_is_all_remote() {
            self.status = "The selected rows are not checked out; check them out first".into();
            return;
        }
        let files = self.selected_local_names();
        if !self.selected.is_empty() && files.is_empty() {
            self.status =
                "The selected rows are hidden by the current filter; clear it or select visible rows"
                    .into();
            return;
        }
        let mut req = CommandRequest::new(Cmd::Edit);
        req.files = files;
        req.extra = vec!["-c".into()];
        self.start(req, OutputKind::Console);
    }

    /// Open a folder that is not on disk yet by writing only its CVS control files, then
    /// let the normal folder navigation show it. Its files stay on the server and are
    /// listed as `Not checked out` until an update fetches them.
    pub(super) fn ensure_and_open_dir(&mut self, name: String) {
        if self.runner.running || self.loading.is_some() {
            log::debug!(
                target: "cvsdesk::command",
                "phase=refused reason=busy from=new-folder"
            );
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
                    "Created empty folder: {} (files are not checked out)",
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
            self.status = "The selected rows are not checked out; nothing local to delete".into();
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

/// The command a "Compare with…" runs: the same flags a plain diff uses, then the
/// revision or tag the user picked, then the selection. cvs reads the revision as a
/// `diff` switch, so it must come before the file names.
pub(super) fn compare_request(
    files: Vec<String>,
    flags: Vec<String>,
    revision: &str,
) -> CommandRequest {
    let mut req = CommandRequest::new(Cmd::Diff);
    req.files = files;
    req.extra = flags;
    req.extra.push("-r".to_string());
    req.extra.push(revision.to_string());
    req
}

/// The first line of a failed reply, for the small error line inside a dialog.
fn first_error(lines: &[String]) -> String {
    lines
        .iter()
        .find(|line| !line.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "cvs failed".to_string())
}

/// The line a person should read when a command fails. A login greets the reader before
/// it fails, and a checkout prints progress first, so the first non-empty line is often
/// not the error.
fn command_error(lines: &[String]) -> String {
    lines
        .iter()
        .find(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("aborted")
                || lower.contains("failed")
                || lower.contains("refused")
                || lower.contains("cannot")
                || lower.contains("could not")
                || lower.contains("no saved password")
                || lower.contains("not installed")
                || lower.contains("permission denied")
                || lower.contains("access is denied")
        })
        .or_else(|| lines.iter().rev().find(|line| !line.trim().is_empty()))
        .cloned()
        .unwrap_or_else(|| "cvs failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_error_is_not_the_greeting() {
        let lines = vec![
            "Logging in to :pserver:ann@host:/repo".to_string(),
            "cvs [login aborted]: authorization failed".to_string(),
        ];
        assert_eq!(
            command_error(&lines),
            "cvs [login aborted]: authorization failed"
        );
    }

    #[test]
    fn a_checkout_progress_line_is_not_the_error() {
        let lines = vec![
            "cvs server: Updating error_module".to_string(),
            "cvs [checkout aborted]: cannot find module".to_string(),
        ];
        assert_eq!(
            command_error(&lines),
            "cvs [checkout aborted]: cannot find module"
        );
    }

    #[test]
    fn without_a_keyword_the_last_line_is_the_best_guess() {
        let lines = vec!["one".to_string(), "the last".to_string()];
        assert_eq!(command_error(&lines), "the last");
    }
}
