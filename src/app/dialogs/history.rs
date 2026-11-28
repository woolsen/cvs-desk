//! The File History window: one file's revisions, tags and diffs.
//!
//! The left side is the revision list with its branch gutter, the right side is the
//! selected revision. The diff opens as a floating window from the details' Diff button,
//! and selecting a row reloads it through a separate preview worker, so the main cvs
//! command slot stays free.

use std::path::Path;
use std::time::{Duration, Instant};

use cvsdesk::entries::State;
use cvsdesk::graph;
use cvsdesk::history::History;
use cvsdesk::sidediff::{DiffModel, DiffOptions, FileText};
use cvsdesk::truncate_columns;

use super::DialogAction;
use crate::app::ui::diff_view::DiffView;
use crate::app::ui::typography::small_monospace;
use crate::app::version_loader::{Loader, Msg, Side, Target};

const SELECTED_BG: egui::Color32 = egui::Color32::from_rgb(0xE8, 0xF0, 0xFE);
/// The second revision of a comparison. A different tint keeps it apart from the
/// primary selection while both stay visible.
const SECOND_BG: egui::Color32 = egui::Color32::from_rgb(0xEF, 0xF3, 0xE6);
const HOVER_BG: egui::Color32 = egui::Color32::from_rgb(0xF5, 0xF8, 0xFC);
const TRUNK: egui::Color32 = egui::Color32::from_rgb(0x40, 0x9E, 0xFF);
const BRANCH: egui::Color32 = egui::Color32::from_rgb(0x2F, 0x9E, 0x5F);
const REV_TEXT: egui::Color32 = egui::Color32::from_rgb(0x30, 0x90, 0xC0);
const MUTED: egui::Color32 = egui::Color32::from_gray(110);
const MESSAGE: egui::Color32 = egui::Color32::from_gray(60);
const WORKING: egui::Color32 = egui::Color32::from_rgb(0x9A, 0x65, 0x00);
const LATEST: egui::Color32 = egui::Color32::from_rgb(0x2F, 0x5F, 0xB3);
const DEAD: egui::Color32 = egui::Color32::from_rgb(0xA5, 0x0E, 0x0E);

const ROW_H: f32 = 28.0;
const GUTTER_LEFT: f32 = 14.0;
const LANE_GAP: f32 = 15.0;
/// How far a branch lane rounds off before it turns into the trunk. Four pixels take
/// the hard edge off a 1 px line without eating the 15 px run between the lanes.
const FORK_RADIUS: f32 = 4.0;
const REV_W: f32 = 62.0;
const DATE_W: f32 = 48.0;
const PREVIEW_DELAY: Duration = Duration::from_millis(150);

/// Which branch lanes the list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum BranchFilter {
    All,
    Trunk,
    Branch(String),
}

/// The window's own state. The revisions come from one `cvs log` reply; the diff comes
/// from the preview worker.
pub(crate) struct HistoryState {
    /// The folder the window was opened from. Browsing elsewhere in the main window
    /// must not change where a refresh or a diff preview runs.
    dir: std::path::PathBuf,
    root: String,
    file: String,
    working: String,
    working_state: State,
    history: History,
    selected: Option<usize>,
    /// The second revision a control click picked. With `selected` it gives the two
    /// ends the details' Compare button reads.
    second: Option<usize>,
    filter: String,
    /// The lowercased text each revision is searched by, built once per log refresh.
    haystacks: Vec<String>,
    /// Whether the detail panel shows every tag of the selected revision.
    tags_expanded: bool,
    branch_filter: BranchFilter,
    diff_target: Target,
    diff_open: bool,
    /// The two revisions a `Target::Pair` preview reads, older first. Kept so a reload
    /// repeats the same comparison after the window's state has moved on.
    pair_revisions: Option<(String, String)>,
    /// Raise each native window once when it opens, then leave the z-order to the user.
    raised: bool,
    diff_raise: bool,
    diff_model: Option<DiffModel>,
    diff_view: DiffView,
    diff_generation: u64,
    diff_error: String,
    diff_loading: bool,
    diff_revision: String,
    pending: Option<Instant>,
    loader: Option<Loader>,
    /// Set when the selection was chosen by the window rather than by a click, so the
    /// list scrolls the working revision into view after opening or refreshing.
    scroll_to_selected: bool,
    /// The sample window has no server behind it, so clicks must not start a preview.
    demo: bool,
    loading: bool,
    error: String,
}

/// The one tag shown for a revision, and how many more it has.
///
/// `cvs log` writes a new tag at the top of its symbol table, so the names arrive newest
/// first and the first one is the tag created most recently.
fn shown_tag(tags: &[String]) -> Option<(&str, usize)> {
    tags.first().map(|tag| (tag.as_str(), tags.len() - 1))
}

/// The lowercased text of every revision, built once when a log is loaded.
fn search_texts(history: &History) -> Vec<String> {
    history
        .revisions
        .iter()
        .map(|node| {
            let mut text = format!(
                "{} {} {} {} {} {}",
                node.revision,
                node.message,
                node.author,
                node.date_time,
                node.state,
                node.tags.join(" ")
            );
            if node.branch {
                if let Some(id) = graph::branch_id(&node.revision) {
                    if let Some(label) = history.branch_label(&id) {
                        text.push(' ');
                        text.push_str(label);
                    }
                }
            }
            text.to_lowercase()
        })
        .collect()
}

/// One line of message text, cut at `max_width` pixels with an ellipsis.
///
/// The column estimate the list used before assumed every letter took half the font
/// size, which is wrong for Chinese: a wide letter takes a whole em and ran over the
/// author and date columns. egui lays the row out at the real pixel width instead.
fn message_job(text: &str, font: egui::FontId, max_width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: font,
            color: MESSAGE,
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    job
}

impl HistoryState {
    pub(crate) fn new(
        dir: std::path::PathBuf,
        root: String,
        file: String,
        working: String,
        working_state: State,
        history: History,
    ) -> Self {
        let selected = history.working_index(&working);
        let haystacks = search_texts(&history);
        Self {
            dir,
            root,
            file,
            working,
            working_state,
            history,
            selected,
            second: None,
            filter: String::new(),
            haystacks,
            tags_expanded: false,
            branch_filter: BranchFilter::All,
            diff_target: Target::Previous,
            diff_open: false,
            pair_revisions: None,
            raised: false,
            diff_raise: false,
            diff_model: None,
            diff_view: DiffView::new(),
            diff_generation: 0,
            diff_error: String::new(),
            diff_loading: false,
            diff_revision: String::new(),
            pending: None,
            loader: None,
            scroll_to_selected: true,
            demo: false,
            loading: false,
            error: String::new(),
        }
    }

    /// The sample window shown by `--show-graph`: no server is involved, so it carries a
    /// saved diff instead of starting a preview worker.
    pub(crate) fn demo(
        dir: std::path::PathBuf,
        root: String,
        file: String,
        working: String,
        history: History,
    ) -> Self {
        let mut state = Self::new(dir, root, file, working, State::Unchanged, history);
        state.demo = true;
        state.pending = None;
        state.loader = None;
        state.diff_loading = false;
        state.diff_revision = "1.5".to_string();
        state.diff_model = Some(DiffModel::build(
            FileText::from_text(
                "main.c @ 1.4",
                "/* sample */\n#include <stdio.h>\n\nint main(void) { return 0; }\n",
                "c",
            ),
            FileText::from_text(
                "@ 1.5",
                "/* sample */\n#include <stdio.h>\n\nint main(void) { return 1; }\n",
                "c",
            ),
            DiffOptions::default(),
        ));
        state.diff_generation = 1;
        state
    }

