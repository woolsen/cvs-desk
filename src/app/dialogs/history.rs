//! The File History window: one file's revisions, tags and diffs.
//!
//! The left side is the revision list with its branch gutter, the right side is the
//! selected revision, and the bottom is its diff. Selecting a row loads the diff through
//! a separate preview worker, so the main cvs command slot stays free.

use std::path::Path;
use std::time::{Duration, Instant};

use cvsdesk::entries::State;
use cvsdesk::graph;
use cvsdesk::history::History;
use cvsdesk::truncate_columns;

use super::DialogAction;
use crate::app::history_preview::{self, Loader, Msg, Target};

const SELECTED_BG: egui::Color32 = egui::Color32::from_rgb(0xE8, 0xF0, 0xFE);
const HOVER_BG: egui::Color32 = egui::Color32::from_rgb(0xF5, 0xF8, 0xFC);
const TRUNK: egui::Color32 = egui::Color32::from_rgb(0x40, 0x9E, 0xFF);
const BRANCH: egui::Color32 = egui::Color32::from_rgb(0x2F, 0x9E, 0x5F);
const REV_TEXT: egui::Color32 = egui::Color32::from_rgb(0x30, 0x90, 0xC0);
const MUTED: egui::Color32 = egui::Color32::from_gray(110);
const MESSAGE: egui::Color32 = egui::Color32::from_gray(60);
const WORKING: egui::Color32 = egui::Color32::from_rgb(0x9A, 0x65, 0x00);
const LATEST: egui::Color32 = egui::Color32::from_rgb(0x2F, 0x5F, 0xB3);
const DEAD: egui::Color32 = egui::Color32::from_rgb(0xA5, 0x0E, 0x0E);

const ROW_H: f32 = 24.0;
const GUTTER_LEFT: f32 = 14.0;
const LANE_GAP: f32 = 15.0;
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
    raw: Vec<String>,
    selected: Option<usize>,
    filter: String,
    branch_filter: BranchFilter,
    diff_target: Target,
    diff_open: bool,
    diff_lines: Vec<String>,
    diff_incoming: Vec<String>,
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

