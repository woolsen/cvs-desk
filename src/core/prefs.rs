//! The settings CVS Desk keeps, in a TOML file beside the program. The root a folder's
//! commands use is not one of them: it comes from that folder's `CVS/Root`. The
//! connection fields and the recent-root list only feed the Checkout, Import and Login
//! windows. The `P_*` names are kept stable for compatibility with existing settings files,
//! and the older `NAME = value` text file still loads.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::entries::{build_cvsroot, ConnKind};

/// The file list's remembered view: which rows are shown and how they are ordered.
///
/// These are CVS Desk's own setting names, kept beside the older compatibility keys in
/// the same file. The view state is owned and written by this program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewState {
    pub show_unchanged: bool,
    pub show_modified: bool,
    pub show_added: bool,
    pub show_removed: bool,
    pub show_conflicts: bool,
    pub show_unknown: bool,
    pub hide_unknown: bool,
    pub show_missing: bool,
    pub hide_missing: bool,
    pub show_folders: bool,
    pub sort: String,
    pub sort_ascending: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            show_unchanged: true,
            show_modified: true,
            show_added: true,
            show_removed: true,
            show_conflicts: true,
            show_unknown: true,
            hide_unknown: false,
            show_missing: true,
            hide_missing: false,
            show_folders: true,
            sort: "name".into(),
            sort_ascending: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Prefs {
    // CVSROOT
    pub kind: ConnKind,
    pub user: String,
    pub host: String,
    pub port: String,
    pub path: String,
    pub use_proxy: bool,
    pub proxy_host: String,
    pub proxy_port: String,
    /// The last root used, kept for settings files written by older versions. It no
    /// longer decides which root the window acts on; each folder carries its own root.
    pub cvsroot: String,
    /// Roots Checkout, Import and Login offer before their own fields. Newest first.
    pub recent_roots: Vec<String>,

    // External programs
    pub settings_folder: String,
    /// The folder the window showed when it last closed, so the next start opens it.
    pub last_dir: String,
    pub viewer: String,
    pub helper: String,
    pub browser: String,
    pub extdiff: String,

    // CVS
    pub quiet: bool,
    pub z9: bool,
    pub prune: bool,
    pub iso_convert: bool,
    pub checkout_ro: bool,
    pub diff_ext: bool,
    pub unix_lf: bool,
    pub keep_editors: bool,
    pub z_level: i32,

    // Diff defaults
    pub unified: bool,
    pub ignore_case: bool,
    pub no_whitespace: bool,
    pub no_blank_lines: bool,
    pub diff_options: String,

    // Commit
    pub word_wrap_log: bool,
    pub log_template: String,

    // Import: tags last used, offered again next time (P_VendorNames / P_ReleaseNames;
    // the module names in P_ImportModuleNames).
    pub vendor_names: Vec<String>,
    pub release_names: Vec<String>,
    pub import_modules: Vec<String>,

    /// The file list's filters and sort order, kept across starts.
    pub view: ViewState,
}

impl Default for Prefs {
    fn default() -> Self {
        let p = Self {
            kind: ConnKind::Pserver,
            user: String::new(),
            host: String::new(),
            port: String::new(),
            path: String::new(),
            use_proxy: false,
            proxy_host: "".into(),
            proxy_port: "".into(),
            cvsroot: String::new(),
            recent_roots: Vec::new(),

            settings_folder: "".into(),
            last_dir: String::new(),
            viewer: "notepad.exe".into(),
            helper: "".into(),
            browser: "".into(),
            extdiff: "".into(),

            quiet: true,
            z9: true,
            prune: false,
            iso_convert: false,
            checkout_ro: false,
            diff_ext: false,
            unix_lf: false,
            keep_editors: false,
            z_level: 9,

            unified: true,
            ignore_case: false,
            no_whitespace: false,
            no_blank_lines: false,
            diff_options: String::new(),

            word_wrap_log: true,
            log_template: String::new(),

            vendor_names: Vec::new(),
            release_names: Vec::new(),
            import_modules: Vec::new(),

            view: ViewState::default(),
        };
        p
    }
}

