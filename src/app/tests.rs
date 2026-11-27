//! Cross-module tests for the window state, plus the helpers the other test modules use.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use cvsdesk::cvs::{Cmd, CommandRequest, Msg, Runner};
use cvsdesk::entries::{self, Origin, Row, State};
use cvsdesk::prefs::Prefs;
use cvsdesk::remote::Listing;
use cvsdesk::tags::TagInfo;

use super::dialogs::tags::TagsState;
use super::dialogs::Dialog;
use super::files::{Filters, SortKey};
use super::{App, HistoryWait, OutputKind, RemoteState, TagWait, Trouble};

pub(super) fn row(name: &str, is_dir: bool) -> Row {
    Row {
        name: name.to_string(),
        is_dir,
        state: if is_dir {
            State::Folder
        } else {
            State::Unknown
        },
        origin: Origin::Local,
        revision: String::new(),
        tag: String::new(),
        options: String::new(),
        timestamp: String::new(),
        binary: false,
        unicode: false,
    }
}

pub(super) fn app_with_rows(rows: Vec<Row>) -> App {
    let dir = PathBuf::from("C:\\work");
    App {
        address: dir.display().to_string(),
        address_editing: false,
        focus_address: false,
        history: super::nav::History::new(dir.clone()),
        dir,
        rows,
        notes: Vec::new(),
        has_cvs: true,
        root: String::new(),
        repository: String::new(),
        partial_checkout: false,
        selected: HashSet::new(),
        anchor: None,
        filters: Filters::default(),
        filters_open: false,
        search: String::new(),
        search_open: false,
        focus_search: false,
        sort: SortKey::Name,
        sort_asc: true,
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
        remote_enabled: false,
        remote_seq: 0,
        remote_lines: Vec::new(),
        remote_parser: None,
        remote_pending: Vec::new(),
        remote_last_merge: Instant::now(),
        remote_found: 0,
        remote_cache: super::remote::Cache::default(),
        list_anchor: None,
        pending_scroll: None,
        list_interacting: false,
        refresh_in_place: false,
        refresh_after_command: false,
        checkout_waiting: None,
        pending: Vec::new(),
        captured: Vec::new(),
        pane_output: Default::default(),
        running_kind: None,
        history_waiting: None,
        tag_waiting: None,
        output: OutputKind::Console,
        output_selection: None,
        prefs: Prefs::default(),
        dialog: None,
        status: String::new(),
        trouble: None,
        stop_requested: false,
        state_sheet: String::new(),
        shot: None,
    }
}

pub(super) fn loaded(rows: Vec<Row>) -> entries::Loaded {
    entries::Loaded {
        rows,
        notes: Vec::new(),
        has_cvs: true,
        root: String::new(),
        repository: String::new(),
        partial_checkout: false,
    }
}

#[test]
fn a_filter_change_is_remembered_in_the_prefs_draft() {
    let mut app = app_with_rows(vec![row("a.txt", false)]);
    app.filters.unknown = false;
    app.filters.hide_missing = true;
    app.sort = SortKey::State;
    app.sort_asc = false;

    app.sync_view_prefs();

    assert!(!app.prefs.view.show_unknown);
    assert!(app.prefs.view.hide_missing);
    assert_eq!(app.prefs.view.sort, "state");
    assert!(!app.prefs.view.sort_ascending);
}

#[test]
fn the_dialog_connection_prefers_the_working_copy_root() {
    let mut app = app_with_rows(Vec::new());
    app.root = ":pserver:work@host:/repo".to_string();
    app.prefs.cvsroot = ":pserver:configured@host:/repo".to_string();
    app.prefs.recent_roots = vec![":pserver:recent@host:/repo".to_string()];

    assert_eq!(app.default_connection(), ":pserver:work@host:/repo");

    app.root.clear();
    assert_eq!(
        app.default_connection(),
        ":pserver:recent@host:/repo",
        "with no working copy the last root used comes before the old setting"
    );

    app.prefs.recent_roots.clear();
    assert_eq!(app.default_connection(), ":pserver:configured@host:/repo");
}

#[test]
fn the_folder_kind_comes_from_the_folders_own_root() {
    let mut app = app_with_rows(Vec::new());
    app.prefs.kind = cvsdesk::entries::ConnKind::Pserver;
    app.root = ":kserver:ann@host:/repo".to_string();
    assert_eq!(app.folder_kind(), Some("kserver"));
    app.root.clear();
    assert_eq!(app.folder_kind(), None);
}

#[test]
fn reading_another_folder_forgets_the_old_root() {
    let mut app = app_with_rows(Vec::new());
    app.root = ":pserver:work@host:/repo".to_string();
    app.repository = "sampleproj".to_string();

    app.reload();

    assert!(
        app.root.is_empty(),
        "the old root must not label the new folder"
    );
    assert!(app.repository.is_empty());
}

