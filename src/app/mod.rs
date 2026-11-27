//! The window's state and lifetime: the folder it shows, the command it runs and the
//! worker threads that feed both.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::Instant;

use cvsdesk::cvs::{Msg, Runner};
use cvsdesk::demo;
use cvsdesk::entries::{self, Row, State};
use cvsdesk::prefs::Prefs;

use crate::app::dialogs::history::HistoryState;
use crate::app::dialogs::tags::TagsState;
use crate::app::dialogs::Dialog;
use crate::app::files::{Filters, SortKey};
use crate::app::shot::ShotJob;

mod actions;
mod dialogs;
mod files;
mod history_preview;
mod nav;
mod remote;
mod shot;
mod trash;
mod ui;

#[cfg(test)]
mod tests;

/// Which pane at the bottom shows what.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum OutputKind {
    Console,
    State,
    Log,
    Diff,
    Status,
    Annotate,
    History,
    Editors,
}

/// How much of the repository's own folder listing the window knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RemoteState {
    Off,
    Loading,
    Ready,
    Stopped,
    Failed,
}

impl OutputKind {
    fn title(self) -> &'static str {
        match self {
            OutputKind::Console => "Console",
            OutputKind::State => "File states",
            OutputKind::Log => "Log",
            OutputKind::Diff => "Diff",
            OutputKind::Status => "Status",
            OutputKind::Annotate => "Annotate",
            OutputKind::History => "Repository History",
            OutputKind::Editors => "Editors & Watchers",
        }
    }
}

/// What the Tags window is waiting for when a cvs command finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TagWait {
    /// A `cvs log -h` reply: fill or refresh the window's list.
    Query,
    /// A create or delete command: when it ends, read the tags again.
    AfterWrite,
}

/// The file whose log reply will fill the File History window.
pub(super) struct HistoryWait {
    pub dir: PathBuf,
    pub root: String,
    pub file: String,
    pub working: String,
    pub working_state: State,
    /// True when an open File History window asked for a refresh.
    pub refresh: bool,
}

/// Something that stopped a command, kept so the status bar can show it.
#[derive(Clone, PartialEq)]
enum Trouble {
    /// A command was asked for while one was already running.
    Busy,
    /// cvs exited with a code other than zero.
    Exit(i32),
    /// Something outside a cvs command failed, such as writing the settings file.
    Message(String),
}

impl Trouble {
    fn text(&self) -> String {
        match self {
            Trouble::Busy => "Error : cvs is already running !".to_string(),
            Trouble::Exit(code) => {
                format!("cvs returned an non-zero error code ({})", code)
            }
            Trouble::Message(text) => text.clone(),
        }
    }
}