impl HistoryState {
    pub(crate) fn new(
        dir: std::path::PathBuf,
        root: String,
        file: String,
        working: String,
        working_state: State,
        history: History,
        raw: Vec<String>,
    ) -> Self {
        let selected = history.working_index(&working);
        Self {
            dir,
            root,
            file,
            working,
            working_state,
            history,
            raw,
            selected,
            filter: String::new(),
            branch_filter: BranchFilter::All,
            diff_target: Target::Previous,
            diff_open: true,
            diff_lines: Vec::new(),
            diff_incoming: Vec::new(),
            diff_error: String::new(),
            diff_loading: selected.is_some(),
            diff_revision: String::new(),
            pending: selected.map(|_| Instant::now()),
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
        raw: Vec<String>,
    ) -> Self {
        let mut state = Self::new(dir, root, file, working, State::Unchanged, history, raw);
        state.demo = true;
        state.pending = None;
        state.loader = None;
        state.diff_loading = false;
        state.diff_revision = "1.5".to_string();
        state.diff_lines = vec![
            "--- main.c\t2026/11/21 16:20:02".to_string(),
            "+++ main.c\t2026/11/22 10:04:11".to_string(),
            "@@ -1,5 +1,5 @@".to_string(),
            " /* sample */".to_string(),
            " #include <stdio.h>".to_string(),
            " ".to_string(),
            "-int main(void) { return 0; }".to_string(),
            "+int main(void) { return 1; }".to_string(),
            " ".to_string(),
        ];
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

    pub(crate) fn raw(&self) -> &[String] {
        &self.raw
    }

    pub(crate) fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
        if loading {
            self.loader = None;
            self.pending = None;
            self.diff_loading = false;
            self.diff_lines.clear();
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
        raw: Vec<String>,
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
        self.working = working;
        self.working_state = working_state;
        self.raw = raw;
        self.loading = false;
        self.error.clear();
        self.loader = None;
        self.diff_incoming.clear();
        self.diff_lines.clear();
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
        self.pending = self.selected.map(|_| Instant::now());
        self.diff_loading = self.diff_open && self.selected.is_some();
        self.scroll_to_selected = true;
    }

    fn select(&mut self, index: usize) {
        if self.selected == Some(index) {
            return;
        }
        self.selected = Some(index);
        self.loader = None;
        self.diff_incoming.clear();
        self.diff_lines.clear();
        self.diff_error.clear();
        self.diff_revision.clear();
        self.pending = Some(Instant::now());
        self.diff_loading = self.diff_open;
    }

    fn set_target(&mut self, target: Target) {
        if self.diff_target == target {
            return;
        }
        self.diff_target = target;
        self.loader = None;
        self.diff_incoming.clear();
        self.diff_lines.clear();
        self.diff_error.clear();
        self.diff_revision.clear();
        self.pending = Some(Instant::now());
        self.diff_loading = self.diff_open;
    }

    fn set_diff_open(&mut self, open: bool) {
        if self.diff_open == open {
            return;
        }
        self.diff_open = open;
        if open {
            self.pending = self.selected.map(|_| Instant::now());
            self.diff_loading = self.selected.is_some();
        } else {
            self.loader = None;
            self.pending = None;
            self.diff_loading = false;
        }
    }

    fn reload_diff(&mut self) {
        self.loader = None;
        self.diff_incoming.clear();
        self.diff_error.clear();
        self.pending = self.selected.map(|_| Instant::now());
        self.diff_loading = self.diff_open && self.selected.is_some();
    }

    fn tick(&mut self, ctx: &egui::Context, globals: &[String], flags: &[String]) {
        self.poll_loader();

        if !self.demo && self.diff_open && self.loader.is_none() {
            if let Some(at) = self.pending {
                if at.elapsed() >= PREVIEW_DELAY {
                    self.pending = None;
                    let dir = self.dir.clone();
                    let root = self.root.clone();
                    self.start_loader(&dir, &root, globals, flags);
                }
            }
        }

        if self.pending.is_some() || self.loader.is_some() || self.loading {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn start_loader(&mut self, dir: &Path, root: &str, globals: &[String], flags: &[String]) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(node) = self.history.revisions.get(index) else {
            return;
        };
        let revision = node.revision.clone();
        let previous = if self.diff_target == Target::Previous {
            self.history.previous_revision(index)
        } else {
            None
        };

        if self.diff_target == Target::Previous && previous.is_none() {
            self.diff_lines = vec!["Initial revision — no previous revision to compare.".into()];
            self.diff_error.clear();
            self.diff_loading = false;
            self.diff_revision = revision;
            return;
        }

        let args = history_preview::diff_args(
            &self.file,
            &revision,
            previous.as_deref(),
            flags,
            root,
            globals,
        );
        self.diff_incoming.clear();
        self.diff_error.clear();
        self.diff_loading = true;
        self.loader = Some(Loader::start(dir, args, revision, self.diff_target));
    }

    fn poll_loader(&mut self) {
        let Some(loader) = self.loader.as_mut() else {
            return;
        };
        let mut finished: Option<Result<i32, String>> = None;
        loop {
            match loader.rx.try_recv() {
                Ok(Msg::Line(line)) => self.diff_incoming.push(line),
                Ok(Msg::Done(code)) => {
                    finished = Some(Ok(code));
                    break;
                }
                Ok(Msg::Failed(error)) => {
                    finished = Some(Err(error));
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = Some(Err("the diff preview stopped".into()));
                    break;
                }
            }
        }

        let Some(finished) = finished else {
            return;
        };
        let loader = self.loader.take().expect("the loader was just borrowed");
        let current = self
            .selected
            .and_then(|index| self.history.revisions.get(index))
            .map(|node| node.revision.as_str());
        if current != Some(loader.revision.as_str()) || self.diff_target != loader.target {
            return;
        }

        match finished {
            Ok(code) if history_preview::success_code(code) => {
                let mut lines = std::mem::take(&mut self.diff_incoming);
                if lines.is_empty() {
                    lines.push("No changes.".to_string());
                }
                self.diff_lines = lines;
                self.diff_error.clear();
                self.diff_loading = false;
                self.diff_revision = loader.revision.clone();
            }
            Ok(code) => {
                self.diff_error = format!("cvs diff failed (exit code {})", code);
                self.diff_loading = false;
                self.diff_incoming.clear();
            }
            Err(error) => {
                self.diff_error = error;
                self.diff_loading = false;
                self.diff_incoming.clear();
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
                    && (query.is_empty() || self.search_text(node).to_lowercase().contains(&query))
            })
            .collect()
    }

    fn search_text(&self, node: &cvsdesk::graph::RevNode) -> String {
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
                if let Some(label) = self.history.branch_label(&id) {
                    text.push(' ');
                    text.push_str(label);
                }
            }
        }
        text
    }

    fn ensure_visible(&mut self, visible: &[usize]) {
        if visible.is_empty() {
            self.selected = None;
            self.loader = None;
            self.pending = None;
            self.diff_loading = false;
            self.diff_lines.clear();
            self.diff_error.clear();
            self.diff_revision.clear();
            return;
        }
        if self
            .selected
            .is_none_or(|selected| !visible.contains(&selected))
        {
            self.select(visible[0]);
            self.scroll_to_selected = true;
        }
    }
}

/// Draw the window. `dir`, `root`, the global flags and the diff flags are the pieces
/// the preview worker needs; the dialog itself only owns the revision state.
pub(super) fn show(
    ctx: &egui::Context,
    state: &mut HistoryState,
    globals: &[String],
    flags: &[String],
) -> Option<DialogAction> {
    state.tick(ctx, globals, flags);

    let mut open = true;
    let mut action = None;
    egui::Window::new(format!("File History — {}", state.file))
        .id(egui::Id::unique("file_history"))
        .open(&mut open)
        .drag_area(egui::WindowDrag::Anywhere)
        .collapsible(false)
        .resizable(true)
        .default_width(920.0)
        .default_height(650.0)
        .min_width(700.0)
        .min_height(420.0)
        .show(ctx, |ui| {
            toolbar(ui, state, &mut action);

            let visible = state.visible_indices();
            state.ensure_visible(&visible);

            // Panels keep the diff and status visible whatever the window size is, and
            // let the user drag the split between the revision list and the details.
            egui::Panel::bottom("file_history_status").show(ui, |ui| status(ui, state));
            let diff_panel = egui::Panel::bottom("file_history_diff");
            let diff_panel = if state.diff_open {
                diff_panel
                    .resizable(true)
                    .default_size(180.0)
                    .min_size(120.0)
            } else {
                diff_panel.resizable(false).exact_size(30.0)
            };
            diff_panel.show(ui, |ui| diff(ui, state));
            egui::Panel::right("file_history_details")
                .resizable(true)
                .default_size(320.0)
                .min_size(240.0)
                .show(ui, |ui| details(ui, state));
            egui::CentralPanel::default()
                .show(ui, |ui| list(ui, state, &visible, ui.available_height()));
        });

    if !open {
        action = Some(DialogAction::Close);
    }
    action
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
            ui.menu_button("More", |ui| {
                if ui.button("Raw log").clicked() {
                    *action = Some(DialogAction::HistoryRawLog);
                    ui.close();
                }
            });
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
            for &index in visible {
                let node = &state.history.revisions[index];
                let row = row_of[index];
                let lane = lane_of[index];
                let tags = state.history.tag_names(&node.revision);
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
                    clicked = Some(index);
                }
                if state.selected == Some(index) && state.scroll_to_selected {
                    state.scroll_to_selected = false;
                    response.scroll_to_me(Some(egui::Align::Center));
                }

                let painter = ui.painter_at(rect);
                if state.selected == Some(index) {
                    painter.rect_filled(rect, 0.0, SELECTED_BG);
                } else if response.hovered() {
                    painter.rect_filled(rect, 0.0, HOVER_BG);
                }

                draw_lanes(&painter, rect, row, lane, &trunk_rows, &branch_ranges);

                let mono = egui::FontId::new(11.0, egui::FontFamily::Monospace);
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
                let msg_x = rev_x + REV_W;
                let mut x = msg_x;

                if index == working_index.unwrap_or(usize::MAX) {
                    x = badge(&painter, x, y, "working", &small, WORKING);
                }
                if index == latest.unwrap_or(usize::MAX) {
                    x = badge(&painter, x, y, "latest", &small, LATEST);
                }
                if node.state == "dead" {
                    x = badge(&painter, x, y, "dead", &small, DEAD);
                }
                if first_on_branch {
                    if let Some(label) = &branch_label {
                        x = badge(&painter, x, y, label, &small, BRANCH);
                    }
                }
                for tag in tags.iter().take(2) {
                    x = badge(&painter, x, y, tag, &small, REV_TEXT);
                }

                let available = (author_right - 8.0 - x).max(20.0);
                let cell = (body.size * 0.5).max(1.0);
                let cols = (available / cell) as usize;
                painter.text(
                    egui::pos2(x.max(msg_x), y),
                    egui::Align2::LEFT_CENTER,
                    truncate_columns(&node.message, cols),
                    body.clone(),
                    MESSAGE,
                );
                painter.text(
                    egui::pos2(author_right, y),
                    egui::Align2::RIGHT_CENTER,
                    truncate_columns(&node.author, 10),
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

    if let Some(index) = clicked {
        state.select(index);
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

    if let (Some(first), Some(last)) = (trunk_rows.first(), trunk_rows.last()) {
        if row >= *first && row <= *last {
            painter.line_segment(
                [egui::pos2(x(0), top), egui::pos2(x(0), bottom)],
                egui::Stroke::new(1.0_f32, TRUNK),
            );
        }
    }
    for &(branch_lane, start, end, has_fork) in branch_ranges {
        if row >= start && row <= end {
            painter.line_segment(
                [
                    egui::pos2(x(branch_lane), top),
                    egui::pos2(x(branch_lane), bottom),
                ],
                egui::Stroke::new(1.0_f32, BRANCH),
            );
            if has_fork && row == end {
                painter.line_segment(
                    [egui::pos2(x(0), middle), egui::pos2(x(branch_lane), middle)],
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
        for tag in state.history.tag_names(&node.revision) {
            ui.label(egui::RichText::new(tag).small().color(REV_TEXT));
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
    ui.horizontal_wrapped(|ui| {
        if ui.button("Copy revision").clicked() {
            ui.ctx().copy_text(node.revision.clone());
        }
    });
}

fn diff(ui: &mut egui::Ui, state: &mut HistoryState) {
    ui.horizontal(|ui| {
        let label = if state.diff_open {
            "Hide diff"
        } else {
            "Show diff"
        };
        if ui.small_button(label).clicked() {
            state.set_diff_open(!state.diff_open);
        }
        ui.label(egui::RichText::new("Diff").strong());
        let previous_on = state.diff_target == Target::Previous;
        if ui.selectable_label(previous_on, "This revision").clicked() {
            state.set_target(Target::Previous);
        }
        let working_on = state.diff_target == Target::Working;
        if ui.selectable_label(working_on, "Working copy").clicked() {
            state.set_target(Target::Working);
        }
        if state.diff_loading {
            ui.spinner();
            ui.label(egui::RichText::new("Loading…").small().color(MUTED));
        }
        if !state.diff_error.is_empty() {
            ui.colored_label(DEAD, &state.diff_error);
        }
        if let Some(index) = state.selected {
            if let Some(node) = state.history.revisions.get(index) {
                let range = match state.diff_target {
                    Target::Previous => state
                        .history
                        .previous_revision(index)
                        .map(|previous| format!("{} → {}", previous, node.revision))
                        .unwrap_or_else(|| format!("initial → {}", node.revision)),
                    Target::Working => format!("{} → working copy", node.revision),
                };
                ui.label(egui::RichText::new(range).small().color(MUTED));
                if state.diff_target == Target::Previous {
                    if let Some((added, removed)) = node.lines {
                        ui.label(
                            egui::RichText::new(format!("+{} −{}", added, removed))
                                .small()
                                .color(MUTED),
                        );
                    }
                }
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Reload").clicked() {
                state.reload_diff();
            }
        });
    });

    if !state.diff_open {
        return;
    }

    let body = if state.selected.is_none() {
        "Select a revision.".to_string()
    } else if state.diff_lines.is_empty() {
        if state.diff_loading {
            "Loading diff…".to_string()
        } else if !state.diff_error.is_empty() {
            "No diff available.".to_string()
        } else {
            "No changes.".to_string()
        }
    } else {
        state.diff_lines.join("\n")
    };

    egui::ScrollArea::both()
        .id_salt("file_history_diff")
        .max_height(ui.available_height().max(60.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut view = body.as_str();
            ui.add(
                egui::TextEdit::multiline(&mut view)
                    .desired_width(f32::INFINITY)
                    .desired_rows(8)
                    .font(egui::TextStyle::Monospace),
            );
        });
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
                "Working copy: {} · {}",
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
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(index) = state.selected {
                if let Some(node) = state.history.revisions.get(index) {
                    if !state.diff_revision.is_empty()
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
            log,
        )
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
        assert!(state.diff_loading, "opening starts the diff preview");
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
    fn selecting_another_revision_queues_a_fresh_diff() {
        let mut state = sample();
        state.selected = Some(0);
        state.pending = None;
        state.diff_loading = false;

        let index = state
            .history
            .revisions
            .iter()
            .position(|node| node.revision == "1.3")
            .expect("the sample has 1.3");
        state.select(index);

        assert_eq!(state.selected, Some(index));
        assert!(
            state.pending.is_some(),
            "the preview waits for the debounce"
        );
        assert!(state.diff_loading, "the diff pane shows its loading state");
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
}