    pub(crate) fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    pub(crate) fn file(&self) -> &str {
        &self.file
    }

    pub(crate) fn working(&self) -> &str {
        &self.working
    }

    pub(crate) fn working_state(&self) -> State {
        self.working_state
    }

    #[cfg(test)]
    pub(crate) fn revision_count(&self) -> usize {
        self.history.revisions.len()
    }

    pub(crate) fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
        if loading {
            self.loader = None;
            self.pending = None;
            self.diff_loading = false;
            self.diff_model = None;
            self.diff_revision.clear();
        }
    }

    pub(crate) fn set_error(&mut self, error: String) {
        self.error = error;
        self.loading = false;
    }

    pub(crate) fn replace_history(
        &mut self,
        history: History,
        working: String,
        working_state: State,
    ) {
        let previous_revision = self
            .selected
            .and_then(|index| self.history.revisions.get(index))
            .map(|node| node.revision.clone());

        if let BranchFilter::Branch(id) = &self.branch_filter {
            if !history.branch_ids().iter().any(|known| known == id) {
                self.branch_filter = BranchFilter::All;
            }
        }

        self.history = history;
        self.haystacks = search_texts(&self.history);
        self.working = working;
        self.working_state = working_state;
        self.loading = false;
        self.error.clear();
        self.tags_expanded = false;
        self.second = None;
        self.leave_pair();
        self.loader = None;
        self.diff_model = None;
        self.diff_error.clear();
        self.diff_revision.clear();
        self.selected = previous_revision
            .and_then(|revision| {
                self.history
                    .revisions
                    .iter()
                    .position(|node| node.revision == revision)
            })
            .or_else(|| self.history.working_index(&self.working));
        self.queue_preview();
        self.scroll_to_selected = true;
    }

    /// Start a debounced preview when there is a revision and the diff window is open.
    /// With the window closed nothing runs, so browsing revisions costs no `cvs diff`.
    fn queue_preview(&mut self) {
        if self.demo || !self.diff_open || self.selected.is_none() {
            self.pending = None;
            self.diff_loading = false;
        } else {
            self.pending = Some(Instant::now());
            self.diff_loading = true;
        }
    }

    /// Apply a click on a revision row. A plain click moves the primary selection; a
    /// control click (Command on a Mac) picks the second revision the Compare button
    /// reads, or drops it when the same row is clicked again.
    fn click(&mut self, index: usize, extend: bool) {
        if extend {
            let previous = self.second;
            if self.selected == Some(index) || self.second == Some(index) {
                self.second = None;
            } else {
                self.second = Some(index);
            }
            self.leave_pair();
            if self.second != previous {
                self.queue_preview();
            }
            return;
        }
        if self.selected == Some(index) {
            self.second = None;
            return;
        }
        self.selected = Some(index);
        self.second = None;
        self.tags_expanded = false;
        self.leave_pair();
        self.loader = None;
        self.diff_model = None;
        self.diff_error.clear();
        self.diff_revision.clear();
        self.queue_preview();
    }

    /// A changed selection ends a two-revision comparison: the diff window falls back to
    /// the revision the new row stands for.
    fn leave_pair(&mut self) {
        if self.diff_target == Target::Pair {
            self.diff_target = Target::Previous;
        }
        self.pair_revisions = None;
    }

    /// The two revisions the Compare button reads, older first. `cvs log` lists
    /// revisions newest first, so the larger row index is the older revision.
    fn pair(&self) -> Option<(&str, &str)> {
        let primary = self.selected?;
        let second = self.second?;
        if primary == second {
            return None;
        }
        let (older, newer) = if primary > second {
            (primary, second)
        } else {
            (second, primary)
        };
        Some((
            self.history.revisions.get(older)?.revision.as_str(),
            self.history.revisions.get(newer)?.revision.as_str(),
        ))
    }

    /// Show the two picked revisions side by side, opening the diff window if it was
    /// closed. The window says which two revisions it shows.
    fn compare_selected(&mut self) {
        let Some(pair) = self
            .pair()
            .map(|(older, newer)| (older.to_string(), newer.to_string()))
        else {
            return;
        };
        self.pair_revisions = Some(pair);
        self.diff_target = Target::Pair;
        self.diff_open = true;
        self.diff_raise = true;
        self.loader = None;
        self.diff_model = None;
        self.diff_error.clear();
        self.diff_revision.clear();
        self.queue_preview();
    }

    fn set_target(&mut self, target: Target) {
        if self.diff_target == target {
            return;
        }
        self.diff_target = target;
        if !self.demo {
            self.loader = None;
            self.diff_model = None;
            self.diff_error.clear();
            self.diff_revision.clear();
        }
        self.queue_preview();
    }

    fn set_diff_open(&mut self, open: bool) {
        if self.diff_open == open {
            return;
        }
        self.diff_open = open;
        if open {
            self.diff_raise = true;
            if !self.demo {
                self.loader = None;
                self.diff_model = None;
                self.diff_error.clear();
                self.diff_revision.clear();
            }
            self.queue_preview();
        } else {
            self.loader = None;
            self.pending = None;
            self.diff_loading = false;
        }
    }

    fn reload_diff(&mut self) {
        if self.demo {
            return;
        }
        self.loader = None;
        self.diff_model = None;
        self.diff_error.clear();
        self.diff_revision.clear();
        self.queue_preview();
    }

    fn tick(&mut self, ctx: &egui::Context, globals: &[String]) {
        self.poll_loader();

        if !self.demo && self.diff_open && self.loader.is_none() {
            if let Some(at) = self.pending {
                if at.elapsed() >= PREVIEW_DELAY {
                    self.pending = None;
                    let dir = self.dir.clone();
                    let root = self.root.clone();
                    self.start_loader(&dir, &root, globals);
                }
            }
        }

        if self.pending.is_some() || self.loader.is_some() || self.loading {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn start_loader(&mut self, dir: &Path, root: &str, globals: &[String]) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(node) = self.history.revisions.get(index) else {
            return;
        };
        let revision = node.revision.clone();
        let (left, right, identity) = match self.diff_target {
            Target::Previous => {
                let Some(previous) = self.history.previous_revision(index) else {
                    self.diff_model = None;
                    self.diff_error = "Initial revision — no previous revision to compare.".into();
                    self.diff_loading = false;
                    self.diff_revision = revision;
                    return;
                };
                (
                    Side::Revision(previous),
                    Side::Revision(revision.clone()),
                    revision,
                )
            }
            Target::Working => (
                Side::Revision(revision.clone()),
                Side::WorkingCopy,
                revision,
            ),
            Target::Pair => {
                let Some((older, newer)) = self.pair_revisions.clone() else {
                    self.diff_model = None;
                    self.diff_error = "Pick a second revision to compare.".into();
                    self.diff_loading = false;
                    return;
                };
                let identity = format!("{older}..{newer}");
                (Side::Revision(older), Side::Revision(newer), identity)
            }
        };

        self.diff_model = None;
        self.diff_error.clear();
        self.diff_loading = true;
        self.loader = Some(Loader::start(
            dir.to_path_buf(),
            root.to_string(),
            globals.to_vec(),
            self.file.clone(),
            left,
            right,
            identity,
            self.diff_target,
        ));
    }

    fn poll_loader(&mut self) {
        let Some(loader) = self.loader.as_ref() else {
            return;
        };
        let message = match loader.rx.try_recv() {
            Ok(message) => message,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                log::error!(
                    target: "cvsdesk::diff",
                    "phase=preview result=worker-vanished"
                );
                Msg::Failed("the diff preview stopped".into())
            }
        };
        let loader = self.loader.take().expect("the loader was just borrowed");
        let current = self
            .selected
            .and_then(|index| self.history.revisions.get(index))
            .map(|node| node.revision.as_str());
        let expected = match self.diff_target {
            Target::Pair => self
                .pair_revisions
                .as_ref()
                .map(|(older, newer)| format!("{older}..{newer}")),
            Target::Previous | Target::Working => current.map(str::to_string),
        };
        if expected.as_deref() != Some(loader.revision.as_str())
            || self.diff_target != loader.target
        {
            return;
        }

        match message {
            Msg::Ready(model) => {
                self.diff_model = Some(*model);
                self.diff_generation = self.diff_generation.wrapping_add(1);
                self.diff_error.clear();
                self.diff_loading = false;
                self.diff_revision = loader.revision.clone();
            }
            Msg::Failed(error) => {
                // The text can name a path (`cannot read <path>: ...`), so only the event
                // goes to the file; the window shows the full message.
                log::warn!(target: "cvsdesk::diff", "phase=preview result=failed");
                self.diff_model = None;
                self.diff_error = error;
                self.diff_loading = false;
            }
        }
    }

    fn visible_indices(&self) -> Vec<usize> {
        let query = self.filter.trim().to_lowercase();
        // The list must follow the same order as the graph gutter, or a branch lane's
        // dots would sit on rows that are not its revisions.
        graph::display_order(&self.history.revisions)
            .into_iter()
            .filter(|index| {
                let node = &self.history.revisions[*index];
                let branch_ok = match &self.branch_filter {
                    BranchFilter::All => true,
                    BranchFilter::Trunk => !node.branch,
                    BranchFilter::Branch(id) => graph::branch_id(&node.revision)
                        .as_deref()
                        .is_some_and(|known| known == id),
                };
                branch_ok
                    && (query.is_empty()
                        || self
                            .haystacks
                            .get(*index)
                            .is_some_and(|text| text.contains(&query)))
            })
            .collect()
    }

    fn ensure_visible(&mut self, visible: &[usize]) {
        if visible.is_empty() {
            self.selected = None;
            self.second = None;
            self.leave_pair();
            self.tags_expanded = false;
            self.loader = None;
            self.pending = None;
            self.diff_loading = false;
            self.diff_model = None;
            self.diff_error.clear();
            self.diff_revision.clear();
            return;
        }
        if self
            .selected
            .is_none_or(|selected| !visible.contains(&selected))
        {
            self.click(visible[0], false);
            self.scroll_to_selected = true;
        }
        if self.second.is_some_and(|second| !visible.contains(&second)) {
            self.second = None;
            self.leave_pair();
        }
    }
}