impl Prefs {
    /// Where CVS Desk keeps its settings: a TOML file beside the program. The older
    /// `NAME = value` text file is only read, so an existing install keeps its settings
    /// and writes them as TOML from then on.
    pub fn settings_file() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("cvsdesk-settings.toml"))
    }

    /// The settings file written before the TOML format.
    pub fn legacy_settings_file() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("cvsdesk-settings.txt"))
    }

    /// Write the settings out as TOML, keeping any key this version does not know.
    pub fn save(&self) -> std::io::Result<std::path::PathBuf> {
        let path = Self::settings_file()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no settings path"))?;
        let mut table = read_table(&path);
        self.write_into(&mut table);
        write_table(&path, &table)?;
        Ok(path)
    }

    /// The settings as TOML text, without touching a file.
    pub fn to_toml(&self) -> String {
        let mut table = Table::new();
        self.write_into(&mut table);
        table_to_toml(&table)
    }

    /// Put every known setting into `table`, leaving keys from another version alone.
    fn write_into(&self, table: &mut Table) {
        let text = |value: &str| Value::String(value.to_string());
        table.insert("P_Cvsroot".into(), text(&self.cvsroot));
        table.insert("P_OldCvsroots".into(), string_array(&self.recent_roots));
        table.insert("P_ConnKind".into(), text(self.kind.token()));
        table.insert("P_User".into(), text(&self.user));
        table.insert("P_Host".into(), text(&self.host));
        table.insert("P_Port".into(), text(&self.port));
        table.insert("P_Path".into(), text(&self.path));
        table.insert("P_LastWorkingDir".into(), text(&self.last_dir));
        table.insert("P_UseProxy".into(), Value::Boolean(self.use_proxy));
        table.insert("P_ProxyHost".into(), text(&self.proxy_host));
        table.insert("P_ProxyPort".into(), text(&self.proxy_port));
        table.insert("P_Editor".into(), text(&self.viewer));
        table.insert("P_DiffUseExtDiff".into(), Value::Boolean(self.diff_ext));
        table.insert("P_ExtDiff".into(), text(&self.extdiff));
        table.insert("P_UnifiedDiff".into(), Value::Boolean(self.unified));
        table.insert("P_IgnoreCase".into(), Value::Boolean(self.ignore_case));
        table.insert("P_NoWhiteSpace".into(), Value::Boolean(self.no_whitespace));
        table.insert("P_NoBlankLines".into(), Value::Boolean(self.no_blank_lines));
        table.insert("P_DiffOptions".into(), text(&self.diff_options));
        table.insert("P_QuietOption".into(), Value::Boolean(self.quiet));
        table.insert("P_PruneOption".into(), Value::Boolean(self.prune));
        table.insert("P_Z9Option".into(), Value::Boolean(self.z9));
        table.insert("P_ZLevel".into(), Value::Integer(self.z_level as i64));
        table.insert("P_CheckoutRO".into(), Value::Boolean(self.checkout_ro));
        table.insert("P_NIsoConvert".into(), Value::Boolean(self.iso_convert));
        table.insert("P_UseShell".into(), Value::Boolean(self.unix_lf));
        table.insert("P_KeepEditors".into(), Value::Boolean(self.keep_editors));
        table.insert("P_CommitLogs".into(), Value::Boolean(self.word_wrap_log));
        table.insert("P_LogTemplate".into(), text(&self.log_template));
        table.insert("P_VendorNames".into(), string_array(&self.vendor_names));
        table.insert("P_ReleaseNames".into(), string_array(&self.release_names));
        table.insert(
            "P_ImportModuleNames".into(),
            string_array(&self.import_modules),
        );
        for (name, value) in view_settings(&self.view) {
            table.insert(name.into(), value);
        }
    }

    /// Read the settings back: the TOML file when it is there, otherwise the text file
    /// written by an older version. Unknown names are ignored.
    pub fn load() -> Option<Prefs> {
        if let Some(path) = Self::settings_file() {
            if let Ok(text) = std::fs::read_to_string(path) {
                return Some(Self::from_settings_text(&text));
            }
        }
        let path = Self::legacy_settings_file()?;
        let text = std::fs::read_to_string(path).ok()?;
        Some(Self::from_settings_text(&text))
    }

    /// The folder the last session closed on, if it is still there. A folder that has
    /// been deleted, or a drive that is not connected, gives `None` so the caller can
    /// fall back to its usual start folder.
    pub fn last_folder(&self) -> Option<PathBuf> {
        let text = self.last_dir.trim();
        if text.is_empty() {
            return None;
        }
        let dir = PathBuf::from(text);
        dir.is_dir().then_some(dir)
    }

    /// Remember the folder to reopen next time. Only this one key of the settings file
    /// is touched: a key written by another version is left as it was.
    pub fn remember_last_dir(dir: &Path) -> std::io::Result<()> {
        update_settings(|table| {
            table.insert(
                "P_LastWorkingDir".into(),
                Value::String(dir.display().to_string()),
            );
        })
    }

    /// Remember a root the user just used. The newest stays first, without duplicates,
    /// and `cvsroot` keeps naming the last one for older settings readers.
    pub fn remember_root(&mut self, root: &str) {
        let root = root.trim();
        if root.is_empty() {
            return;
        }
        self.cvsroot = root.to_string();
        Prefs::remember(&mut self.recent_roots, root);
    }

    /// Write just the root keys into the settings file, keeping every other key as it
    /// was. Called when the window closes, the way the last folder is remembered.
    pub fn remember_roots(&self) -> std::io::Result<()> {
        if self.cvsroot.trim().is_empty() && self.recent_roots.is_empty() {
            return Ok(());
        }
        update_settings(|table| {
            table.insert("P_Cvsroot".into(), Value::String(self.cvsroot.clone()));
            table.insert("P_OldCvsroots".into(), string_array(&self.recent_roots));
        })
    }

    /// Remember the file list's filters and sort order. Only the view keys are touched,
    /// so every setting written by another part of the program stays in the file.
    pub fn remember_view(&self) -> std::io::Result<()> {
        update_settings(|table| {
            for (name, value) in view_settings(&self.view) {
                table.insert(name.into(), value);
            }
        })
    }

    /// Parse settings text: TOML when it is, the older `NAME = value` text otherwise.
    pub fn from_settings_text(text: &str) -> Prefs {
        match text.parse::<Table>() {
            Ok(table) => Self::from_table(&table),
            Err(_) => Self::from_legacy_text(text),
        }
    }

    /// Parse a TOML settings file. `None` when the text is not TOML.
    pub fn from_toml(text: &str) -> Option<Prefs> {
        text.parse::<Table>()
            .ok()
            .map(|table| Self::from_table(&table))
    }

    fn from_table(table: &Table) -> Prefs {
        let mut p = Prefs::default();
        let mut seen_cvsroot = false;
        let mut old_roots: Option<Vec<String>> = None;
        for (name, value) in table {
            match name.as_str() {
                "P_Cvsroot" => {
                    if let Some(text) = value.as_str() {
                        p.cvsroot = text.to_string();
                        seen_cvsroot = true;
                    }
                }
                "P_OldCvsroots" => old_roots = Some(string_list(value)),
                "P_ConnKind" => {
                    if let Some(text) = value.as_str() {
                        p.kind = ConnKind::from_token(text).unwrap_or(p.kind);
                    }
                }
                "P_User" => set_text(&mut p.user, value),
                "P_Host" => set_text(&mut p.host, value),
                "P_Port" => set_text(&mut p.port, value),
                "P_Path" => set_text(&mut p.path, value),
                "P_LastWorkingDir" => set_text(&mut p.last_dir, value),
                "P_UseProxy" => set_flag(&mut p.use_proxy, value),
                "P_ProxyHost" => set_text(&mut p.proxy_host, value),
                "P_ProxyPort" => set_text(&mut p.proxy_port, value),
                "P_Editor" => set_text(&mut p.viewer, value),
                "P_DiffUseExtDiff" => set_flag(&mut p.diff_ext, value),
                "P_ExtDiff" => set_text(&mut p.extdiff, value),
                "P_UnifiedDiff" => set_flag(&mut p.unified, value),
                "P_IgnoreCase" => set_flag(&mut p.ignore_case, value),
                "P_NoWhiteSpace" => set_flag(&mut p.no_whitespace, value),
                "P_NoBlankLines" => set_flag(&mut p.no_blank_lines, value),
                "P_DiffOptions" => set_text(&mut p.diff_options, value),
                "P_QuietOption" => set_flag(&mut p.quiet, value),
                "P_PruneOption" => set_flag(&mut p.prune, value),
                "P_Z9Option" => set_flag(&mut p.z9, value),
                "P_ZLevel" => {
                    if let Some(level) = as_int(value) {
                        p.z_level = level as i32;
                    }
                }
                "P_CheckoutRO" => set_flag(&mut p.checkout_ro, value),
                "P_NIsoConvert" => set_flag(&mut p.iso_convert, value),
                "P_UseShell" => set_flag(&mut p.unix_lf, value),
                "P_KeepEditors" => set_flag(&mut p.keep_editors, value),
                "P_CommitLogs" => set_flag(&mut p.word_wrap_log, value),
                "P_LogTemplate" => set_text(&mut p.log_template, value),
                "P_VendorNames" => p.vendor_names = string_list(value),
                "P_ReleaseNames" => p.release_names = string_list(value),
                "P_ImportModuleNames" => p.import_modules = string_list(value),
                "ViewShowUnchanged" => set_flag(&mut p.view.show_unchanged, value),
                "ViewShowModified" => set_flag(&mut p.view.show_modified, value),
                "ViewShowAdded" => set_flag(&mut p.view.show_added, value),
                "ViewShowRemoved" => set_flag(&mut p.view.show_removed, value),
                "ViewShowConflicts" => set_flag(&mut p.view.show_conflicts, value),
                "ViewShowUnknown" => set_flag(&mut p.view.show_unknown, value),
                "ViewHideUnknown" => set_flag(&mut p.view.hide_unknown, value),
                "ViewShowMissing" => set_flag(&mut p.view.show_missing, value),
                "ViewHideMissing" => set_flag(&mut p.view.hide_missing, value),
                "ViewShowFolders" => set_flag(&mut p.view.show_folders, value),
                "ViewSort" => set_text(&mut p.view.sort, value),
                "ViewSortAscending" => set_flag(&mut p.view.sort_ascending, value),
                _ => {}
            }
        }
        // A file written before the recent list existed names one root. Take it as the
        // first entry so the pickers still have something to offer.
        p.recent_roots = old_roots.unwrap_or_else(|| {
            if seen_cvsroot && !p.cvsroot.trim().is_empty() {
                vec![p.cvsroot.trim().to_string()]
            } else {
                Vec::new()
            }
        });
        p
    }

    /// Parse the older `NAME = value` text, so a file written before the TOML format
    /// still loads.
    fn from_legacy_text(text: &str) -> Prefs {
        let mut p = Prefs::default();
        let mut seen_cvsroot = false;
        let mut old_roots: Option<Vec<String>> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            let (name, value) = match line.split_once('=') {
                Some((n, v)) => (n.trim(), v.trim()),
                None => continue,
            };
            let flag = value == "1" || value.eq_ignore_ascii_case("true");
            let list = |v: &str| -> Vec<String> {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };
            match name {
                "P_Cvsroot" => {
                    p.cvsroot = value.to_string();
                    seen_cvsroot = true;
                }
                "P_OldCvsroots" => old_roots = Some(list(value)),
                "P_ConnKind" => p.kind = ConnKind::from_token(value).unwrap_or(p.kind),
                "P_User" => p.user = value.to_string(),
                "P_Host" => p.host = value.to_string(),
                "P_Port" => p.port = value.to_string(),
                "P_Path" => p.path = value.to_string(),
                "P_LastWorkingDir" => p.last_dir = value.to_string(),
                "P_UseProxy" => p.use_proxy = flag,
                "P_ProxyHost" => p.proxy_host = value.to_string(),
                "P_ProxyPort" => p.proxy_port = value.to_string(),
                "P_Editor" => p.viewer = value.to_string(),
                "P_DiffUseExtDiff" => p.diff_ext = flag,
                "P_ExtDiff" => p.extdiff = value.to_string(),
                "P_UnifiedDiff" => p.unified = flag,
                "P_IgnoreCase" => p.ignore_case = flag,
                "P_NoWhiteSpace" => p.no_whitespace = flag,
                "P_NoBlankLines" => p.no_blank_lines = flag,
                "P_DiffOptions" => p.diff_options = value.to_string(),
                "P_QuietOption" => p.quiet = flag,
                "P_PruneOption" => p.prune = flag,
                "P_Z9Option" => p.z9 = flag,
                "P_ZLevel" => p.z_level = value.parse().unwrap_or(p.z_level),
                "P_CheckoutRO" => p.checkout_ro = flag,
                "P_NIsoConvert" => p.iso_convert = flag,
                "P_UseShell" => p.unix_lf = flag,
                "P_KeepEditors" => p.keep_editors = flag,
                "P_CommitLogs" => p.word_wrap_log = flag,
                "P_LogTemplate" => p.log_template = value.replace("\\n", "\n"),
                "P_VendorNames" => p.vendor_names = list(value),
                "P_ReleaseNames" => p.release_names = list(value),
                "P_ImportModuleNames" => p.import_modules = list(value),
                "ViewShowUnchanged" => p.view.show_unchanged = flag,
                "ViewShowModified" => p.view.show_modified = flag,
                "ViewShowAdded" => p.view.show_added = flag,
                "ViewShowRemoved" => p.view.show_removed = flag,
                "ViewShowConflicts" => p.view.show_conflicts = flag,
                "ViewShowUnknown" => p.view.show_unknown = flag,
                "ViewHideUnknown" => p.view.hide_unknown = flag,
                "ViewShowMissing" => p.view.show_missing = flag,
                "ViewHideMissing" => p.view.hide_missing = flag,
                "ViewShowFolders" => p.view.show_folders = flag,
                "ViewSort" => p.view.sort = value.to_string(),
                "ViewSortAscending" => p.view.sort_ascending = flag,
                _ => {}
            }
        }
        // A file written before the recent list existed names one root. Take it as the
        // first entry so the pickers still have something to offer.
        p.recent_roots = old_roots.unwrap_or_else(|| {
            if seen_cvsroot && !p.cvsroot.trim().is_empty() {
                vec![p.cvsroot.trim().to_string()]
            } else {
                Vec::new()
            }
        });
        p
    }

    /// The root string from the fields.
    pub fn build_root(&self) -> String {
        let proxy = if self.use_proxy {
            Some((self.proxy_host.as_str(), self.proxy_port.as_str()))
        } else {
            None
        };
        build_cvsroot(
            self.kind, &self.user, &self.host, &self.port, &self.path, proxy,
        )
    }

    /// The switches the app puts in front of every command. `-P` is not one of them:
    /// it belongs after `update` or `checkout`, which is where `CommandRequest` puts it.
    pub fn globals(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.quiet {
            v.push("-q".into());
        }
        if self.z9 {
            v.push(format!("-z{}", self.z_level));
        }
        v
    }

    pub fn diff_flags(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.unified {
            v.push("-u".into());
        }
        if self.ignore_case {
            v.push("-i".into());
        }
        if self.no_whitespace {
            v.push("-w".into());
        }
        if self.no_blank_lines {
            v.push("-B".into());
        }
        for opt in self.diff_options.split_whitespace() {
            v.push(opt.to_string());
        }
        v
    }

    /// Remember a tag or module name this dialog has just used, newest first, without
    /// duplicates, keeping the list short.
    pub fn remember(list: &mut Vec<String>, value: &str) {
        let v = value.trim();
        if v.is_empty() {
            return;
        }
        list.retain(|s| s != v);
        list.insert(0, v.to_string());
        list.truncate(10);
    }
}

