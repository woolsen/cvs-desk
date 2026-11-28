//! The file list's view model: filters, sort order, selection and the state sheet.

use std::cmp::Ordering;
use std::path::Path;

use cvsdesk::entries::{self, Origin, Row, State};
use cvsdesk::prefs::{ViewScope, ViewState};

use crate::app::{App, RemoteState};

/// A named task view. Each preset is a complete set of filter switches, so selecting
/// one never leaves a half-applied state behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FilterPreset {
    WorkingCopy,
    MyChanges,
    NeedsAttention,
    NotCheckedOut,
    Untracked,
    AllNames,
    Custom,
}

impl FilterPreset {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::WorkingCopy => "Local",
            Self::MyChanges => "My changes",
            Self::NeedsAttention => "Needs attention",
            Self::NotCheckedOut => "Not checked out",
            Self::Untracked => "Untracked",
            Self::AllNames => "All names",
            Self::Custom => "Custom",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Filters {
    pub(super) scope: ViewScope,
    pub(super) unchanged: bool,
    pub(super) changed: bool,
    pub(super) added: bool,
    pub(super) removed: bool,
    pub(super) conflicts: bool,
    pub(super) unknown: bool,
    pub(super) hide_unknown: bool,
    pub(super) missing: bool,
    pub(super) hide_missing: bool,
    pub(super) folders: bool,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            // A folder opens with every name in it: the working copy's rows together
            // with the repository's not checked out files.
            scope: ViewScope::All,
            unchanged: true,
            changed: true,
            added: true,
            removed: true,
            conflicts: true,
            unknown: true,
            hide_unknown: false,
            missing: true,
            hide_missing: false,
            folders: true,
        }
    }
}

impl Filters {
    /// The switches the user chose last time, as saved in the settings file.
    pub(super) fn from_view(view: &ViewState) -> Self {
        Self {
            scope: view.scope,
            unchanged: view.show_unchanged,
            changed: view.show_modified,
            added: view.show_added,
            removed: view.show_removed,
            conflicts: view.show_conflicts,
            unknown: view.show_unknown,
            hide_unknown: view.hide_unknown,
            missing: view.show_missing,
            hide_missing: view.hide_missing,
            folders: view.show_folders,
        }
    }

    /// Copy the switches into the settings draft without touching its sort fields.
    pub(super) fn write_view(&self, view: &mut ViewState) {
        view.scope = self.scope;
        view.show_unchanged = self.unchanged;
        view.show_modified = self.changed;
        view.show_added = self.added;
        view.show_removed = self.removed;
        view.show_conflicts = self.conflicts;
        view.show_unknown = self.unknown;
        view.hide_unknown = self.hide_unknown;
        view.show_missing = self.missing;
        view.hide_missing = self.hide_missing;
        view.show_folders = self.folders;
    }
}

/// The search box's stable id, so Esc can find the field.
pub(super) fn search_id() -> egui::Id {
    egui::Id::unique("file-search")
}

/// What the search box is looking for: a case-insensitive substring, or - when the text
/// holds `*` or `?` - a wildcard pattern that must match the whole shown name.
pub(super) struct Search {
    needle: String,
    wildcard: bool,
}

impl Search {
    /// `None` when the box is empty (or only spaces), so the list shows everything.
    pub(super) fn new(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(Self {
            needle: text.to_lowercase(),
            wildcard: text.contains(['*', '?']),
        })
    }

    pub(super) fn matches(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        if self.wildcard {
            glob_match(&self.needle, &name)
        } else {
            name.contains(&self.needle)
        }
    }
}