/// Draw the windows. `dir`, `root`, the global flags and the diff flags are the pieces
/// the preview worker needs; the dialog itself only owns the revision state.
///
/// Both windows are real OS windows, so they can leave the main window and sit on
/// another monitor. The close button arrives as a viewport event; the state is still
/// owned by `App`, which is why this borrows it instead of moving it into a callback.
pub(super) fn show(
    ctx: &egui::Context,
    state: &mut HistoryState,
    globals: &[String],
    raise: bool,
) -> Option<DialogAction> {
    state.tick(ctx, globals);

    let mut action = None;
    let mut open = true;

    ctx.show_viewport_immediate(
        history_viewport_id(),
        history_viewport(state, raise),
        |ui, _class| {
            egui::Panel::top("file_history_toolbar").show(ui, |ui| toolbar(ui, state, &mut action));

            let visible = state.visible_indices();
            state.ensure_visible(&visible);

            // Panels keep the status visible whatever the window size is, and let the
            // user drag the split between the revision list and the details. The diff
            // has its own window, opened from the details' Diff button.
            egui::Panel::bottom("file_history_status").show(ui, |ui| status(ui, state));
            egui::Panel::right("file_history_details")
                .resizable(true)
                .default_size(320.0)
                .min_size(240.0)
                .show(ui, |ui| details(ui, state));
            egui::CentralPanel::default()
                .show(ui, |ui| list(ui, state, &visible, ui.available_height()));

            if ui.ctx().input(|i| i.viewport().close_requested()) {
                open = false;
            }
        },
    );

    // A fresh native window can be created behind the parent. Ask for the front once;
    // after that the user owns the z-order.
    if raise && !state.raised {
        state.raised = true;
        ctx.send_viewport_cmd_to(history_viewport_id(), egui::ViewportCommand::Focus);
    }

    // Keep the diff window warm from the first frame of a normal run. Creating a
    // native window and its wgpu surface is expensive, and doing it inside the frame
    // of the Diff… click froze the window for a moment; building it here moves that
    // cost to the frame that opens the history, while the user is already waiting for
    // the log. A screenshot run (`raise == false`) only builds it when it is open.
    if state.diff_open || raise {
        diff_window(ctx, state, raise);
    }

    if !open {
        // One more pass drops the viewports that are no longer shown.
        ctx.request_repaint();
        action = Some(DialogAction::Close);
    }
    action
}

fn history_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("file_history")
}

fn history_viewport(state: &HistoryState, active: bool) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(format!("File History — {}", state.file))
        .with_inner_size([920.0, 650.0])
        .with_min_inner_size([700.0, 420.0])
        .with_active(active)
}

fn diff_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("file_history_diff")
}

fn diff_viewport(state: &HistoryState, visible: bool, active: bool) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(format!("Diff — {}", state.file))
        .with_inner_size([1100.0, 650.0])
        .with_min_inner_size([640.0, 320.0])
        .with_visible(visible)
        .with_active(active)
}

/// The diff is its own OS window beside the history window, so the user can keep
/// clicking revisions and watch the diff follow the selection, even on another monitor.
///
/// The window is created hidden with the history window and stays alive while the
/// history is open. Closing the diff only hides it: a new native window and its wgpu
/// surface are expensive, and creating them in the frame of the Diff… click froze the
/// whole application for a moment.
fn diff_window(ctx: &egui::Context, state: &mut HistoryState, raise: bool) {
    let visible = state.diff_open;
    let mut close_requested = false;

    ctx.show_viewport_immediate(
        diff_viewport_id(),
        // `active` must not follow `visible`: changing it makes egui recreate the
        // window, which is the cost this function avoids. The explicit Focus command
        // below raises the window when it opens.
        diff_viewport(state, visible, raise),
        |ui, _class| {
            if !visible {
                // A hidden viewport still runs its UI and paints. Drawing nothing
                // keeps the price of keeping it warm negligible.
                return;
            }
            egui::CentralPanel::default().show(ui, |ui| diff(ui, state));
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                close_requested = true;
            }
        },
    );

    if visible && raise && state.diff_raise {
        state.diff_raise = false;
        ctx.send_viewport_cmd_to(diff_viewport_id(), egui::ViewportCommand::Focus);
    }

    if close_requested {
        state.set_diff_open(false);
        // Hide the window instead of dropping its viewport: the next Diff… must not
        // create another native window.
        ctx.send_viewport_cmd_to(diff_viewport_id(), egui::ViewportCommand::Visible(false));
        ctx.request_repaint();
    }
}