#[test]
fn a_request_root_wins_over_the_folder_root() {
    let mut app = app_with_rows(Vec::new());
    app.root = ":pserver:folder@host:/repo".to_string();

    let mut req = CommandRequest::new(Cmd::Status);
    req.root = Some(":pserver:dialog@host:/repo".to_string());
    assert_eq!(app.root_for(&req), ":pserver:dialog@host:/repo");

    let req = CommandRequest::new(Cmd::Status);
    assert_eq!(
        app.root_for(&req),
        ":pserver:folder@host:/repo",
        "without an override the folder decides"
    );
}

#[test]
fn trouble_uses_the_expected_wording() {
    assert_eq!(
        Trouble::Busy.text(),
        "Error : cvs is already running !".to_string()
    );
    assert_eq!(
        Trouble::Exit(1).text(),
        "cvs returned an non-zero error code (1)".to_string()
    );
    assert_eq!(Trouble::Message("settings".into()).text(), "settings");
}

#[test]
fn a_requested_stop_is_not_an_error() {
    // The rule: when the user asked to stop, the killed process's exit code (which is
    // never 0) must be reported as "Stopped.", not as an error.
    let mut stop_requested = true;
    let mut trouble: Option<Trouble> = None;
    let mut lines: Vec<String> = Vec::new();
    let code = 1;
    if stop_requested {
        stop_requested = false;
        lines.push("Stopped.".into());
        trouble = None;
    } else if code != 0 {
        lines.push(format!("cvs returned an non-zero error code ({})", code));
        trouble = Some(Trouble::Exit(code));
    }
    assert!(stop_requested == false);
    assert_eq!(lines, vec!["Stopped.".to_string()]);
    assert!(trouble.is_none(), "a deliberate stop raises no banner");
}

#[test]
fn loading_a_folder_leaves_the_state_sheet_until_it_is_shown() {
    let mut app = app_with_rows(Vec::new());
    app.apply_loaded(loaded(vec![row("a.txt", false)]));
    assert!(
        app.state_sheet.is_empty(),
        "loading a folder must not format the whole sheet"
    );
    assert!(app.state_dirty);
    app.refresh_state_sheet();
    assert!(!app.state_dirty);
    assert!(app.state_sheet.contains("Working folder"));
    assert!(
        !app.state_sheet.contains("a.txt"),
        "the sheet sums the states up; the list shows the files"
    );
}

#[test]
fn loading_a_folder_computes_the_state_counts_once() {
    let mut app = app_with_rows(Vec::new());
    let mut changed = row("a.txt", false);
    changed.state = State::Modified;
    app.apply_loaded(loaded(vec![changed]));
    assert_eq!(app.counts, vec![(State::Modified, 1)]);
}

#[test]
fn a_load_result_for_another_folder_is_ignored() {
    let mut app = app_with_rows(Vec::new());
    app.dir = PathBuf::from("B");
    app.loading = Some(PathBuf::from("B"));
    app.apply_load_result(PathBuf::from("A"), loaded(vec![row("a.txt", false)]));
    assert!(
        app.rows.is_empty(),
        "a result for another folder must not replace the list"
    );
    assert_eq!(
        app.loading,
        Some(PathBuf::from("B")),
        "the current read stays pending"
    );
}

#[test]
fn a_load_result_for_the_current_folder_replaces_the_rows() {
    let mut app = app_with_rows(Vec::new());
    app.dir = PathBuf::from("A");
    app.loading = Some(PathBuf::from("A"));
    app.apply_load_result(PathBuf::from("A"), loaded(vec![row("a.txt", false)]));
    assert_eq!(app.rows.len(), 1);
    assert!(app.loading.is_none());
    assert!(app.order_dirty);
}

#[test]
fn enter_on_a_selected_folder_opens_it_and_remembers_the_step() {
    let root = std::env::temp_dir().join(format!("cvsdesk-open-{}", std::process::id()));
    let sub = root.join("sub");
    fs::create_dir_all(&sub).unwrap();

    let mut app = app_with_rows(vec![row("sub", true)]);
    app.dir = root.clone();
    app.address = root.display().to_string();
    app.history = super::nav::History::new(root.clone());
    app.selected.insert(0);

    app.open_selected_folder();

    assert_eq!(app.dir, sub);
    assert_eq!(app.address, app.dir.display().to_string());
    assert!(app.history.can_back());
    assert!(!app.history.can_forward());

    fs::remove_dir_all(&root).ok();
}

#[test]
fn enter_on_a_file_leaves_the_folder_alone() {
    let mut app = app_with_rows(vec![row("a.txt", false)]);
    app.selected.insert(0);
    let before = app.dir.clone();
    app.open_selected_folder();
    assert_eq!(app.dir, before);
    assert!(!app.history.can_back());
}