/// The view settings in the order they are written, using CVS Desk's own names.
fn view_settings(view: &ViewState) -> Vec<(&'static str, Value)> {
    vec![
        ("ViewShowUnchanged", Value::Boolean(view.show_unchanged)),
        ("ViewShowModified", Value::Boolean(view.show_modified)),
        ("ViewShowAdded", Value::Boolean(view.show_added)),
        ("ViewShowRemoved", Value::Boolean(view.show_removed)),
        ("ViewShowConflicts", Value::Boolean(view.show_conflicts)),
        ("ViewShowUnknown", Value::Boolean(view.show_unknown)),
        ("ViewHideUnknown", Value::Boolean(view.hide_unknown)),
        ("ViewShowMissing", Value::Boolean(view.show_missing)),
        ("ViewHideMissing", Value::Boolean(view.hide_missing)),
        ("ViewShowFolders", Value::Boolean(view.show_folders)),
        ("ViewSort", Value::String(view.sort.clone())),
        ("ViewSortAscending", Value::Boolean(view.sort_ascending)),
    ]
}

fn string_array(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

fn set_text(dst: &mut String, value: &Value) {
    if let Some(text) = value.as_str() {
        *dst = text.to_string();
    }
}

fn set_flag(dst: &mut bool, value: &Value) {
    if let Some(flag) = as_bool(value) {
        *dst = flag;
    }
}

fn as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Boolean(flag) => Some(*flag),
        // A file from before the TOML format wrote 1 and 0.
        Value::Integer(number) => Some(*number != 0),
        _ => None,
    }
}

fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(number) => Some(*number),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn string_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .collect(),
        Value::String(text) => text
            .split(',')
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// The settings table already on disk. A TOML file is read as it is; a file from before
/// the TOML format is converted, and so is the old `.txt` file when no TOML file exists
/// yet. Keys this version does not know stay in the table.
fn read_table(path: &Path) -> Table {
    if let Ok(text) = std::fs::read_to_string(path) {
        return table_from_settings_text(&text);
    }
    let Some(legacy) = Prefs::legacy_settings_file() else {
        return Table::new();
    };
    match std::fs::read_to_string(legacy) {
        Ok(text) => table_from_settings_text(&text),
        Err(_) => Table::new(),
    }
}

fn table_from_settings_text(text: &str) -> Table {
    match text.parse::<Table>() {
        Ok(table) => table,
        Err(_) => {
            let mut table = Table::new();
            Prefs::from_legacy_text(text).write_into(&mut table);
            table
        }
    }
}

fn update_settings(edit: impl FnOnce(&mut Table)) -> std::io::Result<()> {
    let path = Prefs::settings_file()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no settings path"))?;
    let mut table = read_table(&path);
    edit(&mut table);
    write_table(&path, &table)
}

fn write_table(path: &Path, table: &Table) -> std::io::Result<()> {
    std::fs::write(path, table_to_toml(table))
}