/// `*` matches any run of characters (including none) and `?` matches exactly one.
/// Both sides are already lowercased by [`Search`].
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // Where to resume when a `*` has to swallow one more character.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi + 1, ti));
            pi += 1;
        } else if let Some((after, at)) = star {
            star = Some((after, at + 1));
            pi = after;
            ti = at + 1;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// The name a row shows. A `D` entry normally names one child folder, but some working
/// copies record a full path there; the list shows the folder it points at, not the
/// whole path, while the recorded text stays untouched for cvs commands.
pub(super) fn display_name(name: &str, is_dir: bool) -> &str {
    if is_dir {
        let last = name.rsplit(['\\', '/']).next().unwrap_or(name);
        if !last.is_empty() {
            return last;
        }
    }
    name
}

impl Filters {
    pub(super) fn shows(&self, r: &Row) -> bool {
        let in_scope = match self.scope {
            ViewScope::All => true,
            ViewScope::WorkingCopy => r.origin == Origin::Local,
            ViewScope::NotCheckedOut => r.origin == Origin::Remote,
        };
        if !in_scope {
            return false;
        }
        match r.state {
            State::Folder => self.folders,
            State::Unchanged => self.unchanged,
            State::Modified => self.changed,
            State::Added => self.added,
            State::Removed => self.removed,
            State::Conflict | State::NeedsPatch => self.conflicts,
            State::Unknown => self.unknown && !self.hide_unknown,
            State::Missing => self.missing && !self.hide_missing,
            // Repository names have no local state to filter; the scope decides whether
            // they are in the list at all.
            State::Remote => !r.is_dir || self.folders,
        }
    }

    pub(super) fn enabled(&self, s: State) -> bool {
        match s {
            State::Folder => self.folders,
            State::Unchanged => self.unchanged,
            State::Modified => self.changed,
            State::Added => self.added,
            State::Removed => self.removed,
            State::Conflict | State::NeedsPatch => self.conflicts,
            State::Unknown => self.unknown,
            State::Missing => self.missing,
            State::Remote => self.scope != ViewScope::WorkingCopy,
        }
    }

    /// Is the state's box ticked at all, whether or not "hide" keeps its rows out of the list?
    /// Used by the Show menu to say which states the list is filtering on.
    pub(super) fn filtering(&self) -> Vec<&'static str> {
        let mut hidden = Vec::new();
        for (state, name) in [
            (State::Unchanged, "up-to-date"),
            (State::Modified, "modified"),
            (State::Added, "added"),
            (State::Removed, "removed"),
            (State::Conflict, "conflicts"),
        ] {
            if !self.enabled(state) {
                hidden.push(name);
            }
        }
        if !self.unknown || self.hide_unknown {
            hidden.push("untracked");
        }
        if !self.missing || self.hide_missing {
            hidden.push("missing locally");
        }
        if !self.folders {
            hidden.push("sub-folders");
        }
        if self.scope == ViewScope::NotCheckedOut {
            hidden.push("local (scope)");
        }
        hidden
    }

    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(super) fn set_state(&mut self, state: State, on: bool) {
        match state {
            State::Folder => self.folders = on,
            State::Unchanged => self.unchanged = on,
            State::Modified => self.changed = on,
            State::Added => self.added = on,
            State::Removed => self.removed = on,
            State::Conflict | State::NeedsPatch => self.conflicts = on,
            State::Unknown => self.unknown = on,
            State::Missing => self.missing = on,
            State::Remote => {}
        }
    }

    pub(super) fn apply_preset(&mut self, preset: FilterPreset) {
        let mut filters = Self::default();
        // The default view shows every name, so a preset that is about the working copy
        // sets its scope back explicitly instead of inheriting the default.
        match preset {
            FilterPreset::WorkingCopy => {
                filters.scope = ViewScope::WorkingCopy;
            }
            FilterPreset::MyChanges => {
                filters.scope = ViewScope::WorkingCopy;
                filters.unchanged = false;
                filters.unknown = false;
                filters.missing = false;
                filters.folders = false;
            }
            FilterPreset::NeedsAttention => {
                filters.scope = ViewScope::WorkingCopy;
                filters.unchanged = false;
                filters.changed = false;
                filters.added = false;
                filters.removed = false;
                filters.unknown = false;
                filters.folders = false;
            }
            FilterPreset::NotCheckedOut => {
                filters.scope = ViewScope::NotCheckedOut;
            }
            FilterPreset::Untracked => {
                filters.scope = ViewScope::WorkingCopy;
                filters.unchanged = false;
                filters.changed = false;
                filters.added = false;
                filters.removed = false;
                filters.conflicts = false;
                filters.missing = false;
                filters.folders = false;
            }
            FilterPreset::AllNames => {
                filters.scope = ViewScope::All;
            }
            FilterPreset::Custom => return,
        }
        *self = filters;
    }

    /// Which named view the current switches describe, if any.
    pub(super) fn preset(&self) -> FilterPreset {
        for preset in [
            FilterPreset::WorkingCopy,
            FilterPreset::MyChanges,
            FilterPreset::NeedsAttention,
            FilterPreset::NotCheckedOut,
            FilterPreset::Untracked,
            FilterPreset::AllNames,
        ] {
            let mut candidate = Self::default();
            candidate.apply_preset(preset);
            if *self == candidate {
                return preset;
            }
        }
        FilterPreset::Custom
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SortKey {
    Name,
    State,
    Revision,
    Tag,
    Date,
}

impl SortKey {
    /// The wording the Sort by menu uses. The list's heading for Date says "Recorded";
    /// the menu spells out that it means the recorded time.
    pub(super) fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::State => "State",
            SortKey::Revision => "Revision",
            SortKey::Tag => "Sticky tag",
            SortKey::Date => "Recorded time",
        }
    }

    /// The value stored in the settings file.
    pub(super) fn token(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::State => "state",
            SortKey::Revision => "revision",
            SortKey::Tag => "tag",
            SortKey::Date => "date",
        }
    }

    /// Read a saved value, falling back to the name column for anything unknown.
    pub(super) fn from_token(token: &str) -> Self {
        match token.trim().to_ascii_lowercase().as_str() {
            "state" => SortKey::State,
            "revision" => SortKey::Revision,
            "tag" => SortKey::Tag,
            "date" => SortKey::Date,
            _ => SortKey::Name,
        }
    }
}