fn toolbar(ui: &mut egui::Ui, state: &mut HistoryState, action: &mut Option<DialogAction>) {
    ui.horizontal(|ui| {
        if ui.button("Refresh").clicked() {
            *action = Some(DialogAction::HistoryRefresh);
        }

        let mut branch = state.branch_filter.clone();
        let branch_text = match &branch {
            BranchFilter::All => "All branches".to_string(),
            BranchFilter::Trunk => "Trunk only".to_string(),
            BranchFilter::Branch(id) => state.history.branch_label(id).unwrap_or(id).to_string(),
        };
        egui::ComboBox::from_id_salt("file_history_branch")
            .selected_text(branch_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut branch, BranchFilter::All, "All branches");
                ui.selectable_value(&mut branch, BranchFilter::Trunk, "Trunk only");
                for id in state.history.branch_ids() {
                    let label = state.history.branch_label(&id).unwrap_or(&id).to_string();
                    ui.selectable_value(&mut branch, BranchFilter::Branch(id), label);
                }
            });
        if branch != state.branch_filter {
            state.branch_filter = branch;
        }

        ui.add(
            egui::TextEdit::singleline(&mut state.filter)
                .hint_text("Filter revisions")
                .desired_width(190.0),
        );

        if state.loading {
            ui.spinner();
            ui.label(egui::RichText::new("Refreshing…").small().color(MUTED));
        }
        if !state.error.is_empty() {
            ui.colored_label(DEAD, &state.error);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(state.history.summary())
                    .small()
                    .color(MUTED),
            );
        });
    });
}

fn list(ui: &mut egui::Ui, state: &mut HistoryState, visible: &[usize], height: f32) {
    if state.history.revisions.is_empty() {
        ui.label("No revisions in this log.");
        return;
    }
    if visible.is_empty() {
        ui.label("No revisions match the filter.");
        return;
    }

    let order = graph::display_order(&state.history.revisions);
    let branches = graph::branch_layout(&state.history.revisions, &order);
    let mut row_of = vec![0usize; state.history.revisions.len()];
    for (row, &index) in order.iter().enumerate() {
        row_of[index] = row;
    }
    let mut lane_of = vec![0usize; state.history.revisions.len()];
    for branch in &branches {
        for &index in &branch.revisions {
            lane_of[index] = branch.lane;
        }
    }
    let lanes = branches.iter().map(|branch| branch.lane).max().unwrap_or(0) + 1;
    let gutter = GUTTER_LEFT + (lanes.saturating_sub(1)) as f32 * LANE_GAP + 10.0;
    let header_h = 20.0;
    let (header, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), header_h),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(header);
    let small = egui::TextStyle::Small.resolve(ui.style());
    let y = header.center().y;
    let rev_x = header.left() + gutter + 2.0;
    let date_right = header.right() - 6.0;
    let author_right = date_right - DATE_W;
    painter.text(
        egui::pos2(rev_x, y),
        egui::Align2::LEFT_CENTER,
        "Revision",
        small.clone(),
        MUTED,
    );
    painter.text(
        egui::pos2(rev_x + REV_W, y),
        egui::Align2::LEFT_CENTER,
        "Message",
        small.clone(),
        MUTED,
    );
    painter.text(
        egui::pos2(author_right, y),
        egui::Align2::RIGHT_CENTER,
        "Author",
        small.clone(),
        MUTED,
    );
    painter.text(
        egui::pos2(date_right, y),
        egui::Align2::RIGHT_CENTER,
        "Date",
        small,
        MUTED,
    );
    painter.line_segment(
        [
            egui::pos2(header.left(), header.bottom()),
            egui::pos2(header.right(), header.bottom()),
        ],
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(225)),
    );

    let working_index = state.history.revisions.iter().position(|node| {
        !state.working.is_empty() && node.revision == state.working && state.working != "0"
    });
    let latest = state.history.latest_trunk_index();
    let trunk_rows: Vec<usize> = order
        .iter()
        .enumerate()
        .filter(|(_, index)| !state.history.revisions[**index].branch)
        .map(|(row, _)| row)
        .collect();
    let branch_ranges: Vec<(usize, usize, usize, bool)> = branches
        .iter()
        .map(|branch| {
            let start = branch
                .revisions
                .iter()
                .map(|index| row_of[*index])
                .min()
                .unwrap_or(0);
            let end = branch.fork.map(|fork| row_of[fork]).unwrap_or_else(|| {
                branch
                    .revisions
                    .iter()
                    .map(|index| row_of[*index])
                    .max()
                    .unwrap_or(start)
            });
            (branch.lane, start, end, branch.fork.is_some())
        })
        .collect();

    let mut clicked = None;
    egui::ScrollArea::vertical()
        .id_salt("file_history_list")
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // A lane is painted inside each row, and the rows keep a 28 px pitch either
            // way: the gap between rows would cut the branch lines into dashes, so the
            // rows sit flush and carry the old spacing in their own height.
            ui.spacing_mut().item_spacing.y = 0.0;
            for &index in visible {
                let node = &state.history.revisions[index];
                let row = row_of[index];
                let lane = lane_of[index];
                let tags = &node.tags;
                let branch_label = if node.branch {
                    graph::branch_id(&node.revision)
                        .and_then(|id| state.history.branch_label(&id))
                        .map(str::to_string)
                } else {
                    None
                };
                let first_on_branch = branch_label.is_some()
                    && branches
                        .iter()
                        .find(|branch| branch.revisions.contains(&index))
                        .and_then(|branch| branch.revisions.first())
                        .is_some_and(|first| *first == index);

                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), ROW_H),
                    egui::Sense::click(),
                );
                if response.clicked() && !state.demo {
                    let extend = ui.input(|input| input.modifiers.ctrl || input.modifiers.command);
                    clicked = Some((index, extend));
                }
                if state.selected == Some(index) && state.scroll_to_selected {
                    state.scroll_to_selected = false;
                    response.scroll_to_me(Some(egui::Align::Center));
                }

                let painter = ui.painter_at(rect);
                if state.selected == Some(index) {
                    painter.rect_filled(rect, 0.0, SELECTED_BG);
                } else if state.second == Some(index) {
                    painter.rect_filled(rect, 0.0, SECOND_BG);
                } else if response.hovered() {
                    painter.rect_filled(rect, 0.0, HOVER_BG);
                }

                draw_lanes(&painter, rect, row, lane, &trunk_rows, &branch_ranges);

                let mono = small_monospace();
                let small = egui::TextStyle::Small.resolve(ui.style());
                let body = egui::TextStyle::Body.resolve(ui.style());
                let y = rect.center().y;
                let rev_x = rect.left() + gutter + 2.0;
                let rev_color = if node.branch { BRANCH } else { REV_TEXT };
                painter.text(
                    egui::pos2(rev_x, y),
                    egui::Align2::LEFT_CENTER,
                    &node.revision,
                    mono,
                    rev_color,
                );

                let date_right = rect.right() - 6.0;
                let author_right = date_right - DATE_W;
                let author = truncate_columns(&node.author, 10);
                // The author is right-aligned at author_right, so its text grows to the
                // left. The message and the tag badges must stop before that left edge,
                // not at the column's right edge.
                let author_width = painter
                    .layout_no_wrap(author.clone(), small.clone(), MUTED)
                    .size()
                    .x;
                let msg_x = rev_x + REV_W;
                let right_limit = author_right - author_width - 8.0;
                let mut x = msg_x;

                if index == working_index.unwrap_or(usize::MAX) {
                    x = badge_fit(&painter, x, y, "working", &small, WORKING, right_limit);
                }
                if index == latest.unwrap_or(usize::MAX) {
                    x = badge_fit(&painter, x, y, "latest", &small, LATEST, right_limit);
                }
                if node.state == "dead" {
                    x = badge_fit(&painter, x, y, "dead", &small, DEAD, right_limit);
                }
                if first_on_branch {
                    if let Some(label) = &branch_label {
                        x = badge_fit(&painter, x, y, label, &small, BRANCH, right_limit);
                    }
                }
                if let Some((tag, more)) = shown_tag(tags) {
                    let more_label = (more > 0).then(|| format!("+{more}"));
                    let reserve = more_label.as_ref().map_or(0.0, |label| {
                        painter
                            .layout_no_wrap(label.clone(), small.clone(), MUTED)
                            .size()
                            .x
                            + 5.0
                    });
                    x = badge_fit(&painter, x, y, tag, &small, REV_TEXT, right_limit - reserve);
                    if let Some(label) = more_label {
                        x = badge_fit(&painter, x, y, &label, &small, MUTED, right_limit);
                    }
                }

                draw_message(&painter, x.max(msg_x), y, &node.message, body, right_limit);
                painter.text(
                    egui::pos2(author_right, y),
                    egui::Align2::RIGHT_CENTER,
                    author,
                    small.clone(),
                    MUTED,
                );
                painter.text(
                    egui::pos2(date_right, y),
                    egui::Align2::RIGHT_CENTER,
                    short_date(&node.date),
                    small,
                    MUTED,
                );
            }
        });

    if let Some((index, extend)) = clicked {
        state.click(index, extend);
    }
}