pub(crate) struct App {
    dir: PathBuf,
    /// What the address bar shows. Kept in step with `dir`, and editable by the user.
    address: String,
    /// True while the address bar is a text field taking the keyboard; false while it
    /// shows the readable tail of the current path.
    address_editing: bool,
    /// A click on the address label asks the field to take the keyboard. The request is
    /// made on the frame that draws the field, not on the click frame: AccessKit is given
    /// the focused id at the end of every frame, and a focus id that the same frame never
    /// drew is a contract violation its consumer panics on.
    focus_address: bool,
    /// The folders visited, so Back and Forward can walk them.
    history: nav::History,
    rows: Vec<Row>,
    notes: Vec<String>,
    has_cvs: bool,
    root: String,
    repository: String,
    /// `CVS/Entries.Static` is present: this folder was checked out without all of its
    /// repository files. It is context, not an error.
    partial_checkout: bool,
    selected: HashSet<usize>,
    /// The row a Shift-click extends from: the last row picked by a plain or Ctrl
    /// click. Cleared when the selection is cleared or the folder is read again.
    anchor: Option<usize>,
    filters: Filters,
    /// True while the filter bar above the list is unfolded. The bar starts folded
    /// into the funnel button at the right of the nav bar, so it only takes a row
    /// when the user asks for it.
    filters_open: bool,
    /// What the search box above the list is looking for. Empty shows everything.
    search: String,
    /// True while the search field is open in the nav bar. Closing it clears `search`,
    /// so a collapsed box never hides a filter.
    search_open: bool,
    /// Set by the View menu, so the next frame hands the keyboard to the
    /// search box even though the request came before the box was drawn.
    focus_search: bool,
    sort: SortKey,
    sort_asc: bool,
    /// The display order: indices into `rows`, filtered and sorted. Rebuilt only when
    /// `order_dirty` says the rows, filters or sort column changed - not on every frame.
    order: Vec<usize>,
    order_dirty: bool,
    /// The per-state counts shown in the status bar, computed when the rows change.
    counts: Vec<(State, usize)>,
    /// The File states sheet is only formatted when that pane is actually shown.
    state_dirty: bool,
    runner: Runner,
    rx: Option<Receiver<Msg>>,
    /// A folder read running on a worker thread: the path it is reading and the channel
    /// that carries the finished rows back. Reading 15,000 entries must not freeze the window.
    load_rx: Option<Receiver<(PathBuf, entries::Loaded)>>,
    loading: Option<PathBuf>,
    /// The repository-name listing running beside the folder read.
    remote: Option<remote::Loader>,
    remote_state: RemoteState,
    /// Tests keep the network out; the window turns this on.
    remote_enabled: bool,
    remote_seq: u64,
    remote_lines: Vec<String>,
    /// Reads the running listing's lines as they arrive, so names can appear before
    /// `rlog -R` has walked the whole folder.
    remote_parser: Option<cvsdesk::remote::Parser>,
    /// Names the parser found that the list has not taken yet.
    remote_pending: Vec<cvsdesk::remote::Found>,
    /// When the pending names were last merged, so a fast server cannot rebuild the
    /// list on every line.
    remote_last_merge: Instant,
    /// Direct repository names found so far, for the progress label.
    remote_found: usize,
    /// The last complete listing for each folder, kept for this run of the window.
    remote_cache: remote::Cache,
    /// The row at the top of the list and the pixels of it above the viewport. A batch
    /// of names restores this row instead of moving what the reader is looking at.
    list_anchor: Option<(String, f32)>,
    /// Set when the next frame must restore `list_anchor`.
    pending_scroll: Option<(String, f32)>,
    /// True while the pointer is down or the wheel is moving. Name merges wait for a
    /// quiet frame, so the list never changes under an active pointer.
    list_interacting: bool,
    /// True when the read in flight was started to show a command's changes in place:
    /// the rows on screen stay until the new ones arrive, and the selection follows its
    /// names instead of being cleared.
    refresh_in_place: bool,
    /// True when the running command can change the working copy. The folder is read
    /// again only then; a query such as Log or Status leaves the list untouched.
    refresh_after_command: bool,
    /// The folder a checkout is expected to create. cvs succeeds, it is shown; any
    /// other command clears it, so an old value can never open the wrong folder.
    checkout_waiting: Option<PathBuf>,
    pending: Vec<String>,
    captured: Vec<String>,
    /// The last finished reply of each pane that keeps one. Switching tabs shows what
    /// that pane last reported instead of the newest command's reply.
    pane_output: HashMap<OutputKind, Vec<String>>,
    /// The pane the running command reports into. While it runs, only that pane follows
    /// its output.
    running_kind: Option<OutputKind>,
    /// The file whose `cvs log` was asked for the File History window. When the command
    /// finishes, the captured log is parsed and the window opens or refreshes.
    history_waiting: Option<HistoryWait>,
    /// What the Tags window asked cvs for. Cleared when the window closes.
    tag_waiting: Option<TagWait>,
    output: OutputKind,
    /// What the reader selected in the output pane in the last frame. Copy takes it when
    /// it is there, and while it is there the console stops following new output.
    output_selection: Option<String>,
    prefs: Prefs,
    dialog: Option<Dialog>,
    status: String,
    /// The last thing that went wrong, shown in the status bar until the next command.
    /// The console reports these; this also keeps the last one in view.
    trouble: Option<Trouble>,
    /// Set when the user stops a running command, so its killed exit code is not
    /// reported as an error.
    stop_requested: bool,
    state_sheet: String,
    shot: Option<ShotJob>,
}