impl App {
    /// Open the search box at the right of the nav bar and hand it the keyboard.
    pub(super) fn open_search(&mut self) {
        self.search_open = true;
        self.focus_search = true;
    }

    /// Close the search box and drop its filter, so the whole folder is listed again.
    pub(super) fn close_search(&mut self, ctx: &egui::Context) {
        self.search.clear();
        self.search_open = false;
        self.focus_search = false;
        self.order_dirty = true;
        ctx.memory_mut(|m| m.surrender_focus(search_id()));
    }

    /// Does any row carry a sticky tag? Sorting by it is only meaningful then; the
    /// folder's own `CVS/Tag` does not order rows on its own.
    pub(super) fn has_row_tags(&self) -> bool {
        self.rows.iter().any(|row| !row.tag.is_empty())
    }

    /// The sort actually used. A saved tag sort falls back to the name while no row
    /// carries a tag, so the list never looks unsorted; a tagged folder restores it.
    pub(super) fn effective_sort(&self) -> SortKey {
        if self.sort == SortKey::Tag && !self.has_row_tags() {
            SortKey::Name
        } else {
            self.sort
        }
    }

    /// Rebuild the visible order only when something that affects it changed.
    pub(super) fn ensure_order(&mut self) {
        if !self.order_dirty {
            return;
        }
        let search = Search::new(&self.search);
        let mut order: Vec<usize> = (0..self.rows.len())
            .filter(|i| self.row_matches(&self.rows[*i], search.as_ref()))
            .collect();
        let key = self.effective_sort();
        let ascending = self.sort_asc;
        let rows = &self.rows;
        order.sort_by(|a, b| compare_rows(key, ascending, &rows[*a], &rows[*b]));
        self.order = order;
        self.order_dirty = false;
    }

    pub(super) fn build_state_sheet(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Folder         : {}\n", self.dir.display()));
        out.push_str(&format!(
            "CVSROOT        : {}\n",
            if self.root.is_empty() {
                "(none - not a project folder)"
            } else {
                &self.root
            }
        ));
        out.push_str(&format!(
            "Repository     : {}\n",
            if self.repository.is_empty() {
                "(unknown)"
            } else {
                &self.repository
            }
        ));
        out.push_str(&format!("Checked        : {}\n\n", entries::now_text()));
        // A folder with 20,000 files must produce the same short sheet as an empty one:
        // the list already shows each row, so this sheet only counts them.
        let files = self
            .rows
            .iter()
            .filter(|r| !r.is_dir && r.origin == Origin::Local)
            .count();
        out.push_str(&format!("{:<21}: {}\n", "Files", files));
        for state in [
            State::Unchanged,
            State::Modified,
            State::Added,
            State::Removed,
            State::NeedsPatch,
            State::Conflict,
            State::Missing,
            State::Unknown,
        ] {
            let count = self
                .rows
                .iter()
                .filter(|r| !r.is_dir && r.origin == Origin::Local && r.state == state)
                .count();
            out.push_str(&format!("  {:<20}: {}\n", state.label(), count));
        }
        out.push_str("\nEvery file's state is in the list above; this sheet only counts them.\n");
        out.push_str("\nHow each state is decided:\n");
        out.push_str("  Up-to-date          file time equals the time recorded in CVS/Entries\n");
        out.push_str("  Locally Modified    the two times differ\n");
        out.push_str("  Locally Added       revision recorded as 0 (added, never committed)\n");
        out.push_str("  Locally Removed     revision recorded as -1\n");
        out.push_str("  Needs Patch         the recorded date begins with '/' (already patched)\n");
        out.push_str("  Unresolved Conflict the recorded date says 'Result of merge'\n");
        out.push_str("  Missing locally     CVS/Entries names a file that is not on disk\n");
        out.push_str("  Untracked           a file on disk that CVS/Entries does not name\n");
        out
    }