fn badge(
    painter: &egui::Painter,
    x: f32,
    y: f32,
    text: &str,
    font: &egui::FontId,
    color: egui::Color32,
) -> f32 {
    let rect = painter.text(
        egui::pos2(x, y),
        egui::Align2::LEFT_CENTER,
        text,
        font.clone(),
        color,
    );
    rect.right() + 5.0
}

/// Paint a badge that may not run past `right_limit`; a long tag is shortened with an
/// ellipsis instead of covering the author and date columns.
fn badge_fit(
    painter: &egui::Painter,
    x: f32,
    y: f32,
    text: &str,
    font: &egui::FontId,
    color: egui::Color32,
    right_limit: f32,
) -> f32 {
    let room = (right_limit - x - 5.0).max(0.0);
    if room <= 0.0 {
        return x;
    }
    let full = painter.layout_no_wrap(text.to_string(), font.clone(), color);
    if full.size().x <= room {
        return badge(painter, x, y, text, font, color);
    }

    // Longest prefix that still fits with the ellipsis after it.
    let chars: Vec<char> = text.chars().collect();
    let mut low = 0;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high).div_ceil(2);
        let mut candidate: String = chars[..mid].iter().collect();
        candidate.push('…');
        let width = painter
            .layout_no_wrap(candidate, font.clone(), color)
            .size()
            .x;
        if width <= room {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    if low == 0 {
        return x;
    }
    let mut shown: String = chars[..low].iter().collect();
    shown.push('…');
    badge(painter, x, y, &shown, font, color)
}

/// Draw a message in the columns between `x` and `right_limit`, cut with an ellipsis at
/// the real pixel width. Nothing is drawn when the badges already used the room.
fn draw_message(
    painter: &egui::Painter,
    x: f32,
    y: f32,
    text: &str,
    font: egui::FontId,
    right_limit: f32,
) {
    let width = right_limit - x;
    if text.is_empty() || width < 24.0 {
        return;
    }
    let galley = painter.layout_job(message_job(text, font, width));
    painter.galley(egui::pos2(x, y - galley.size().y / 2.0), galley, MESSAGE);
}

/// The vertical part of a lane in one row, as (starts at the row top, reaches the row
/// bottom). A lane begins and ends at the node in the middle of its first and last row,
/// so two branches that share a lane are not joined by the line between their rows.
fn lane_span(row: usize, start: usize, end: usize) -> Option<(bool, bool)> {
    if row < start || row > end || start == end {
        return None;
    }
    Some((row != start, row != end))
}

/// The points of the quarter circle that turns a branch lane into the trunk. The arc
/// starts `radius` above the fork node and ends `radius` left of it, so the straight
/// lane and the fork line meet it with no kink.
fn fork_arc(x: f32, y: f32, radius: f32) -> Vec<egui::Pos2> {
    const STEPS: usize = 8;
    (0..=STEPS)
        .map(|step| {
            let angle = step as f32 / STEPS as f32 * std::f32::consts::FRAC_PI_2;
            egui::pos2(
                x - radius + radius * angle.cos(),
                y - radius + radius * angle.sin(),
            )
        })
        .collect()
}

fn draw_lanes(
    painter: &egui::Painter,
    rect: egui::Rect,
    row: usize,
    lane: usize,
    trunk_rows: &[usize],
    branch_ranges: &[(usize, usize, usize, bool)],
) {
    let x = |lane: usize| rect.left() + GUTTER_LEFT + lane as f32 * LANE_GAP;
    let top = rect.top();
    let bottom = rect.bottom();
    let middle = rect.center().y;

    if let (Some(&first), Some(&last)) = (trunk_rows.first(), trunk_rows.last()) {
        if let Some((from_top, to_bottom)) = lane_span(row, first, last) {
            painter.line_segment(
                [
                    egui::pos2(x(0), if from_top { top } else { middle }),
                    egui::pos2(x(0), if to_bottom { bottom } else { middle }),
                ],
                egui::Stroke::new(1.0_f32, TRUNK),
            );
        }
    }
    for &(branch_lane, start, end, has_fork) in branch_ranges {
        if let Some((from_top, to_bottom)) = lane_span(row, start, end) {
            let lane_x = x(branch_lane);
            // The fork row ends in a rounded turn into the trunk instead of a corner.
            let turns = has_fork && row == end;
            let lane_end = if to_bottom {
                bottom
            } else if turns {
                middle - FORK_RADIUS
            } else {
                middle
            };
            painter.line_segment(
                [
                    egui::pos2(lane_x, if from_top { top } else { middle }),
                    egui::pos2(lane_x, lane_end),
                ],
                egui::Stroke::new(1.0_f32, BRANCH),
            );
            if turns {
                painter.add(egui::Shape::line(
                    fork_arc(lane_x, middle, FORK_RADIUS),
                    egui::Stroke::new(1.0_f32, BRANCH),
                ));
                painter.line_segment(
                    [
                        egui::pos2(lane_x - FORK_RADIUS, middle),
                        egui::pos2(x(0), middle),
                    ],
                    egui::Stroke::new(1.0_f32, BRANCH),
                );
            }
        }
    }

    let color = if lane == 0 { TRUNK } else { BRANCH };
    painter.circle(
        egui::pos2(x(lane), middle),
        3.2,
        egui::Color32::WHITE,
        egui::Stroke::new(1.5_f32, color),
    );
}