fn table_to_toml(table: &Table) -> String {
    let mut text = String::from("# CVS Desk settings. TOML.\n");
    let body = toml::to_string(table).unwrap_or_default();
    if !body.is_empty() {
        text.push('\n');
        text.push_str(&body);
        if !body.ends_with('\n') {
            text.push('\n');
        }
    }
    text
}

/// Replace just the view keys in `text`, keeping every other key where it was.
#[cfg(test)]
fn with_view(text: &str, view: &ViewState) -> String {
    let mut table = table_from_settings_text(text);
    for (name, value) in view_settings(view) {
        table.insert(name.into(), value);
    }
    table_to_toml(&table)
}

/// Write the view keys into an existing settings file. A missing file starts one.
#[cfg(test)]
fn write_view_to(path: &Path, view: &ViewState) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    std::fs::write(path, with_view(&text, view))
}

/// Replace one key in settings text, or add it. Every other key is kept, so a remembered
/// value never rewrites the file with defaults the user has not saved.
#[cfg(test)]
fn with_setting(text: &str, name: &str, value: Value) -> String {
    let mut table = table_from_settings_text(text);
    table.insert(name.into(), value);
    table_to_toml(&table)
}

/// The last-folder key, written the same way as any other single setting.
#[cfg(test)]
fn with_last_dir(text: &str, dir: &Path) -> String {
    with_setting(
        text,
        "P_LastWorkingDir",
        Value::String(dir.display().to_string()),
    )
}