    /// Format the File states sheet now. Loading a folder only marks it stale; this runs
    /// when the pane is shown or when the user asks for a fresh sheet.
    pub(super) fn refresh_state_sheet(&mut self) {
        self.state_sheet = self.build_state_sheet();
        self.state_dirty = false;
    }

    /// Counts for the filter menu. Conflicts and Needs Patch share one switch, so their
    /// counts are added; the other states are counted exactly.
    pub(super) fn filter_state_counts(&self) -> Vec<(State, usize)> {
        let local = |state: State| {
            self.rows
                .iter()
                .filter(|row| row.origin == Origin::Local && row.state == state)
                .count()
        };
        vec![
            (State::Unchanged, local(State::Unchanged)),
            (State::Modified, local(State::Modified)),
            (State::Added, local(State::Added)),
            (State::Removed, local(State::Removed)),
            (
                State::Conflict,
                local(State::Conflict) + local(State::NeedsPatch),
            ),
            (State::Unknown, local(State::Unknown)),
            (State::Missing, local(State::Missing)),
        ]
    }

    pub(super) fn count_states(&self) -> Vec<(State, usize)> {
        let mut v = Vec::new();
        for s in [
            State::Modified,
            State::Added,
            State::Removed,
            State::Conflict,
            State::Unknown,
            State::Missing,
        ] {
            let n = self.rows.iter().filter(|r| r.state == s).count();
            if n > 0 {
                v.push((s, n));
            }
        }
        v
    }

    pub(super) fn scope_counts(&self) -> (usize, usize) {
        let local = self
            .rows
            .iter()
            .filter(|row| row.origin == Origin::Local)
            .count();
        (local, self.rows.len() - local)
    }

    /// What the status bar and the scope bar may say about the repository side.
    /// A listing that is not ready must never be reported as zero.
    pub(super) fn remote_status_label(&self) -> String {
        let (_, remote) = self.scope_counts();
        match self.remote_state {
            RemoteState::Off => "Not checked out: unavailable".to_string(),
            RemoteState::Loading if self.remote_found > 0 => {
                format!("Checking server… {} found", self.remote_found)
            }
            RemoteState::Loading => "Checking server…".to_string(),
            RemoteState::Ready => format!("{remote} not checked out"),
            RemoteState::Stopped => format!("{remote} not checked out (partial)"),
            RemoteState::Failed => "Server list unavailable".to_string(),
        }
    }

    pub(super) fn has_active_filter(&self) -> bool {
        !self.filters.is_default() || !self.search.trim().is_empty()
    }