fn details(ui: &mut egui::Ui, state: &mut HistoryState) {
    let Some(index) = state.selected else {
        ui.label("Select a revision.");
        return;
    };
    let Some(node) = state.history.revisions.get(index).cloned() else {
        return;
    };

    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(format!("Revision {}", node.revision)).heading());
        if let Some((tag, more)) = shown_tag(&node.tags) {
            ui.label(egui::RichText::new(tag).small().color(REV_TEXT));
            if more > 0 {
                let label = if state.tags_expanded {
                    "Show less".to_string()
                } else {
                    format!("+{more} more")
                };
                if ui.small_button(label).clicked() {
                    state.tags_expanded = !state.tags_expanded;
                }
            }
        }
        if node.branch {
            if let Some(id) = graph::branch_id(&node.revision) {
                let label = state.history.branch_label(&id).unwrap_or(&id);
                ui.label(egui::RichText::new(label).small().color(BRANCH));
            }
        } else {
            ui.label(egui::RichText::new("trunk").small().color(MUTED));
        }
    });

    if state.tags_expanded && node.tags.len() > 1 {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("file_history_tags")
                .max_height(160.0)
                .auto_shrink([false, true])
                .show_rows(ui, 20.0, node.tags.len(), |ui, rows| {
                    for tag in &node.tags[rows] {
                        ui.label(egui::RichText::new(tag).small().color(REV_TEXT));
                    }
                });
        });
    }

    ui.label(
        egui::RichText::new(format!("{} · {}", node.author, node.date_time))
            .small()
            .color(MUTED),
    );
    ui.horizontal(|ui| {
        if node.state.is_empty() {
            ui.label(egui::RichText::new("state unknown").small().color(MUTED));
        } else if node.state == "dead" {
            ui.label(egui::RichText::new("dead").small().color(DEAD));
        } else {
            ui.label(egui::RichText::new(&node.state).small().color(MUTED));
        }
        if let Some((added, removed)) = node.lines {
            ui.label(
                egui::RichText::new(format!("+{} −{}", added, removed))
                    .small()
                    .color(MUTED),
            );
        }
    });

    ui.separator();
    let mut message = node.message_full.as_str();
    egui::ScrollArea::vertical()
        .id_salt("file_history_message")
        .max_height(110.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut message)
                    .desired_width(f32::INFINITY)
                    .desired_rows(4)
                    .font(egui::TextStyle::Body),
            );
        });

    ui.separator();
    let compare_label = state
        .pair()
        .map(|(older, newer)| format!("Compare {older} → {newer}"));
    ui.horizontal_wrapped(|ui| {
        if ui.button("Copy revision").clicked() {
            ui.ctx().copy_text(node.revision.clone());
        }
        if let Some(label) = compare_label {
            if ui.button(label).clicked() {
                state.compare_selected();
            }
        } else if ui.button("Diff…").clicked() {
            state.set_diff_open(true);
        }
    });
}

fn diff(ui: &mut egui::Ui, state: &mut HistoryState) {
    ui.horizontal(|ui| {
        if state.diff_target == Target::Pair {
            if let Some((older, newer)) = &state.pair_revisions {
                ui.label(egui::RichText::new(format!("{older} → {newer}")).strong());
            }
            ui.separator();
        }
        let previous_on = state.diff_target == Target::Previous;
        if ui.selectable_label(previous_on, "This revision").clicked() {
            state.set_target(Target::Previous);
        }
        let working_on = state.diff_target == Target::Working;
        if ui.selectable_label(working_on, "Local file").clicked() {
            state.set_target(Target::Working);
        }
        if state.diff_loading {
            ui.spinner();
            ui.label(egui::RichText::new("Loading…").small().color(MUTED));
        }
        if !state.diff_error.is_empty() {
            ui.colored_label(DEAD, &state.diff_error);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Reload").clicked() {
                state.reload_diff();
            }
            if let Some(index) = state.selected {
                if let Some(node) = state.history.revisions.get(index) {
                    if state.diff_target != Target::Pair
                        && !state.diff_revision.is_empty()
                        && state.diff_revision != node.revision
                        && !state.diff_loading
                    {
                        ui.label(
                            egui::RichText::new("diff is for an older selection")
                                .small()
                                .color(WORKING),
                        );
                    }
                }
            }
        });
    });

    if state.selected.is_none() {
        ui.label("Select a revision.");
        return;
    }
    if state.diff_loading && state.diff_model.is_none() {
        ui.label("Loading diff…");
        return;
    }
    if let Some(model) = &state.diff_model {
        let generation = state.diff_generation;
        state.diff_view.show(ui, model, generation);
    } else if state.diff_error.is_empty() {
        ui.label("No changes.");
    } else {
        ui.label("No diff available.");
    }
}

fn status(ui: &mut egui::Ui, state: &HistoryState) {
    ui.horizontal(|ui| {
        let revision = if state.working.is_empty() {
            "unknown".to_string()
        } else {
            state.working.clone()
        };
        ui.label(
            egui::RichText::new(format!(
                "Local file: {} · {}",
                revision,
                state.working_state.label()
            ))
            .small()
            .color(MUTED),
        );
        if state.working == "0"
            || (!state.working.is_empty()
                && !state
                    .history
                    .revisions
                    .iter()
                    .any(|node| node.revision == state.working))
        {
            ui.label(
                egui::RichText::new("working revision is not in the log")
                    .small()
                    .color(WORKING),
            );
        }
    });
}