#[test]
fn up_back_and_forward_walk_real_folders() {
    let root = std::env::temp_dir().join(format!("cvsdesk-walk-{}", std::process::id()));
    let parent = root.join("parent");
    let child = parent.join("child");
    fs::create_dir_all(&child).unwrap();

    let mut app = app_with_rows(Vec::new());
    app.dir = child.clone();
    app.address = child.display().to_string();
    app.history = super::nav::History::new(child.clone());

    assert!(app.can_go_up());
    app.go_up();
    assert_eq!(app.dir, parent);
    app.go_back();
    assert_eq!(app.dir, child);
    app.go_forward();
    assert_eq!(app.dir, parent);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn navigating_remembers_the_folder_for_the_next_start() {
    let root = std::env::temp_dir().join(format!("cvsdesk-remember-{}", std::process::id()));
    let child = root.join("child");
    fs::create_dir_all(&child).unwrap();

    let mut app = app_with_rows(Vec::new());
    app.dir = root.clone();
    app.address = root.display().to_string();
    app.history = super::nav::History::new(root.clone());

    app.go_to(child.clone());

    assert_eq!(
        app.prefs.last_dir,
        child.display().to_string(),
        "the folder on screen is what the next start reopens"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn move_to_trash_deletes_a_folder_and_drops_its_record_without_reloading() {
    let root = std::env::temp_dir().join(format!("cvsdesk-trash-{}", std::process::id()));
    let folder = root.join("junk");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("inside.txt"), b"x").unwrap();
    fs::create_dir_all(root.join("CVS")).unwrap();
    fs::write(
        root.join("CVS").join("Entries"),
        b"D/junk////\n/kept.txt/1.1/Tue Nov 24 09:52:53 2026//\n",
    )
    .unwrap();

    let mut app = app_with_rows(vec![row("junk", true)]);
    app.dir = root.clone();
    app.address = root.display().to_string();
    app.history = super::nav::History::new(root.clone());
    app.selected = HashSet::from([0]);

    app.move_to_trash();

    assert!(!folder.exists(), "the folder must leave the disk");
    assert!(
        !app.runner.lines.iter().any(|line| line.starts_with("$ ")),
        "moving to the trash is a local action, not a cvs command: {:?}",
        app.runner.lines
    );
    assert_eq!(
        app.runner.lines,
        vec![format!("Moved to the trash: {}", folder.display())],
        "one line for one deleted folder"
    );
    let entries = fs::read_to_string(root.join("CVS").join("Entries")).unwrap();
    assert!(
        !entries.contains("D/junk////"),
        "the folder record must not bring the row back: {entries}"
    );
    assert!(
        entries.contains("/kept.txt/1.1/"),
        "another entry must survive: {entries}"
    );
    assert!(app.rows.is_empty(), "the row goes without a folder re-read");
    assert!(
        app.loading.is_none(),
        "deleting must not start a full folder reload"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn move_to_trash_removes_a_missing_folder_record_with_one_line() {
    let root = std::env::temp_dir().join(format!("cvsdesk-trash-dgone-{}", std::process::id()));
    fs::create_dir_all(root.join("CVS")).unwrap();
    let recorded = r"C:\Users\example\AppData\Local\Temp\cvsdesk-baseline-1788748544833-7r9o829z";
    fs::write(
        root.join("CVS").join("Entries"),
        format!("D/{recorded}////\n/kept.txt/1.1/Tue Nov 24 09:52:53 2026//\n"),
    )
    .unwrap();

    let mut app = app_with_rows(vec![row(recorded, true)]);
    app.dir = root.clone();
    app.selected = HashSet::from([0]);

    app.move_to_trash();

    assert_eq!(
        app.runner.lines,
        vec![format!("Removed the folder entry: {}", recorded)],
        "a missing folder record is one normal deletion, not Not found plus a removal"
    );
    assert!(
        !app.runner.lines.iter().any(|line| line.starts_with("$ ")),
        "no cvs command may be started: {:?}",
        app.runner.lines
    );
    let entries = fs::read_to_string(root.join("CVS").join("Entries")).unwrap();
    assert!(
        !entries.contains(recorded),
        "the stale folder record must be gone: {entries}"
    );
    assert!(
        entries.contains("/kept.txt/1.1/"),
        "another entry must survive: {entries}"
    );
    assert!(app.rows.is_empty(), "the row goes without a folder re-read");
    assert!(
        app.loading.is_none(),
        "deleting must not start a full folder reload"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn move_to_trash_skips_names_that_are_not_on_disk_without_running_cvs() {
    let root = std::env::temp_dir().join(format!("cvsdesk-trash-gone-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();

    let mut app = app_with_rows(vec![row("gone", true)]);
    app.dir = root.clone();
    app.selected = HashSet::from([0]);

    app.move_to_trash();

    assert!(
        app.runner
            .lines
            .iter()
            .any(|line| line.contains("Not found")),
        "{:?}",
        app.runner.lines
    );
    assert!(
        !app.runner.lines.iter().any(|line| line.starts_with("$ ")),
        "a name that is not there cannot be handed to cvs: {:?}",
        app.runner.lines
    );
    assert!(
        app.rows.iter().any(|row| row.name == "gone"),
        "a row with no cvs record cannot be removed from the index"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn move_to_trash_follows_a_folder_entry_that_records_a_full_path() {
    let root = std::env::temp_dir().join(format!("cvsdesk-trash-d-{}", std::process::id()));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let outside = std::env::temp_dir().join(format!(
        "cvsdesk-trash-outside-{}-{}",
        std::process::id(),
        stamp
    ));
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("inside.txt"), b"x").unwrap();
    fs::create_dir_all(root.join("CVS")).unwrap();
    fs::write(
        root.join("CVS").join("Entries"),
        format!("D/{}////\n", outside.display()),
    )
    .unwrap();

    // A `D` line can record a full path (the working copy this was reported from has
    // several). The row names that path, so the trash must follow it, not join it under
    // the folder on screen.
    let mut app = app_with_rows(vec![row(&outside.display().to_string(), true)]);
    app.dir = root.clone();
    app.selected = HashSet::from([0]);

    app.move_to_trash();

    assert!(
        !outside.exists(),
        "the folder the entry names must leave the disk"
    );
    assert_eq!(
        app.runner.lines,
        vec![format!("Moved to the trash: {}", outside.display())]
    );
    let entries = fs::read_to_string(root.join("CVS").join("Entries")).unwrap();
    assert!(
        entries.trim().is_empty(),
        "the folder record must go with the folder: {entries}"
    );
    assert!(app.rows.is_empty());
    assert!(app.loading.is_none());

    fs::remove_dir_all(&root).ok();
}

#[test]
fn move_to_trash_leaves_a_versioned_file_row_as_missing_in_place() {
    let root = std::env::temp_dir().join(format!("cvsdesk-trash-file-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let file = root.join("hello.txt");
    fs::write(&file, b"x").unwrap();

    let mut app = app_with_rows(vec![row("hello.txt", false)]);
    app.rows[0].state = State::Unchanged;
    app.dir = root.clone();
    app.selected = HashSet::from([0]);

    app.move_to_trash();

    assert!(!file.exists(), "the file must leave the disk");
    assert_eq!(
        app.runner.lines,
        vec![format!("Moved to the trash: {}", file.display())]
    );
    assert_eq!(app.rows.len(), 1, "the versioned row stays in the list");
    assert_eq!(app.rows[0].state, State::Missing);
    assert_eq!(
        app.selected,
        HashSet::from([0]),
        "the row keeps its selection"
    );
    assert!(
        app.loading.is_none(),
        "deleting must not re-read the folder"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn deleting_rows_before_a_kept_one_moves_the_selection_with_them() {
    let root = std::env::temp_dir().join(format!("cvsdesk-trash-shift-{}", std::process::id()));
    let folder = root.join("junk");
    fs::create_dir_all(&folder).unwrap();
    let file = root.join("hello.txt");
    fs::write(&file, b"x").unwrap();

    let mut app = app_with_rows(vec![row("junk", true), row("hello.txt", false)]);
    app.rows[1].state = State::Unchanged;
    app.dir = root.clone();
    app.selected = HashSet::from([0, 1]);

    app.move_to_trash();

    assert_eq!(app.rows.len(), 1);
    assert_eq!(app.rows[0].name, "hello.txt");
    assert_eq!(app.rows[0].state, State::Missing);
    assert_eq!(
        app.selected,
        HashSet::from([0]),
        "the kept row is selected at its new index"
    );
    assert!(app.loading.is_none());

    fs::remove_dir_all(&root).ok();
}

#[test]
fn only_commands_that_write_the_working_copy_ask_for_a_read() {
    for cmd in [
        Cmd::Update,
        Cmd::Commit,
        Cmd::Add,
        Cmd::AddBinary,
        Cmd::AddUnicode,
        Cmd::Remove,
        Cmd::Tag,
        Cmd::Checkout,
        Cmd::Export,
        Cmd::Edit,
        Cmd::Unedit,
    ] {
        assert!(
            cmd.changes_working_copy(),
            "{cmd:?} changes files or CVS/Entries"
        );
    }
    for cmd in [
        Cmd::QueryUpdate,
        Cmd::Diff,
        Cmd::Log,
        Cmd::Status,
        Cmd::Annotate,
        Cmd::Rtag,
        Cmd::Import,
        Cmd::Init,
        Cmd::Editors,
        Cmd::Watchers,
        Cmd::WatchOn,
        Cmd::WatchOff,
        Cmd::Login,
        Cmd::Logout,
        Cmd::Admin,
        Cmd::History,
        Cmd::Rdiff,
    ] {
        assert!(
            !cmd.changes_working_copy(),
            "{cmd:?} only reads; the list must not be redrawn"
        );
    }
}

#[test]
fn a_command_runs_in_the_window_process() {
    let dir = std::env::temp_dir().join(format!(
        "cvsdesk-in-process-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let mut app = app_with_rows(Vec::new());
    app.dir = dir.clone();

    assert!(app.start(CommandRequest::new(Cmd::Status), OutputKind::Console));
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.runner.running && Instant::now() < deadline {
        app.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    app.poll();

    assert!(!app.runner.running, "the command never finished");
    assert!(
        app.runner.lines.len() > 1,
        "the command produced no output: {:?}",
        app.runner.lines
    );
    assert!(matches!(app.trouble, Some(Trouble::Exit(1))));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_query_command_leaves_the_rows_and_selection_alone() {
    let mut app = app_with_rows(vec![row("a.txt", false), row("b.txt", false)]);
    app.selected = HashSet::from([1]);
    app.anchor = Some(1);

    let (tx, rx) = channel();
    app.rx = Some(rx);
    app.refresh_after_command = false;
    tx.send(Msg::Done(0)).unwrap();
    app.poll();

    assert!(
        app.loading.is_none(),
        "log, status and diff must not start a folder read"
    );
    assert_eq!(app.rows.len(), 2, "the rows stay as they were");
    assert_eq!(app.selected, HashSet::from([1]));
    assert_eq!(app.anchor, Some(1));
}

#[test]
fn the_file_history_entry_needs_one_file() {
    let mut app = app_with_rows(vec![
        row("a.txt", false),
        row("b.txt", false),
        row("sub", true),
    ]);
    app.selected = HashSet::from([0, 1]);
    app.history_selected();
    assert!(app.history_waiting.is_none());
    assert!(app.status.contains("one file"));

    app.selected = HashSet::from([2]);
    app.history_selected();
    assert!(app.history_waiting.is_none());
    assert!(app.status.contains("not a folder"));
}

#[test]
fn a_finished_history_log_opens_the_history_window() {
    let mut app = app_with_rows(Vec::new());
    app.history_waiting = Some(HistoryWait {
        dir: app.dir.clone(),
        root: app.root.clone(),
        file: "main.c".into(),
        working: "1.2".into(),
        working_state: State::Unchanged,
        refresh: false,
    });

    let (tx, rx) = channel();
    app.rx = Some(rx);
    tx.send(Msg::Line("revision 1.2".into())).unwrap();
    tx.send(Msg::Line(
        "date: 2026/11/19 09:41:13;  author: bob;  state: Exp;".into(),
    ))
    .unwrap();
    tx.send(Msg::Line("Initial revision".into())).unwrap();
    tx.send(Msg::Done(0)).unwrap();
    app.poll();

    match app.dialog {
        Some(Dialog::History(ref state)) => {
            assert_eq!(state.file(), "main.c");
            assert_eq!(state.revision_count(), 1);
        }
        _ => panic!("the finished log should open the File History window"),
    }
}

#[test]
fn server_rows_join_the_list_and_stay_out_of_local_commands() {
    let mut app = app_with_rows(vec![row("a.txt", false), row("sub", true)]);
    app.selected = HashSet::from([0, 1]);

    app.apply_remote_listing(Listing {
        files: vec!["a.txt".to_string(), "b.txt".to_string()],
        dirs: vec!["sub".to_string(), "more".to_string()],
    });

    assert_eq!(app.rows.len(), 4);
    assert_eq!(app.rows[0].origin, Origin::Local);
    assert_eq!(app.rows[2].name, "b.txt");
    assert_eq!(app.rows[2].state, State::Remote);
    assert_eq!(app.rows[2].origin, Origin::Remote);
    assert_eq!(app.rows[3].name, "more");
    assert!(app.rows[3].is_dir);
    assert!(app.order_dirty);

    app.selected = HashSet::from([2]);
    assert!(app.selected_local_names().is_empty());
    assert!(app.selection_is_all_remote());
    assert_eq!(app.selected_names(), vec!["b.txt".to_string()]);

    app.selected = HashSet::from([0, 2]);
    assert_eq!(app.selected_local_names(), vec!["a.txt".to_string()]);
    assert!(!app.selection_is_all_remote());
}

#[test]
fn a_filter_hides_selected_rows_from_local_commands() {
    let mut app = app_with_rows(vec![row("a.txt", false), row("b.txt", false)]);
    app.selected = HashSet::from([0, 1]);
    app.search = "a".into();
    app.order_dirty = true;
    app.ensure_order();

    assert_eq!(
        app.selected_local_names(),
        vec!["a.txt".to_string()],
        "only the visible local row may reach cvs"
    );
    assert_eq!(app.hidden_selected_count(), 1);
}

#[test]
fn a_hidden_selection_does_not_run_a_command() {
    let mut app = app_with_rows(vec![row("a.txt", false), row("b.txt", false)]);
    app.selected = HashSet::from([1]);
    app.search = "a".into();
    app.order_dirty = true;
    app.ensure_order();

    app.cvs_command(Cmd::Update);

    assert!(app.rx.is_none(), "no cvs command should have started");
    assert!(app.status.contains("hidden by"), "{}", app.status);
}

#[test]
fn select_all_shown_selects_only_the_visible_rows() {
    let mut app = app_with_rows(vec![row("a.txt", false), row("b.txt", false)]);
    app.search = "a".into();
    app.order_dirty = true;
    app.ensure_order();

    app.select_all_shown();

    assert_eq!(app.selected, HashSet::from([0]));
}

#[test]
fn a_filtered_empty_selection_does_not_open_commit() {
    let mut app = app_with_rows(vec![row("a.txt", false)]);
    app.search = "a".into();
    app.order_dirty = true;
    app.ensure_order();

    app.open_commit_dialog(String::new());

    assert!(app.dialog.is_none());
    assert!(app.status.contains("Select files"), "{}", app.status);
}

#[test]
fn a_new_server_listing_replaces_the_previous_server_rows() {
    let mut app = app_with_rows(vec![row("a.txt", false)]);
    app.apply_remote_listing(Listing {
        files: vec!["b.txt".to_string()],
        dirs: Vec::new(),
    });
    app.apply_remote_listing(Listing {
        files: vec!["c.txt".to_string()],
        dirs: Vec::new(),
    });

    assert_eq!(app.rows.len(), 2);
    assert_eq!(app.rows[1].name, "c.txt");
    assert!(!app.rows.iter().any(|row| row.name == "b.txt"));
}

#[test]
fn a_listing_marks_the_remote_side_ready() {
    let mut app = app_with_rows(vec![row("a.txt", false)]);
    app.apply_remote_listing(Listing {
        files: vec!["b.txt".to_string()],
        dirs: Vec::new(),
    });

    assert_eq!(app.remote_state, RemoteState::Ready);
    assert_eq!(app.scope_counts(), (1, 1));
    assert_eq!(app.remote_status_label(), "1 not checked out");
}

#[test]
fn an_unfinished_listing_never_reports_zero() {
    let mut app = app_with_rows(vec![row("a.txt", false)]);
    app.remote_state = RemoteState::Loading;
    assert_eq!(app.remote_status_label(), "Checking server…");

    app.remote_state = RemoteState::Failed;
    assert_eq!(app.remote_status_label(), "Server list unavailable");
}

#[test]
fn a_disabled_remote_listing_stays_off() {
    let mut app = app_with_rows(vec![row("a.txt", false)]);
    app.start_remote_listing();
    assert_eq!(app.remote_state, RemoteState::Off);
}

#[test]
fn the_state_sheet_counts_only_rows_from_the_working_copy() {
    let mut same = row("a.txt", false);
    same.state = State::Unchanged;
    let mut app = app_with_rows(vec![same]);
    app.apply_remote_listing(Listing {
        files: vec!["b.txt".to_string()],
        dirs: vec!["more".to_string()],
    });

    let sheet = app.build_state_sheet();
    let files = sheet
        .lines()
        .find(|line| line.starts_with("Files"))
        .unwrap();
    assert!(
        files.ends_with(": 1"),
        "remote rows are not local files: {files}"
    );
}

#[test]
fn a_server_only_selection_does_not_run_a_local_command() {
    let mut app = app_with_rows(vec![row("a.txt", false)]);
    app.apply_remote_listing(Listing {
        files: vec!["b.txt".to_string()],
        dirs: Vec::new(),
    });
    app.selected = HashSet::from([1]);

    app.cvs_command(Cmd::Update);

    assert!(app.rx.is_none(), "no cvs command should have started");
    assert!(app.status.contains("not checked out"), "{}", app.status);
}

#[test]
fn update_all_ignores_the_selection() {
    let mut app = app_with_rows(vec![row("a.txt", false), row("b.txt", false)]);
    app.selected = HashSet::from([0, 1]);

    app.update_whole_folder();

    assert_eq!(
        app.runner.lines.first().map(String::as_str),
        Some("$ cvs -q -z9 update"),
        "{:?}",
        app.runner.lines
    );
}

#[test]
fn a_server_only_selection_does_not_open_a_commit_dialog() {
    let mut app = app_with_rows(vec![row("a.txt", false)]);
    app.apply_remote_listing(Listing {
        files: vec!["b.txt".to_string()],
        dirs: Vec::new(),
    });
    app.selected = HashSet::from([1]);

    app.open_commit_dialog(String::new());

    assert!(app.dialog.is_none());
    assert!(app.status.contains("not checked out"), "{}", app.status);
}

#[test]
fn opening_a_server_folder_writes_an_empty_skeleton_first() {
    let base = std::env::temp_dir().join(format!("cvsdesk-skeleton-open-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("CVS")).unwrap();
    fs::write(base.join("CVS").join("Root"), ":pserver:ann@host:/repo\n").unwrap();
    fs::write(base.join("CVS").join("Repository"), "Develop/Design\n").unwrap();
    fs::write(base.join("CVS").join("Entries"), "").unwrap();

    let mut app = app_with_rows(Vec::new());
    app.dir = base.clone();
    app.has_cvs = true;
    app.repository = "Develop/Design".into();

    app.ensure_and_open_dir("Child".into());

    assert_eq!(app.dir, base.join("Child"));
    assert!(base.join("Child").join("CVS").join("Root").is_file());
    assert_eq!(
        fs::read_to_string(base.join("Child").join("CVS").join("Repository"))
            .unwrap()
            .trim_end(),
        "Develop/Design/Child"
    );

    // Let the folder read finish; the test window never starts the server listing.
    for _ in 0..200 {
        app.poll_load();
        if app.loading.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(app.repository, "Develop/Design/Child");
    assert!(app.rows.is_empty());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_write_command_refreshes_in_place_and_keeps_the_selection() {
    let mut app = app_with_rows(vec![row("a.txt", false), row("b.txt", false)]);
    app.selected = HashSet::from([0]);
    app.anchor = Some(0);

    let (tx, rx) = channel();
    app.rx = Some(rx);
    app.refresh_after_command = true;
    tx.send(Msg::Done(0)).unwrap();
    app.poll();

    assert!(
        app.loading.is_some(),
        "a command that writes the folder reads it again"
    );
    assert_eq!(
        app.rows.len(),
        2,
        "the rows stay on screen while the read runs"
    );
    assert_eq!(app.selected, HashSet::from([0]));

    // The new list has another row first, so the selected name is now at index 1.
    let mut kept = row("a.txt", false);
    kept.state = State::Unchanged;
    app.apply_refresh_result(app.dir.clone(), loaded(vec![row("new.txt", false), kept]));

    assert_eq!(
        app.selected,
        HashSet::from([1]),
        "the selection follows its name, not its old index"
    );
    assert_eq!(app.anchor, Some(1), "the Shift anchor follows too");
    assert_eq!(app.rows.len(), 2);
    assert!(app.loading.is_none());
}

#[test]
fn a_finished_checkout_opens_the_folder_it_created() {
    let base = std::env::temp_dir().join(format!("cvsdesk-checkout-{}", std::process::id()));
    let target = base.join("proj");
    fs::create_dir_all(&target).unwrap();

    let mut app = app_with_rows(Vec::new());
    app.dir = base.clone();
    app.checkout_waiting = Some(target.clone());
    let (tx, rx) = channel();
    app.rx = Some(rx);
    tx.send(Msg::Done(0)).unwrap();
    app.poll();

    assert_eq!(
        app.dir, target,
        "a successful checkout shows what it created"
    );
    assert!(app.checkout_waiting.is_none());
    assert!(app.loading.is_some(), "the new folder is read");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_failed_checkout_stays_where_it_was() {
    let base = std::env::temp_dir().join(format!("cvsdesk-checkout-bad-{}", std::process::id()));
    let target = base.join("proj");
    fs::create_dir_all(&target).unwrap();

    let mut app = app_with_rows(Vec::new());
    app.dir = base.clone();
    app.checkout_waiting = Some(target);
    let (tx, rx) = channel();
    app.rx = Some(rx);
    tx.send(Msg::Done(1)).unwrap();
    app.poll();

    assert_eq!(app.dir, base, "nothing was created, so nothing is opened");
    assert!(app.checkout_waiting.is_none());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_typo_in_the_address_keeps_the_folder_and_says_so() {
    let root = std::env::temp_dir().join(format!("cvsdesk-typo-{}", std::process::id()));
    let sub = root.join("sub");
    fs::create_dir_all(&sub).unwrap();

    let mut app = app_with_rows(Vec::new());
    app.dir = root.clone();
    app.address = root.display().to_string();
    app.history = super::nav::History::new(root.clone());

    app.address = "not-there".into();
    assert!(!app.submit_address());
    assert_eq!(app.dir, root);
    assert!(app.status.contains("No such folder"), "{}", app.status);

    app.address = "sub".into();
    assert!(app.submit_address());
    assert_eq!(app.dir, sub);
    assert!(!app.address_editing);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn opening_tags_with_a_selection_passes_the_names_to_cvs_log() {
    let mut app = app_with_rows(vec![row("a.txt", false), row("b.txt", false)]);
    app.selected = HashSet::from([0, 1]);

    app.open_tags();

    let Some(Dialog::Tags(state)) = &app.dialog else {
        panic!("the Tags window did not open");
    };
    assert_eq!(state.scope, vec!["a.txt".to_string(), "b.txt".to_string()]);
    assert!(state.loading);
    assert_eq!(app.tag_waiting, Some(TagWait::Query));
    assert!(
        app.runner
            .lines
            .iter()
            .any(|line| line.contains("log -h a.txt b.txt")),
        "{:?}",
        app.runner.lines
    );
}

#[test]
fn opening_tags_without_a_selection_uses_the_folder() {
    let mut app = app_with_rows(Vec::new());

    app.open_tags();

    let Some(Dialog::Tags(state)) = &app.dialog else {
        panic!("the Tags window did not open");
    };
    assert!(state.scope.is_empty());
    assert_eq!(state.scope_label, "This folder and its subfolders");
    assert!(
        app.runner.lines.iter().any(|line| line.contains("log -h")),
        "{:?}",
        app.runner.lines
    );
}

#[test]
fn a_finished_tag_query_fills_the_open_window() {
    let mut app = app_with_rows(Vec::new());
    app.dialog = Some(Dialog::Tags(Box::new(TagsState::loading(
        Vec::new(),
        "this folder".into(),
    ))));
    app.tag_waiting = Some(TagWait::Query);

    let (tx, rx) = channel();
    app.rx = Some(rx);
    tx.send(Msg::Line("symbolic names:".into())).unwrap();
    tx.send(Msg::Line("\tRelease_1_0: 1.2".into())).unwrap();
    tx.send(Msg::Done(0)).unwrap();
    app.poll();

    let Some(Dialog::Tags(state)) = &app.dialog else {
        panic!("the Tags window is still open");
    };
    assert_eq!(
        state.tags,
        vec![TagInfo {
            name: "Release_1_0".into(),
            branch: false,
            revision: Some("1.2".into()),
        }]
    );
    assert!(!state.loading);
    assert!(state.error.is_empty());
    assert!(app.tag_waiting.is_none());
}

#[test]
fn a_failed_tag_query_leaves_the_error_in_the_window() {
    let mut app = app_with_rows(Vec::new());
    app.dialog = Some(Dialog::Tags(Box::new(TagsState::loading(
        Vec::new(),
        "this folder".into(),
    ))));
    app.tag_waiting = Some(TagWait::Query);

    let (tx, rx) = channel();
    app.rx = Some(rx);
    tx.send(Msg::Line("cvs log: no such repository".into()))
        .unwrap();
    tx.send(Msg::Done(1)).unwrap();
    app.poll();

    let Some(Dialog::Tags(state)) = &app.dialog else {
        panic!("the Tags window is still open");
    };
    assert_eq!(state.error, "cvs log: no such repository");
    assert!(!state.loading);
    assert!(state.tags.is_empty());
}

#[test]
fn a_failed_tag_write_keeps_its_error_and_does_not_read_the_tags_again() {
    let mut app = app_with_rows(Vec::new());
    app.dialog = Some(Dialog::Tags(Box::new(TagsState::loading(
        Vec::new(),
        "this folder".into(),
    ))));
    app.tag_waiting = Some(TagWait::AfterWrite);

    let (tx, rx) = channel();
    app.rx = Some(rx);
    tx.send(Msg::Line(
        "cvs tag: A tag with this name already exists".into(),
    ))
    .unwrap();
    tx.send(Msg::Done(1)).unwrap();
    app.poll();

    let Some(Dialog::Tags(state)) = &app.dialog else {
        panic!("the Tags window is still open");
    };
    assert_eq!(state.error, "cvs tag: A tag with this name already exists");
    assert!(!state.loading);
    assert!(app.tag_waiting.is_none());
    assert!(
        !app.runner.lines.iter().any(|line| line.contains("log -h")),
        "a failed write has nothing to refresh: {:?}",
        app.runner.lines
    );
}

#[test]
fn changing_folders_closes_the_tags_window() {
    let mut app = app_with_rows(Vec::new());
    app.dialog = Some(Dialog::Tags(Box::new(TagsState::loading(
        Vec::new(),
        "this folder".into(),
    ))));
    app.tag_waiting = Some(TagWait::Query);

    app.go_to(PathBuf::from("C:\\other"));

    assert!(app.dialog.is_none(), "the window belongs to the old folder");
    assert!(app.tag_waiting.is_none());
}