    #[cfg(test)]
    pub(super) fn selected_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .selected
            .iter()
            .filter_map(|i| self.rows.get(*i))
            .map(|r| r.name.clone())
            .collect();
        names.sort();
        names
    }

    /// The selected names that are visible in the current filter. Copy actions use this,
    /// so a row hidden by a filter is not copied behind the user's back.
    pub(super) fn selected_visible_names(&self) -> Vec<String> {
        let search = Search::new(&self.search);
        let mut names: Vec<String> = self
            .selected
            .iter()
            .filter_map(|i| self.rows.get(*i))
            .filter(|row| self.row_matches(row, search.as_ref()))
            .map(|row| row.name.clone())
            .collect();
        names.sort();
        names
    }

    /// The full paths of the selected rows visible in the current view, in the same
    /// order as [`Self::selected_visible_names`].
    pub(super) fn selected_visible_paths(&self) -> Vec<String> {
        self.selected_visible_names()
            .iter()
            .map(|name| self.dir.join(name).display().to_string())
            .collect()
    }

    /// The selected names that exist in the working copy and are visible. Commands such
    /// as update, commit or edit must not be given a name that is not checked out or
    /// hidden by the current view.
    pub(super) fn selected_local_names(&self) -> Vec<String> {
        let search = Search::new(&self.search);
        let mut names: Vec<String> = self
            .selected
            .iter()
            .filter_map(|i| self.rows.get(*i))
            .filter(|row| row.origin == Origin::Local)
            .filter(|row| self.row_matches(row, search.as_ref()))
            .map(|row| row.name.clone())
            .collect();
        names.sort();
        names
    }

    /// True when something is selected and every selected row is server-only.
    pub(super) fn selection_is_all_remote(&self) -> bool {
        !self.selected.is_empty()
            && self.selected.iter().all(|index| {
                self.rows
                    .get(*index)
                    .is_some_and(|row| row.origin == Origin::Remote)
            })
    }

    /// How many selected rows the current filter does not show.
    pub(super) fn hidden_selected_count(&self) -> usize {
        let search = Search::new(&self.search);
        self.selected
            .iter()
            .filter(|index| {
                self.rows
                    .get(**index)
                    .map_or(true, |row| !self.row_matches(row, search.as_ref()))
            })
            .count()
    }

    pub(super) fn clear_hidden_selection(&mut self) {
        let search = Search::new(&self.search);
        let visible: Vec<usize> = self
            .selected
            .iter()
            .copied()
            .filter(|index| {
                self.rows
                    .get(*index)
                    .is_some_and(|row| self.row_matches(row, search.as_ref()))
            })
            .collect();
        self.selected = visible.into_iter().collect();
        if self
            .anchor
            .is_some_and(|index| !self.selected.contains(&index))
        {
            self.anchor = None;
        }
    }

    pub(super) fn select_all_shown(&mut self) {
        let search = Search::new(&self.search);
        self.selected = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| self.row_matches(row, search.as_ref()))
            .map(|(index, _)| index)
            .collect();
        self.anchor = self.selected.iter().copied().min();
    }

    fn row_matches(&self, row: &Row, search: Option<&Search>) -> bool {
        self.filters.shows(row)
            && search.map_or(true, |search| {
                search.matches(display_name(&row.name, row.is_dir))
            })
    }

    pub(super) fn flip_sort(&mut self, k: SortKey) {
        if self.effective_sort() == k {
            self.sort_asc = !self.sort_asc;
        } else {
            self.sort_asc = true;
        }
        self.sort = k;
        self.order_dirty = true;
    }

    /// Copy the list's current filters and sort order into the settings draft. The
    /// settings file is written once when the window closes, not on every click.
    pub(super) fn sync_view_prefs(&mut self) {
        self.filters.write_view(&mut self.prefs.view);
        self.prefs.view.sort = self.sort.token().to_string();
        self.prefs.view.sort_ascending = self.sort_asc;
    }
}