impl App {
    pub(crate) fn new(
        cc: &eframe::CreationContext<'_>,
        start: Option<PathBuf>,
        shot: Option<String>,
        show_history: bool,
        show_prefs: bool,
        show_tags: bool,
        show_about: bool,
    ) -> Self {
        ui::fonts::install(&cc.egui_ctx);

        // egui keeps a style per theme and chooses one from the system theme after
        // creation, so the frame must be written into both or half of it falls back
        // to egui's defaults.
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(6.0, 4.0);
            style.spacing.button_padding = egui::vec2(6.0, 3.0);
            style.visuals = egui::Visuals::light();
        });
        ui::typography::install(&cc.egui_ctx);

        let mut prefs = Prefs::load().unwrap_or_default();
        // A folder named on the command line wins; without one, reopen the folder the
        // window showed when it last closed; without that, the sample folder as before.
        let dir = start
            .or_else(|| prefs.last_folder())
            .unwrap_or_else(demo::ensure);
        prefs.last_dir = dir.display().to_string();
        let filters = Filters::from_view(&prefs.view);
        let sort = SortKey::from_token(&prefs.view.sort);
        let sort_asc = prefs.view.sort_ascending;

        let mut app = Self {
            address: dir.display().to_string(),
            address_editing: false,
            focus_address: false,
            history: nav::History::new(dir.clone()),
            dir: dir.clone(),
            rows: Vec::new(),
            notes: Vec::new(),
            has_cvs: false,
            root: String::new(),
            repository: String::new(),
            partial_checkout: false,
            selected: HashSet::new(),
            anchor: None,
            filters,
            filters_open: false,
            search: String::new(),
            search_open: false,
            focus_search: false,
            sort,
            sort_asc,
            order: Vec::new(),
            order_dirty: true,
            counts: Vec::new(),
            state_dirty: true,
            runner: Runner::default(),
            rx: None,
            load_rx: None,
            loading: None,
            remote: None,
            remote_state: RemoteState::Off,
            remote_enabled: true,
            remote_seq: 0,
            remote_lines: Vec::new(),
            remote_parser: None,
            remote_pending: Vec::new(),
            remote_last_merge: Instant::now(),
            remote_found: 0,
            remote_cache: remote::Cache::default(),
            list_anchor: None,
            pending_scroll: None,
            list_interacting: false,
            refresh_in_place: false,
            refresh_after_command: false,
            checkout_waiting: None,
            pending: Vec::new(),
            captured: Vec::new(),
            pane_output: HashMap::new(),
            running_kind: None,
            history_waiting: None,
            tag_waiting: None,
            output: OutputKind::Console,
            output_selection: None,
            prefs,
            dialog: None,
            status: String::new(),
            trouble: None,
            stop_requested: false,
            state_sheet: String::new(),
            shot: shot.map(|p| ShotJob {
                path: PathBuf::from(p),
                frames: 0,
                requested: false,
                image: Default::default(),
            }),
        };
        app.reload();
        app.runner.lines.push("CVS Desk ready.".into());
        app.runner
            .lines
            .push(format!("Working folder: {}", app.dir.display()));
        if show_history {
            let log: Vec<String> = demo::SAMPLE_LOG.lines().map(|l| l.to_string()).collect();
            let history = cvsdesk::history::History::parse("main.c", &log);
            app.dialog = Some(Dialog::History(Box::new(HistoryState::demo(
                app.dir.clone(),
                app.root.clone(),
                "main.c".into(),
                "1.5".into(),
                history,
                log,
            ))));
            app.status = "File History of a saved cvs log reply".into();
        }
        if show_prefs {
            app.dialog = Some(Dialog::Preferences {
                tab: 0,
                draft: Box::new(app.prefs.clone()),
            });
        }
        if show_tags {
            let log: Vec<String> = demo::SAMPLE_TAGS_LOG
                .lines()
                .map(|line| line.to_string())
                .collect();
            app.dialog = Some(Dialog::Tags(Box::new(TagsState {
                scope: Vec::new(),
                scope_label: "sample file main.c".into(),
                tags: cvsdesk::tags::parse_log(&log),
                name: String::new(),
                branch: false,
                selected: None,
                confirm: None,
                loading: false,
                focus_name: false,
                error: String::new(),
                status: "Tags of a saved cvs log reply".into(),
            })));
            app.status = "Tags of a saved cvs log reply".into();
        }
        if show_about {
            app.dialog = Some(Dialog::About);
        }
        app
    }

    // -- reading the folder -------------------------------------------------

    /// Read another folder from scratch. Only a folder change and an explicit Reload use
    /// this: the old rows go first so they can never be shown under the new address.
    fn reload(&mut self) {
        self.cancel_remote();
        self.rows.clear();
        self.notes.clear();
        // The old root must never label the folder being read; it comes back with the
        // rows when the read finishes.
        self.root.clear();
        self.repository.clear();
        self.partial_checkout = false;
        self.selected.clear();
        self.anchor = None;
        self.order.clear();
        self.order_dirty = true;
        self.counts.clear();
        self.state_sheet.clear();
        self.state_dirty = true;
        self.status = "Reading folder…".into();
        self.read_folder(false);
    }

    /// Read the folder again after a command changed it. Unlike `reload`, the rows on
    /// screen stay while the read runs and the selection follows its names into the new
    /// list, so a command never looks like a folder change: no blank list, no jump to the
    /// top, no lost selection.
    fn refresh(&mut self) {
        self.read_folder(true);
    }

    /// Start reading the working folder on a worker thread. The window stays responsive;
    /// `poll_load` puts the result on screen when it arrives.
    fn read_folder(&mut self, in_place: bool) {
        let dir = self.dir.clone();
        self.refresh_in_place = in_place;
        let (tx, rx) = channel();
        self.load_rx = Some(rx);
        self.loading = Some(dir.clone());
        std::thread::spawn(move || {
            let loaded = entries::load(&dir);
            let _ = tx.send((dir, loaded));
        });
    }

    /// Take a finished folder read, unless the user already moved to another folder.
    fn apply_load_result(&mut self, dir: PathBuf, loaded: entries::Loaded) {
        if dir != self.dir {
            return;
        }
        self.apply_loaded(loaded);
    }

    /// The same for a read that was meant to show a command's changes in place.
    fn apply_refresh_result(&mut self, dir: PathBuf, loaded: entries::Loaded) {
        if dir != self.dir {
            return;
        }
        let selected: HashSet<String> = self
            .selected
            .iter()
            .filter_map(|index| self.rows.get(*index))
            .map(|row| row.name.clone())
            .collect();
        let anchor = self
            .anchor
            .and_then(|index| self.rows.get(index))
            .map(|row| row.name.clone());

        self.install_loaded(loaded);

        self.selected = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| selected.contains(&row.name))
            .map(|(index, _)| index)
            .collect();
        self.anchor = anchor.and_then(|name| self.rows.iter().position(|row| row.name == name));
    }

    fn poll_load(&mut self) {
        let event = self.load_rx.as_ref().map(|rx| rx.try_recv());
        match event {
            Some(Ok((dir, loaded))) => {
                self.load_rx = None;
                if std::mem::take(&mut self.refresh_in_place) {
                    self.apply_refresh_result(dir, loaded);
                } else {
                    self.apply_load_result(dir, loaded);
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.load_rx = None;
                self.loading = None;
                self.refresh_in_place = false;
                self.status = "Could not read this folder".into();
            }
            _ => {}
        }
    }

    /// Put a fresh folder read on screen and drop what the old folder had selected.
    fn apply_loaded(&mut self, loaded: entries::Loaded) {
        self.install_loaded(loaded);
        self.selected.clear();
        self.anchor = None;
    }

    /// Put a folder read on screen. The one place rows, notes and counts are replaced, so
    /// the cached display order and the File states sheet are invalidated together.
    fn install_loaded(&mut self, loaded: entries::Loaded) {
        self.loading = None;
        self.rows = loaded.rows;
        self.notes = loaded.notes;
        self.has_cvs = loaded.has_cvs;
        self.root = loaded.root;
        self.repository = loaded.repository;
        self.partial_checkout = loaded.partial_checkout;
        self.order_dirty = true;
        self.state_dirty = true;
        self.counts = self.count_states();
        for n in &self.notes {
            if !self.runner.lines.iter().any(|l| l == n) {
                self.runner.lines.push(n.clone());
            }
        }
        self.status = format!(
            "{} file(s) in this folder",
            self.rows.iter().filter(|r| !r.is_dir).count()
        );
        self.start_remote_listing();
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.shot_tick(ctx);
        if self.loading.is_some() || self.remote.is_some() || self.runner.running {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.ui_frame(ui, frame);
    }

    /// Keep the folder on screen for the next start. A photograph is taken by a check,
    /// not by the user, so a `--screenshot` run must not decide what opens next time.
    fn on_exit(&mut self) {
        if self.shot.is_none() {
            self.sync_view_prefs();
            let _ = Prefs::remember_last_dir(&self.dir);
            let _ = self.prefs.remember_view();
            let _ = self.prefs.remember_roots();
        }
    }
}

/// The system fonts the window would use, for `--font-info`.
pub(crate) fn font_report() -> Vec<String> {
    ui::fonts::report()
}