/// The two root keys: the last root used and the list the dialogs offer.
#[cfg(test)]
fn with_roots(text: &str, last: &str, roots: &[String]) -> String {
    let mut table = table_from_settings_text(text);
    table.insert("P_Cvsroot".into(), Value::String(last.to_string()));
    table.insert("P_OldCvsroots".into(), string_array(roots));
    table_to_toml(&table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::parse_cvsroot;
    use toml::Table;

    #[test]
    fn the_settings_file_is_named_toml() {
        let path = Prefs::settings_file().expect("a settings path");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("cvsdesk-settings.toml")
        );
        let old = Prefs::legacy_settings_file().expect("the old settings path");
        assert_eq!(
            old.file_name().and_then(|name| name.to_str()),
            Some("cvsdesk-settings.txt")
        );
    }

    #[test]
    fn settings_are_written_as_toml_and_read_back() {
        let mut p = Prefs::default();
        p.kind = ConnKind::Kserver;
        p.user = "ann".into();
        p.host = "cvshost".into();
        p.port = "2402".into();
        p.path = "/repo".into();
        p.last_dir = "D:\\work\\repo".into();
        p.quiet = false;
        p.z9 = true;
        p.z_level = 7;
        p.log_template = "line one\nline two".into();
        p.vendor_names = vec!["acme".into(), "globex".into()];
        p.release_names = vec!["r1".into()];
        p.import_modules = vec!["proj".into()];
        p.view.show_unchanged = false;
        p.view.sort = "state".into();
        p.view.sort_ascending = false;

        let text = p.to_toml();
        let table: Table = text.parse().expect("the settings are valid TOML");
        assert_eq!(
            table.get("P_QuietOption").and_then(|v| v.as_bool()),
            Some(false),
            "flags are TOML booleans, not 1 and 0"
        );
        assert_eq!(
            table
                .get("P_VendorNames")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(2),
            "remembered names are a TOML array"
        );

        let back = Prefs::from_toml(&text).expect("the written TOML parses");
        assert_eq!(back.kind, ConnKind::Kserver);
        assert_eq!(back.user, "ann");
        assert_eq!(back.host, "cvshost");
        assert_eq!(back.port, "2402");
        assert_eq!(back.path, "/repo");
        assert_eq!(back.last_dir, "D:\\work\\repo");
        assert_eq!(back.z_level, 7);
        assert_eq!(back.vendor_names, vec!["acme", "globex"]);
        assert_eq!(back.release_names, vec!["r1"]);
        assert_eq!(back.import_modules, vec!["proj"]);
        assert_eq!(back.log_template, "line one\nline two");
        assert_eq!(back.view, p.view);
        assert_eq!(back.build_root(), ":kserver:ann@cvshost:2402:/repo");
    }

    #[test]
    fn toml_keeps_windows_paths_and_multiline_templates_whole() {
        let mut p = Prefs::default();
        p.last_dir = "D:\\work\\repo".into();
        p.cvsroot = ":pserver:ann@host:D:\\repo".into();
        p.log_template = "line one\nline two".into();

        let text = p.to_toml();
        text.parse::<Table>().expect("valid TOML");
        let back = Prefs::from_toml(&text).expect("round-trip");
        assert_eq!(back.last_dir, p.last_dir);
        assert_eq!(back.cvsroot, p.cvsroot);
        assert_eq!(back.log_template, p.log_template);
    }

    #[test]
    fn a_settings_file_from_before_the_toml_format_still_loads() {
        let text = "P_ConnKind = kserver\n\
                    P_User = ann\n\
                    P_QuietOption = 1\n\
                    P_ZLevel = 7\n\
                    P_VendorNames = acme,globex\n\
                    P_LogTemplate = one\\ntwo\n";
        let p = Prefs::from_settings_text(text);
        assert_eq!(p.kind, ConnKind::Kserver);
        assert_eq!(p.user, "ann");
        assert!(p.quiet);
        assert_eq!(p.z_level, 7);
        assert_eq!(p.vendor_names, vec!["acme", "globex"]);
        assert_eq!(p.log_template, "one\ntwo");
    }

    #[test]
    fn a_toml_file_loads_through_the_same_entry_point() {
        let p = Prefs::from_settings_text("P_User = \"ann\"\nP_QuietOption = false\n");
        assert_eq!(p.user, "ann");
        assert!(!p.quiet);
        assert!(
            Prefs::from_toml("P_User = ann").is_none(),
            "the old unquoted text is not TOML"
        );
    }

    #[test]
    fn changing_one_setting_in_an_old_text_file_keeps_the_others() {
        let text = "P_User = ann\nP_QuietOption = 1\n";
        let out = with_last_dir(text, Path::new("D:\\work"));
        let back = Prefs::from_toml(&out).expect("the updated file is TOML");
        assert_eq!(back.user, "ann");
        assert!(back.quiet);
        assert_eq!(back.last_dir, "D:\\work");
    }

    #[test]
    fn remember_keeps_newest_first_without_duplicates() {
        let mut list = Vec::new();
        Prefs::remember(&mut list, "acme");
        Prefs::remember(&mut list, "globex");
        assert_eq!(list, vec!["globex", "acme"], "newest first");
        Prefs::remember(&mut list, "acme");
        assert_eq!(list, vec!["acme", "globex"], "reused name moves to the top");
        Prefs::remember(&mut list, "   ");
        assert_eq!(list.len(), 2, "a blank name is not remembered");
        for i in 0..20 {
            Prefs::remember(&mut list, &format!("v{}", i));
        }
        assert_eq!(list.len(), 10, "the list stays short");
        assert_eq!(list[0], "v19");
    }

    #[test]
    fn the_prune_switch_is_not_a_global_option() {
        let mut prefs = Prefs::default();
        prefs.quiet = true;
        prefs.z9 = true;
        prefs.z_level = 9;
        prefs.prune = true;
        assert_eq!(prefs.globals(), vec!["-q", "-z9"]);
    }

    #[test]
    fn recent_roots_are_read_back_in_order() {
        let p = Prefs::from_legacy_text(
            "P_OldCvsroots = :pserver:ann@host:/repo,:kserver:bob@host:/other\n",
        );
        assert_eq!(
            p.recent_roots,
            vec![":pserver:ann@host:/repo", ":kserver:bob@host:/other"],
            "the list keeps the order it was written in"
        );
    }

    #[test]
    fn an_old_settings_file_seeds_the_recent_roots_from_its_cvsroot() {
        let p = Prefs::from_legacy_text("P_Cvsroot = :pserver:ann@host:/repo\n");
        assert_eq!(p.recent_roots, vec![":pserver:ann@host:/repo"]);
    }

    #[test]
    fn the_built_in_default_does_not_become_a_recent_root() {
        assert!(Prefs::from_legacy_text("").recent_roots.is_empty());
        assert!(Prefs::default().recent_roots.is_empty());
        assert!(
            Prefs::default().cvsroot.is_empty(),
            "a fresh install must not prefill an example repository"
        );
    }

    #[test]
    fn remembering_a_root_moves_it_to_the_front_and_updates_the_legacy_field() {
        let mut p =
            Prefs::from_legacy_text("P_OldCvsroots = :pserver:a@host:/one,:pserver:b@host:/two\n");
        p.remember_root(":pserver:b@host:/two");
        assert_eq!(
            p.recent_roots,
            vec![":pserver:b@host:/two", ":pserver:a@host:/one"]
        );
        assert_eq!(
            p.cvsroot, ":pserver:b@host:/two",
            "the old line names the last root used"
        );
        p.remember_root("  ");
        assert_eq!(p.recent_roots.len(), 2, "a blank root is not remembered");
    }

    #[test]
    fn rebuilding_the_roots_leaves_the_other_settings_alone() {
        let text = "P_User = \"ann\"\n\
                    P_Cvsroot = \":pserver:old@host:/repo\"\n\
                    P_Host = \"h\"\n\
                    FutureSetting = \"kept by a newer version\"\n";
        let out = with_roots(
            text,
            ":pserver:new@host:/repo",
            &[":pserver:new@host:/repo".to_string()],
        );

        let table: Table = out.parse().expect("still TOML");
        assert_eq!(table.get("P_User").and_then(|v| v.as_str()), Some("ann"));
        assert_eq!(
            table.get("P_Cvsroot").and_then(|v| v.as_str()),
            Some(":pserver:new@host:/repo")
        );
        assert_eq!(
            table
                .get("P_OldCvsroots")
                .and_then(|v| v.as_array())
                .map(|roots| roots.len()),
            Some(1)
        );
        assert_eq!(table.get("P_Host").and_then(|v| v.as_str()), Some("h"));
        assert_eq!(
            table.get("FutureSetting").and_then(|v| v.as_str()),
            Some("kept by a newer version")
        );
        assert_eq!(
            out.matches("P_Cvsroot").count(),
            1,
            "the old value is replaced, not added"
        );
    }

    #[test]
    fn comments_and_unknown_names_are_skipped() {
        let p = Prefs::from_legacy_text("; a comment\n\nP_NotARealSetting = 3\nP_User = bob\n");
        assert_eq!(p.user, "bob");
        assert_eq!(
            p.host,
            Prefs::default().host,
            "an unknown name changes nothing"
        );
    }

    #[test]
    fn root_round_trips_through_the_parser() {
        let p = Prefs::default();
        let root = p.build_root();
        let (kind, user, host, _port, path) = parse_cvsroot(&root);
        assert_eq!(kind, p.kind);
        assert_eq!(user, p.user);
        assert_eq!(host, p.host);
        assert_eq!(path, p.path);
    }

    #[test]
    fn the_last_folder_is_reopened_only_while_it_exists() {
        let dir = std::env::temp_dir().join(format!("cvsdesk-last-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let p = Prefs::from_legacy_text(&format!("P_LastWorkingDir = {}\n", dir.display()));
        assert_eq!(p.last_folder(), Some(dir.clone()));
        assert_eq!(
            Prefs::default().last_folder(),
            None,
            "nothing is remembered yet"
        );
        assert_eq!(
            Prefs::from_legacy_text("P_LastWorkingDir = Z:\\cvsdesk-no-such-drive\\gone\n")
                .last_folder(),
            None,
            "a folder that is no longer there is not reopened"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remembering_the_folder_leaves_the_other_settings_alone() {
        let text = "P_User = \"ann\"\n\
                    P_LastWorkingDir = \"C:\\\\old\"\n\
                    P_Host = \"h\"\n\
                    FutureSetting = 3\n";
        let out = with_last_dir(text, Path::new("D:\\work"));
        let table: Table = out.parse().expect("still TOML");
        assert_eq!(table.get("P_User").and_then(|v| v.as_str()), Some("ann"));
        assert_eq!(
            table.get("P_LastWorkingDir").and_then(|v| v.as_str()),
            Some("D:\\work")
        );
        assert_eq!(table.get("P_Host").and_then(|v| v.as_str()), Some("h"));
        assert_eq!(
            table.get("FutureSetting").and_then(|v| v.as_integer()),
            Some(3)
        );
        assert_eq!(
            out.matches("P_LastWorkingDir").count(),
            1,
            "the old value is replaced, not added"
        );

        let fresh = with_last_dir("", Path::new("D:\\work"));
        let fresh: Table = fresh.parse().expect("a new file is TOML");
        assert_eq!(
            fresh.get("P_LastWorkingDir").and_then(|v| v.as_str()),
            Some("D:\\work")
        );
    }

    #[test]
    fn view_settings_round_trip_through_the_parser() {
        let text = "ViewShowUnchanged = false\n\
                    ViewShowModified = true\n\
                    ViewShowAdded = true\n\
                    ViewShowRemoved = true\n\
                    ViewShowConflicts = true\n\
                    ViewShowUnknown = false\n\
                    ViewHideUnknown = false\n\
                    ViewShowMissing = true\n\
                    ViewHideMissing = true\n\
                    ViewShowFolders = false\n\
                    ViewSort = \"state\"\n\
                    ViewSortAscending = false\n";
        let p = Prefs::from_toml(text).expect("view settings are TOML");
        assert!(!p.view.show_unchanged);
        assert!(p.view.show_modified);
        assert!(!p.view.show_unknown);
        assert!(p.view.hide_missing);
        assert!(!p.view.show_folders);
        assert_eq!(p.view.sort, "state");
        assert!(!p.view.sort_ascending);
    }

    #[test]
    fn remembering_the_view_leaves_the_other_settings_alone() {
        let text = "P_User = \"ann\"\n\
                    ViewSort = \"name\"\n\
                    P_Host = \"h\"\n\
                    FutureSetting = \"kept by a newer version\"\n";
        let view = ViewState {
            show_unchanged: false,
            sort: "state".into(),
            sort_ascending: false,
            ..ViewState::default()
        };

        let out = with_view(text, &view);

        let table: Table = out.parse().expect("still TOML");
        assert_eq!(table.get("P_User").and_then(|v| v.as_str()), Some("ann"));
        assert_eq!(table.get("P_Host").and_then(|v| v.as_str()), Some("h"));
        assert_eq!(
            table.get("FutureSetting").and_then(|v| v.as_str()),
            Some("kept by a newer version")
        );
        assert_eq!(
            table.get("ViewShowUnchanged").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            table.get("ViewSort").and_then(|v| v.as_str()),
            Some("state")
        );
        assert_eq!(
            table.get("ViewSortAscending").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(Prefs::from_toml(&out).expect("valid TOML").view, view);
    }

    #[test]
    fn remembering_the_view_writes_the_file_and_keeps_other_keys() {
        let path = std::env::temp_dir().join(format!(
            "cvsdesk-view-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(
            &path,
            "P_User = \"ann\"\nViewShowUnchanged = true\nFutureSetting = \"kept\"\n",
        )
        .unwrap();
        let view = ViewState {
            show_unchanged: false,
            sort: "state".into(),
            sort_ascending: false,
            ..ViewState::default()
        };

        write_view_to(&path, &view).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let table: Table = text.parse().expect("still TOML");
        assert_eq!(table.get("P_User").and_then(|v| v.as_str()), Some("ann"));
        assert_eq!(
            table.get("FutureSetting").and_then(|v| v.as_str()),
            Some("kept")
        );
        assert_eq!(
            table.get("ViewShowUnchanged").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(Prefs::from_toml(&text).expect("valid TOML").view, view);
        std::fs::remove_file(&path).ok();
    }
}