/// The browser's row order. Folders come first in both directions; the chosen column then
/// orders the folders among themselves and the files among themselves.
fn compare_rows(key: SortKey, ascending: bool, a: &Row, b: &Row) -> Ordering {
    // `false < true`, so comparing b to a puts `is_dir == true` first.
    let folders = b.is_dir.cmp(&a.is_dir);
    if folders != Ordering::Equal {
        return folders;
    }
    if key == SortKey::Date {
        // The recorded text is asctime, whose day column and weekday do not sort by
        // calendar order. Parse it, and keep rows without a readable date at the end
        // in both directions.
        return match (
            entries::parse_entry_date(&a.timestamp),
            entries::parse_entry_date(&b.timestamp),
        ) {
            (Some(left), Some(right)) => {
                let c = left.cmp(&right);
                if ascending {
                    c
                } else {
                    c.reverse()
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
    }
    let c = match key {
        SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SortKey::State => a.state.label().cmp(b.state.label()),
        SortKey::Revision => revision_key(&a.revision).cmp(&revision_key(&b.revision)),
        SortKey::Tag => a.tag.to_lowercase().cmp(&b.tag.to_lowercase()),
        SortKey::Date => unreachable!("the date column is handled above"),
    };
    if ascending {
        c
    } else {
        c.reverse()
    }
}

fn revision_key(rev: &str) -> (u32, u32, String) {
    let mut it = rev.split('.');
    let a = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let b = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (a, b, rev.to_string())
}

#[allow(dead_code)]
fn is_dir(p: &Path) -> bool {
    p.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{app_with_rows, row};
    use cvsdesk::prefs::{ViewScope, ViewState};
    use cvsdesk::remote::Listing;

    #[test]
    fn folders_sort_before_files_in_both_directions() {
        let folder = row("zebra", true);
        let file = row("aaa.txt", false);
        for key in [
            SortKey::Name,
            SortKey::State,
            SortKey::Revision,
            SortKey::Tag,
            SortKey::Date,
        ] {
            for ascending in [true, false] {
                assert_eq!(
                    compare_rows(key, ascending, &folder, &file),
                    Ordering::Less,
                    "{key:?} ascending={ascending} must put the folder first"
                );
                assert_eq!(
                    compare_rows(key, ascending, &file, &folder),
                    Ordering::Greater,
                    "{key:?} ascending={ascending} must put the folder first"
                );
            }
        }
    }

    #[test]
    fn files_still_follow_the_chosen_column() {
        let a = row("aaa.txt", false);
        let b = row("bbb.txt", false);
        assert_eq!(compare_rows(SortKey::Name, true, &a, &b), Ordering::Less);
        assert_eq!(
            compare_rows(SortKey::Name, false, &a, &b),
            Ordering::Greater
        );
    }

    #[test]
    fn sticky_tags_sort_by_the_rows_tag() {
        let mut release = row("alpha.txt", false);
        release.tag = "Release_1_0".into();
        let mut head = row("beta.txt", false);
        head.tag = "HEAD".into();

        assert_eq!(
            compare_rows(SortKey::Tag, true, &head, &release),
            Ordering::Less,
            "HEAD sorts before Release_1_0, not by file name"
        );
    }

    #[test]
    fn a_tag_sort_without_tags_falls_back_to_the_name() {
        let mut app = app_with_rows(vec![row("beta.txt", false), row("alpha.txt", false)]);
        app.sort = SortKey::Tag;
        app.sort_asc = true;
        app.ensure_order();

        assert_eq!(app.sort, SortKey::Tag, "the saved choice is kept");
        assert_eq!(app.order, vec![1, 0], "with no tags the list sorts by name");
    }

    #[test]
    fn recorded_dates_sort_by_the_calendar_not_by_the_weekday() {
        // Both rows record a Wednesday, so the raw asctime text compares "Sep 25" with
        // "Sep 16" and never looks at the year. The date column must parse the time.
        let mut older = row("older.txt", false);
        older.timestamp = "Wed Sep 25 08:01:28 2019".into();
        let mut newer = row("newer.txt", false);
        newer.timestamp = "Wed Sep 16 01:44:33 2026".into();

        assert_eq!(
            compare_rows(SortKey::Date, true, &older, &newer),
            Ordering::Less,
            "2019 comes before 2026"
        );
        assert_eq!(
            compare_rows(SortKey::Date, false, &older, &newer),
            Ordering::Greater,
            "descending puts the newer year first"
        );

        let without_date = row("no-date.txt", false);
        assert_eq!(
            compare_rows(SortKey::Date, true, &older, &without_date),
            Ordering::Less,
            "a row with no readable date stays at the bottom"
        );
        assert_eq!(
            compare_rows(SortKey::Date, false, &older, &without_date),
            Ordering::Less,
            "a row with no readable date stays at the bottom in both directions"
        );
    }

    #[test]
    fn the_order_cache_is_reused_until_it_is_marked_stale() {
        let mut app = app_with_rows(vec![row("a.txt", false), row("b.txt", false)]);
        app.ensure_order();
        assert_eq!(app.order, vec![0, 1]);
        app.filters.unknown = false;
        app.ensure_order();
        assert_eq!(app.order, vec![0, 1], "a clean cache is reused");
        app.order_dirty = true;
        app.ensure_order();
        assert!(app.order.is_empty(), "the rebuilt order applies the filter");
    }

    #[test]
    fn an_empty_search_does_not_filter() {
        assert!(Search::new("").is_none());
        assert!(Search::new("   ").is_none());
    }

    #[test]
    fn plain_text_matches_anywhere_and_ignores_case() {
        let search = Search::new("readme").unwrap();
        assert!(search.matches("README.md"));
        assert!(search.matches("my-readme.txt"));
        assert!(!search.matches("read.txt"));

        let chinese = Search::new("报表").unwrap();
        assert!(chinese.matches("销售报表2026.xlsx"));
        assert!(!chinese.matches("销售明细.xlsx"));
    }

    #[test]
    fn a_star_matches_any_run_and_a_question_mark_one_character() {
        let cpp = Search::new("*.cpp").unwrap();
        assert!(cpp.matches("main.cpp"));
        assert!(
            !cpp.matches("main.cpp.bak"),
            "a wildcard pattern matches the whole name"
        );

        let log = Search::new("log?.txt").unwrap();
        assert!(log.matches("log1.txt"));
        assert!(!log.matches("log10.txt"));

        let middle = Search::new("a*b*c").unwrap();
        assert!(middle.matches("aXXbYYc"));
        assert!(!middle.matches("aXXbYY"));
    }

    #[test]
    fn search_matches_the_folder_name_the_list_shows() {
        let mut app = app_with_rows(vec![row(r"C:\temp\repo", true)]);
        app.search = "rep?".into();
        app.ensure_order();
        assert_eq!(
            app.order,
            vec![0],
            "the recorded path is not what the user sees or searches"
        );
    }

    #[test]
    fn search_and_the_state_filters_apply_together() {
        let mut app = app_with_rows(vec![row("Alpha.txt", false), row("alpha.log", false)]);
        app.rows[0].state = State::Modified;
        app.search = "alpha".into();
        app.filters.unknown = false;
        app.order_dirty = true;
        app.ensure_order();
        assert_eq!(app.order, vec![0]);
    }

    #[test]
    fn presets_set_the_expected_scope_and_states() {
        let mut filters = Filters::default();
        assert_eq!(
            filters.preset(),
            FilterPreset::AllNames,
            "a folder opens showing every name"
        );

        filters.apply_preset(FilterPreset::WorkingCopy);
        assert_eq!(filters.preset(), FilterPreset::WorkingCopy);
        assert_eq!(filters.scope, ViewScope::WorkingCopy);

        filters.apply_preset(FilterPreset::MyChanges);
        assert_eq!(filters.preset(), FilterPreset::MyChanges);
        assert_eq!(filters.scope, ViewScope::WorkingCopy);
        assert!(!filters.unchanged);
        assert!(filters.changed && filters.added && filters.removed && filters.conflicts);
        assert!(!filters.unknown && !filters.missing && !filters.folders);

        filters.apply_preset(FilterPreset::NotCheckedOut);
        assert_eq!(filters.preset(), FilterPreset::NotCheckedOut);
        assert_eq!(filters.scope, ViewScope::NotCheckedOut);

        filters.apply_preset(FilterPreset::AllNames);
        assert_eq!(filters.preset(), FilterPreset::AllNames);
        assert_eq!(filters.scope, ViewScope::All);
    }

    #[test]
    fn a_custom_filter_is_not_mistaken_for_a_preset() {
        let mut filters = Filters::default();
        filters.apply_preset(FilterPreset::Untracked);
        assert_eq!(filters.preset(), FilterPreset::Untracked);

        filters.unknown = false;
        assert_eq!(filters.preset(), FilterPreset::Custom);
    }

    #[test]
    fn state_switches_share_the_conflict_box() {
        let mut filters = Filters::default();
        filters.set_state(State::NeedsPatch, false);
        assert!(!filters.conflicts);

        filters.set_state(State::Conflict, true);
        assert!(filters.conflicts);
    }

    #[test]
    fn filter_state_counts_combine_conflicts_and_needs_patch() {
        let mut app = app_with_rows(vec![row("a.txt", false), row("b.txt", false)]);
        app.rows[0].state = State::Conflict;
        app.rows[1].state = State::NeedsPatch;

        let counts = app.filter_state_counts();
        let conflicts = counts
            .iter()
            .find(|(state, _)| *state == State::Conflict)
            .expect("the menu has a conflict count");
        assert_eq!(conflicts.1, 2);
    }

    #[test]
    fn working_copy_scope_hides_not_checked_out_rows() {
        let mut remote = row("remote.txt", false);
        remote.origin = Origin::Remote;
        remote.state = State::Remote;
        let mut app = app_with_rows(vec![row("local.txt", false), remote]);
        app.filters.scope = ViewScope::WorkingCopy;
        app.ensure_order();
        assert_eq!(app.order, vec![0]);

        app.filters.scope = ViewScope::NotCheckedOut;
        app.order_dirty = true;
        app.ensure_order();
        assert_eq!(app.order, vec![1]);

        app.filters.scope = ViewScope::All;
        app.order_dirty = true;
        app.ensure_order();
        assert_eq!(app.order.len(), 2);
    }

    #[test]
    fn the_default_filters_show_every_name() {
        assert_eq!(Filters::default().scope, ViewScope::All);
    }

    #[test]
    fn the_scope_controls_repository_rows_and_folders_still_apply() {
        let mut remote = row("server.txt", false);
        remote.origin = Origin::Remote;
        remote.state = State::Remote;

        let working_copy = Filters {
            scope: ViewScope::WorkingCopy,
            ..Filters::default()
        };
        assert!(!working_copy.shows(&remote));
        assert!(!working_copy.enabled(State::Remote));

        let mut all = Filters::default();
        assert!(all.shows(&remote));

        let mut remote_folder = row("server-folder", true);
        remote_folder.origin = Origin::Remote;
        remote_folder.state = State::Remote;
        all.folders = false;
        assert!(!all.shows(&remote_folder));
        assert!(all.shows(&remote));
    }

    #[test]
    fn search_finds_a_server_only_name() {
        let mut app = app_with_rows(vec![row("local.txt", false)]);
        app.filters.scope = ViewScope::All;
        app.apply_remote_listing(Listing {
            files: vec!["remote.txt".to_string()],
            dirs: vec!["RemoteFolder".to_string()],
        });
        app.search = "remote.txt".into();
        app.order_dirty = true;
        app.ensure_order();

        assert_eq!(app.order.len(), 1);
        assert_eq!(app.rows[app.order[0]].name, "remote.txt");
        assert_eq!(app.rows[app.order[0]].state, State::Remote);
    }

    #[test]
    fn a_folder_recorded_with_a_full_path_shows_only_its_last_part() {
        assert_eq!(
            display_name(r"C:\temp\cvsdesk-baseline-1", true),
            "cvsdesk-baseline-1"
        );
        assert_eq!(display_name(r"sub\child", true), "child");
        assert_eq!(display_name("plain", true), "plain");
        assert_eq!(display_name(r"C:\temp\file", false), r"C:\temp\file");
    }

    #[test]
    fn the_state_sheet_summarizes_each_state_and_leaves_the_rows_to_the_list() {
        let mut modified = row("changed.txt", false);
        modified.state = State::Modified;
        let mut unchanged = row("same.txt", false);
        unchanged.state = State::Unchanged;
        let mut loose = row("loose.txt", false);
        loose.state = State::Unknown;
        let app = app_with_rows(vec![modified, unchanged, loose, row("sub", true)]);
        let sheet = app.build_state_sheet();

        for name in ["changed.txt", "same.txt", "loose.txt", "sub"] {
            assert!(
                !sheet.contains(name),
                "the file list already shows the rows; the sheet must not repeat {name:?}"
            );
        }
        let files = sheet
            .lines()
            .find(|line| line.starts_with("Files"))
            .expect("the sheet gives a file total");
        assert!(files.ends_with(": 3"), "folders are not files: {files}");
        for (label, count) in [
            ("Up-to-date", 1),
            ("Locally Modified", 1),
            ("Locally Added", 0),
            ("Locally Removed", 0),
            ("Needs Patch", 0),
            ("Unresolved Conflict", 0),
            ("Missing locally", 0),
            ("Untracked", 1),
        ] {
            let line = sheet
                .lines()
                .find(|line| {
                    line.trim_start().starts_with(label) && line.ends_with(&format!(": {count}"))
                })
                .unwrap_or_else(|| panic!("the sheet has no {label:?} count: {sheet}"));
            assert!(
                line.contains(':'),
                "the count line, not the legend line: {line}"
            );
        }
    }

    #[test]
    fn the_state_sheet_stays_short_however_many_files_there_are() {
        let rows: Vec<Row> = (0..20_000)
            .map(|i| row(&format!("file-{i}.txt"), false))
            .collect();
        let app = app_with_rows(rows);
        let sheet = app.build_state_sheet();
        assert!(
            sheet.lines().count() < 40,
            "the sheet grew to {} lines with the folder; it must not list the files",
            sheet.lines().count()
        );
    }

    #[test]
    fn every_sort_key_round_trips_through_its_saved_token() {
        for key in [
            SortKey::Name,
            SortKey::State,
            SortKey::Revision,
            SortKey::Tag,
            SortKey::Date,
        ] {
            assert_eq!(SortKey::from_token(key.token()), key);
        }
    }

    #[test]
    fn an_unknown_saved_sort_falls_back_to_name() {
        assert_eq!(SortKey::from_token("bogus"), SortKey::Name);
    }

    #[test]
    fn filters_read_and_write_every_view_switch() {
        let view = ViewState {
            scope: ViewScope::All,
            show_unchanged: false,
            show_modified: false,
            show_added: false,
            show_removed: false,
            show_conflicts: false,
            show_unknown: false,
            hide_unknown: true,
            show_missing: false,
            hide_missing: true,
            show_folders: false,
            ..ViewState::default()
        };

        let filters = Filters::from_view(&view);
        assert!(!filters.unchanged);
        assert!(!filters.changed);
        assert!(!filters.added);
        assert!(!filters.removed);
        assert!(!filters.conflicts);
        assert!(!filters.unknown);
        assert!(filters.hide_unknown);
        assert!(!filters.missing);
        assert!(filters.hide_missing);
        assert!(!filters.folders);

        let mut back = ViewState::default();
        filters.write_view(&mut back);
        assert_eq!(back, view);
    }
}