fn short_date(date: &str) -> &str {
    date.get(5..).unwrap_or(date)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HistoryState {
        let log: Vec<String> = cvsdesk::demo::SAMPLE_LOG
            .lines()
            .map(str::to_string)
            .collect();
        let history = History::parse("main.c", &log);
        HistoryState::new(
            std::path::PathBuf::from("."),
            String::new(),
            "main.c".into(),
            "1.4".into(),
            State::Unchanged,
            history,
        )
    }

    fn toolbar_texts(state: &mut HistoryState) -> Vec<String> {
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let mut action = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(920.0, 120.0),
                )),
                ..Default::default()
            },
            |ui| toolbar(ui, state, &mut action),
        );
        output.textures_delta.clear();
        shape_texts(&output)
    }

    fn details_texts(state: &mut HistoryState) -> Vec<String> {
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(620.0, 460.0),
                )),
                ..Default::default()
            },
            |ui| details(ui, state),
        );
        output.textures_delta.clear();
        shape_texts(&output)
    }

    fn shape_texts(output: &egui::FullOutput) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }
        texts
    }

    fn index_of(state: &HistoryState, revision: &str) -> usize {
        state
            .history
            .revisions
            .iter()
            .position(|node| node.revision == revision)
            .expect("the sample log lists every revision")
    }

    #[test]
    fn a_plain_click_drops_the_second_revision() {
        let mut state = sample();
        let older = index_of(&state, "1.2");
        let another = index_of(&state, "1.3");
        state.click(older, true);
        assert_eq!(state.second, Some(older), "the control click picks it");

        state.click(another, false);

        assert_eq!(state.selected, Some(another));
        assert_eq!(state.second, None, "one row is one revision again");
    }

    #[test]
    fn a_control_click_keeps_the_first_revision_and_picks_the_second() {
        let mut state = sample();
        let first = state.selected.expect("the working revision is selected");
        let older = index_of(&state, "1.2");

        state.click(older, true);

        assert_eq!(state.selected, Some(first), "the first stays the anchor");
        assert_eq!(state.second, Some(older));
    }

    #[test]
    fn the_comparison_runs_from_the_older_revision_to_the_newer() {
        let mut state = sample();
        let older = index_of(&state, "1.2");
        let newer = index_of(&state, "1.4");
        state.click(newer, false);
        state.click(older, true);

        assert_eq!(
            state.pair(),
            Some(("1.2", "1.4")),
            "the log lists revisions newest first, so the larger row is older"
        );
    }

    #[test]
    fn the_details_offer_the_comparison_once_two_revisions_are_picked() {
        let mut state = sample();
        let older = index_of(&state, "1.2");
        state.click(older, true);

        let texts = details_texts(&mut state);

        assert!(
            texts.iter().any(|text| text == "Compare 1.2 → 1.4"),
            "the button names both ends of the comparison: {texts:?}"
        );
    }

    #[test]
    fn the_details_keep_the_plain_diff_for_one_revision() {
        let mut state = sample();
        let texts = details_texts(&mut state);

        assert!(
            texts.iter().any(|text| text == "Diff…"),
            "one revision still opens the plain diff: {texts:?}"
        );
    }

    #[test]
    fn the_history_toolbar_offers_no_raw_log() {
        let mut state = sample();
        let texts = toolbar_texts(&mut state);

        assert!(
            !texts.iter().any(|text| text == "More" || text == "Raw log"),
            "the toolbar must not offer a raw log: {texts:?}"
        );
    }

    #[test]
    fn the_working_revision_is_selected_on_open() {
        let state = sample();
        assert_eq!(
            state.selected,
            state
                .history
                .revisions
                .iter()
                .position(|n| n.revision == "1.4")
        );
        assert!(
            !state.diff_open,
            "the diff waits for its window to be opened"
        );
        assert!(!state.diff_loading, "opening the window starts no diff");
        assert!(state.pending.is_none(), "no preview is queued");
    }

    #[test]
    fn the_diff_preview_starts_only_when_its_window_opens() {
        let mut state = sample();
        state.set_diff_open(true);

        assert!(state.diff_open);
        assert!(
            state.pending.is_some(),
            "opening the diff window queues a preview"
        );
        assert!(
            state.diff_loading,
            "the diff window shows its loading state"
        );
    }

    #[test]
    fn closing_the_diff_window_cancels_the_preview() {
        let mut state = sample();
        state.set_diff_open(true);
        state.set_diff_open(false);

        assert!(!state.diff_open);
        assert!(state.pending.is_none(), "the debounce is dropped");
        assert!(!state.diff_loading, "the spinner stops");
    }

    /// The OS close button arrives as a viewport event, not as the old egui window's
    /// `open` flag. `show` must translate it into `DialogAction::Close`.
    fn close_requested_input(size: egui::Vec2) -> egui::RawInput {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .expect("every raw input carries the root viewport")
            .events
            .push(egui::ViewportEvent::Close);
        input
    }

    #[test]
    fn closing_the_history_window_closes_the_dialog() {
        let mut state = sample();
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let input = close_requested_input(egui::vec2(920.0, 650.0));

        let mut action = None;
        crate::app::ui::test_run(&ctx, input, |ui| {
            action = show(ui.ctx(), &mut state, &[], false);
        });

        assert!(
            matches!(action, Some(DialogAction::Close)),
            "the dialog must close when its window does"
        );
    }

    #[test]
    fn closing_the_diff_window_stops_its_preview() {
        let mut state = sample();
        state.set_diff_open(true);
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let input = close_requested_input(egui::vec2(1100.0, 650.0));

        crate::app::ui::test_run(&ctx, input, |ui| {
            let _ = show(ui.ctx(), &mut state, &[], false);
        });

        assert!(!state.diff_open, "the diff must close when its window does");
        assert!(state.pending.is_none(), "the debounce is dropped");
        assert!(!state.diff_loading, "the spinner stops");
    }

    #[test]
    fn a_prewarm_pass_keeps_the_diff_preview_idle() {
        let mut state = sample();
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(920.0, 650.0),
        ));

        // A normal run keeps the diff window warm from the first frame. That pass must
        // not look like an opened window: no preview may start before the user asks.
        crate::app::ui::test_run(&ctx, input, |ui| {
            let _ = show(ui.ctx(), &mut state, &[], true);
        });

        assert!(!state.diff_open, "prewarming must not open the window");
        assert!(state.pending.is_none(), "prewarming runs no diff");
        assert!(!state.diff_loading, "the spinner stays still");
        assert!(state.diff_model.is_none());
    }

    #[test]
    fn selecting_with_the_diff_closed_keeps_the_preview_idle() {
        let mut state = sample();
        let index = state
            .history
            .revisions
            .iter()
            .position(|node| node.revision == "1.3")
            .expect("the sample has 1.3");

        state.click(index, false);

        assert_eq!(state.selected, Some(index));
        assert!(state.pending.is_none(), "a closed diff runs nothing");
        assert!(!state.diff_loading);
        assert!(state.diff_model.is_none());
    }

    #[test]
    fn the_graph_order_keeps_the_working_selection_visible() {
        let mut state = sample();
        let visible = state.visible_indices();
        assert!(
            visible.contains(&state.selected.expect("the sample selects a revision")),
            "display order must include the selected revision: {visible:?}"
        );
        state.ensure_visible(&visible);
        assert_eq!(
            state.history.revisions[state.selected.expect("still selected")].revision,
            "1.4"
        );
    }

    #[test]
    fn selecting_another_revision_queues_a_fresh_diff_while_open() {
        let mut state = sample();
        state.set_diff_open(true);
        state.selected = Some(0);
        state.pending = None;
        state.diff_loading = false;

        let index = state
            .history
            .revisions
            .iter()
            .position(|node| node.revision == "1.3")
            .expect("the sample has 1.3");
        state.click(index, false);

        assert_eq!(state.selected, Some(index));
        assert!(
            state.pending.is_some(),
            "the preview waits for the debounce"
        );
        assert!(state.diff_loading, "the diff pane shows its loading state");
    }

    #[test]
    fn the_demo_diff_window_draws_both_complete_versions() {
        let mut state = HistoryState::demo(
            std::path::PathBuf::from("."),
            String::new(),
            "main.c".into(),
            "1.4".into(),
            sample().history,
        );
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 600.0),
        ));
        let output = crate::app::ui::test_run(&ctx, input, |ui| diff(ui, &mut state));

        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }
        for expected in [
            "main.c @ 1.4",
            "@ 1.5",
            "int main(void) { return 0; }",
            "int main(void) { return 1; }",
        ] {
            assert!(
                texts.iter().any(|text| text == expected),
                "the diff window did not draw {expected:?}: {texts:?}"
            );
        }
    }

    #[test]
    fn filtering_keeps_the_selection_when_it_is_still_visible() {
        let mut state = sample();
        let visible = state.visible_indices();
        state.ensure_visible(&visible);
        let selected = state.selected;
        state.filter = "Tidy".into();
        let visible = state.visible_indices();
        assert_eq!(visible.len(), 1);
        state.ensure_visible(&visible);
        assert_ne!(state.selected, selected, "the old selection is hidden");
        assert_eq!(
            state.history.revisions[state.selected.unwrap()].revision,
            "1.5"
        );
    }

    #[test]
    fn a_branch_filter_shows_only_that_lane() {
        let mut state = sample();
        state.branch_filter = BranchFilter::Branch("1.2.4".into());
        let visible = state.visible_indices();
        assert_eq!(visible.len(), 2);
        assert!(visible
            .iter()
            .all(|index| state.history.revisions[*index].branch));
        assert!(visible.iter().all(|index| graph::branch_id(
            &state.history.revisions[*index].revision
        )
        .as_deref()
            == Some("1.2.4")));
    }

    #[test]
    fn only_the_newest_tag_and_a_count_are_shown() {
        // `cvs log` lists tags newest first, so the first name is the newest one. Sorting
        // by name would pick `Z_oldest` here, which is the tag created first.
        let tags: Vec<String> = [
            "BSC_PD_20261126_1",
            "BSC_PD_20260820_1",
            "BSC_GK602526_1_CHENSHAOYU",
            "Z_oldest",
        ]
        .iter()
        .map(|tag| tag.to_string())
        .collect();

        assert_eq!(
            shown_tag(&tags),
            Some(("BSC_PD_20261126_1", 3)),
            "the first name is the newest tag"
        );
        assert_eq!(shown_tag(&tags[1..]), Some(("BSC_PD_20260820_1", 2)));
        assert_eq!(shown_tag(&tags[3..]), Some(("Z_oldest", 0)));
        assert_eq!(shown_tag(&[]), None);
    }

    #[test]
    fn choosing_another_revision_collapses_the_full_tag_list() {
        let mut state = sample();
        state.tags_expanded = true;
        let index = state
            .history
            .revisions
            .iter()
            .position(|node| node.revision == "1.3")
            .expect("the sample has 1.3");

        state.click(index, false);

        assert!(!state.tags_expanded, "the next revision starts compact");
    }

    #[test]
    fn the_filter_text_is_ready_with_tags_and_branch_labels() {
        let state = sample();
        let texts = search_texts(&state.history);
        let index = |revision: &str| {
            state
                .history
                .revisions
                .iter()
                .position(|node| node.revision == revision)
                .expect("the sample has the revision")
        };

        assert!(texts[index("1.4")].contains("release_1_4"));
        assert!(
            texts[index("1.2.4.1")].contains("vendor_acme"),
            "a branch lane is found by its tag too: {}",
            texts[index("1.2.4.1")]
        );
    }

    #[test]
    fn a_long_message_stops_before_the_author_column() {
        let mut state = sample();
        let index = state
            .history
            .revisions
            .iter()
            .position(|node| node.revision == "1.4")
            .expect("the sample has 1.4");
        state.history.revisions[index].message =
            "dealGroupAllBatchOperationMerged方法增加 上下文填充逻辑，改完以后需要重新跑一遍批处理验证"
                .repeat(6);
        state.history.revisions[index].author = "lumingwei".into();
        state.selected = Some(index);
        state.scroll_to_selected = false;
        let visible = state.visible_indices();

        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    list(ui, &mut state, &visible, 500.0);
                });
            },
        );
        output.textures_delta.clear();

        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(text) => out.push((
                    text.galley.text().to_string(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }

        let message = texts
            .iter()
            .find(|(text, _)| text.starts_with("dealGroupAllBatch"))
            .expect("the row paints its message");
        let author = texts
            .iter()
            .find(|(text, _)| text == "lumingwei")
            .expect("the row paints its author");
        assert!(
            message.1.right() <= author.1.left() + 0.5,
            "the message overlaps the author: message right {}, author left {}",
            message.1.right(),
            author.1.left()
        );
    }

    #[test]
    fn a_long_chinese_message_is_cut_to_the_pixels_it_may_use() {
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        // Fonts only exist after the first frame has run.
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                ..Default::default()
            },
            |_ui| {},
        );
        output.textures_delta.clear();
        let job = message_job(
            "601034-1 合并-电视续约的需求说明写得非常长，应该被省略号截断",
            egui::FontId::proportional(14.0),
            120.0,
        );
        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));

        assert!(
            galley.size().x <= 120.5,
            "the message must stay inside its column: {} px",
            galley.size().x
        );
        assert!(galley.elided, "long text ends in an ellipsis");
    }

    #[test]
    fn a_lane_span_starts_and_ends_at_the_node_in_its_row() {
        assert_eq!(lane_span(0, 0, 3), Some((false, true)));
        assert_eq!(lane_span(1, 0, 3), Some((true, true)));
        assert_eq!(lane_span(3, 0, 3), Some((true, false)));
        assert_eq!(lane_span(2, 2, 2), None);
        assert_eq!(lane_span(1, 2, 3), None);
        assert_eq!(lane_span(4, 2, 3), None);
    }

    #[test]
    fn the_fork_arc_starts_and_ends_on_the_two_lanes() {
        let arc = fork_arc(33.0, 42.0, FORK_RADIUS);
        assert_eq!(arc.first().copied(), Some(egui::pos2(33.0, 38.0)));
        assert_eq!(arc.last().copied(), Some(egui::pos2(29.0, 42.0)));
        let centre = egui::pos2(29.0, 38.0);
        for point in &arc {
            assert!(
                (point.distance(centre) - FORK_RADIUS).abs() < 0.001,
                "{point:?} is not on the quarter circle"
            );
        }
    }

    /// The vertical line segments of one colour, as (top, bottom) screen y values.
    fn verticals(output: &egui::FullOutput, color: egui::Color32) -> Vec<(f32, f32)> {
        fn walk(shape: &egui::Shape, color: egui::Color32, out: &mut Vec<(f32, f32)>) {
            match shape {
                egui::Shape::LineSegment { points, stroke }
                    if stroke.color == color && (points[0].x - points[1].x).abs() < 0.01 =>
                {
                    out.push((points[0].y.min(points[1].y), points[0].y.max(points[1].y)));
                }
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk(shape, color, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, color, &mut out);
        }
        out
    }

    fn in_row(segments: &[(f32, f32)], rect: egui::Rect) -> Option<(f32, f32)> {
        segments
            .iter()
            .copied()
            .find(|(top, bottom)| *top >= rect.top() - 0.1 && *bottom <= rect.bottom() + 0.1)
    }

    #[test]
    fn branch_lanes_connect_their_nodes_without_joining_the_next_branch() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            ..Default::default()
        };
        let trunk = [0usize, 1, 3, 6, 7];
        let branches = [(1usize, 2usize, 3usize, true), (1, 4, 6, true)];
        let mut rects = Vec::new();
        let mut output = ctx.run_ui(input, |ui| {
            for row in [0usize, 3, 4, 7] {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(200.0, ROW_H), egui::Sense::hover());
                rects.push((row, rect));
                let lane = if row == 3 || row == 4 { 1 } else { 0 };
                draw_lanes(ui.painter(), rect, row, lane, &trunk, &branches);
            }
        });
        output.textures_delta.clear();

        let green = verticals(&output, BRANCH);
        let fork = rects[1].1;
        let branch_end = in_row(&green, fork).expect("the fork row has a green lane");
        assert!(
            (branch_end.1 - (fork.center().y - FORK_RADIUS)).abs() < 0.5,
            "the lane stops where the rounded turn into the fork starts: {branch_end:?} in {fork:?}"
        );

        let next = rects[2].1;
        let branch_start = in_row(&green, next).expect("the next branch starts with a green lane");
        assert!(
            (branch_start.0 - next.center().y).abs() < 0.5,
            "the next lane starts at its own node: {branch_start:?} in {next:?}"
        );

        let blue = verticals(&output, TRUNK);
        let first = rects[0].1;
        let trunk_start = in_row(&blue, first).expect("the first trunk row has a blue lane");
        assert!(
            (trunk_start.0 - first.center().y).abs() < 0.5,
            "the trunk starts at its newest node: {trunk_start:?} in {first:?}"
        );

        let last = rects[3].1;
        let trunk_end = in_row(&blue, last).expect("the last trunk row has a blue lane");
        assert!(
            (trunk_end.1 - last.center().y).abs() < 0.5,
            "the trunk ends at its oldest node: {trunk_end:?} in {last:?}"
        );
    }

    #[test]
    fn the_painted_branch_lanes_are_continuous_within_each_branch() {
        let mut state = sample();
        state.scroll_to_selected = false;
        let visible = state.visible_indices();
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| list(ui, &mut state, &visible, 500.0));
        });
        output.textures_delta.clear();

        let mut green: Vec<(f32, f32)> = verticals(&output, BRANCH);
        green.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert!(
            green.len() >= 5,
            "the sample paints both branch runs: {green:?}"
        );
        let gaps: Vec<f32> = green.windows(2).map(|pair| pair[1].0 - pair[0].1).collect();
        let unbroken = gaps.iter().filter(|gap| **gap < 0.1).count();
        let breaks = gaps.iter().filter(|gap| **gap > 0.1).count();
        assert_eq!(
            unbroken,
            green.len() - 2,
            "the rows inside a branch touch each other: {gaps:?}"
        );
        assert_eq!(
            breaks, 1,
            "only the break between the two branches remains: {gaps:?}"
        );
    }
}
