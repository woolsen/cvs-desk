//! Reading a CVS working folder: its control files (`CVS/Entries`, `CVS/Root`,
//! `CVS/Repository`) and deciding each file's state.
//!
//! The recorded timestamp in `CVS/Entries` and the file's modification time are both
//! formatted like C's `asctime` and compared as 24-character strings, with the
//! day-of-month column normalised so " 5" and "05" match. Equal -> unchanged, different ->
//! locally modified.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local, NaiveDateTime, TimeZone, Utc};

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub revision: String,
    pub date: String,
    pub options: String,
    pub tag: String,
}

impl Entry {
    /// `-kb`: the file is stored without keyword or line-ending conversion.
    pub fn binary(&self) -> bool {
        self.options.split(',').any(|o| o.trim() == "-kb")
    }
    /// `-ku`: the file is stored as Unicode.
    pub fn unicode(&self) -> bool {
        self.options.split(',').any(|o| o.trim() == "-ku")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Unchanged,
    Modified,
    Added,
    Removed,
    Conflict,
    NeedsPatch,
    Missing,
    Unknown,
    Folder,
    /// The repository has this name, but this working copy has not checked it out.
    Remote,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Unchanged => "Up-to-date",
            State::Modified => "Locally Modified",
            State::Added => "Locally Added",
            State::Removed => "Locally Removed",
            State::Conflict => "Unresolved Conflict",
            State::NeedsPatch => "Needs Patch",
            State::Missing => "Missing locally",
            State::Unknown => "Untracked",
            State::Folder => "Folder",
            State::Remote => "Not checked out",
        }
    }

    /// Which states the browser groups under "changed" (P_ViewChanged and friends).
    pub fn is_change(self) -> bool {
        matches!(
            self,
            State::Modified | State::Added | State::Removed | State::Conflict
        )
    }
}

/// Where a row came from: the working copy on disk, or the repository listing
/// merged into it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    Local,
    Remote,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub name: String,
    pub is_dir: bool,
    pub state: State,
    pub origin: Origin,
    pub revision: String,
    pub tag: String,
    pub options: String,
    pub timestamp: String,
    pub binary: bool,
    pub unicode: bool,
}

pub struct Loaded {
    pub rows: Vec<Row>,
    pub notes: Vec<String>,
    pub has_cvs: bool,
    pub root: String,
    pub repository: String,
    /// The first line of `CVS/Tag`, e.g. `TBRANCH` or `N1.2`; empty when the folder is
    /// not pinned to a tag, branch or date.
    pub sticky: String,
    pub partial_checkout: bool,
}

/// Parse the text of a `CVS/Entries` file. Returns the entries and how many duplicated
/// names were dropped.
pub fn parse_entries(text: &str) -> (Vec<Entry>, usize) {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut dupes = 0usize;
    for line in text.lines() {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('/').collect();
        if parts[0] == "D" {
            if parts.len() < 2 {
                continue;
            }
            let name = parts[1].to_string();
            if !seen.insert(name.clone()) {
                dupes += 1;
                continue;
            }
            out.push(Entry {
                name,
                is_dir: true,
                ..Default::default()
            });
        } else {
            // /name/revision/date/options/tag...
            if parts.len() < 5 || !line.starts_with('/') {
                continue;
            }
            let name = parts[1].to_string();
            if name.is_empty() {
                continue;
            }
            if !seen.insert(name.clone()) {
                dupes += 1;
                continue;
            }
            out.push(Entry {
                name,
                is_dir: false,
                revision: parts[2].to_string(),
                date: parts[3].to_string(),
                options: parts[4].to_string(),
                tag: parts.get(5).unwrap_or(&"").to_string(),
            });
        }
    }
    (out, dupes)
}

/// Apply `CVS/Entries.Log` on top of parsed entries, the way cvs does when it opens a
/// folder. Returns how many changes were applied.
///
/// The journal holds one change per line, `A ` (added) or `R ` (removed) followed by the
/// text an `Entries` line would have: `A D/name////` for a folder,
/// `A /name/revision/date/options/tag` for a file. A command that is interrupted (the
/// window is closed, the machine goes off) leaves it behind and the next cvs command merges
/// it; reading only `CVS/Entries` would call a folder that is really checked out "not in
/// project". Lines this client does not understand are skipped.
pub fn apply_pending_log(entries: &mut Vec<Entry>, text: &str) -> usize {
    let mut applied = 0usize;
    for raw in text.lines() {
        let line = raw.trim_end_matches(['\r', '\n']);
        let Some((op, body)) = line.split_once(' ') else {
            continue;
        };
        let add = match op {
            "A" => true,
            "R" => false,
            _ => continue,
        };
        let body = body.trim_end();
        if body.is_empty() {
            continue;
        }
        let entry = if let Some(rest) = body.strip_prefix("D/") {
            let name = rest.split('/').next().unwrap_or("");
            if name.is_empty() {
                continue;
            }
            Entry {
                name: name.to_string(),
                is_dir: true,
                ..Default::default()
            }
        } else {
            match parse_entries(body) {
                (mut parsed, _) if !parsed.is_empty() => parsed.remove(0),
                _ => continue,
            }
        };
        // The journal repeats what an earlier command already settled, so replace the
        // recorded entry instead of adding a duplicate.
        let name = entry.name.clone();
        entries.retain(|existing| !(existing.name == name && existing.is_dir == entry.is_dir));
        if add {
            entries.push(entry);
        }
        applied += 1;
    }
    applied
}

/// `Mon Nov 24 07:41:21 2026` - the 24 characters `asctime` leaves before its newline.
fn asctime24(n: NaiveDateTime) -> String {
    format!(
        "{} {} {:>2} {} {}",
        n.format("%a"),
        n.format("%b"),
        n.day(),
        n.format("%H:%M:%S"),
        n.format("%Y")
    )
}

fn asctime24_of(t: SystemTime) -> Option<String> {
    // CVS writes the repository's revision time, which CVSNT sends as UTC (`Mod-time ... -0000`)
    // and writes into Entries unchanged. The real working copy proves it: Entries says
    // 01:19:30 while the file on disk says 09:19:30 in China Standard Time. Comparing the
    // file's *local* asctime would mark every unchanged file as modified.
    let dt: DateTime<Utc> = DateTime::<Utc>::from(t);
    Some(asctime24(dt.naive_utc()))
}

/// The 24-character timestamp CVS writes into `CVS/Entries` for a file with this mtime.
pub fn entry_stamp(t: SystemTime) -> String {
    asctime24_of(t).unwrap_or_default()
}

/// The timestamp CVS writes into `CVS/Entries`, in any of the shapes seen in the wild.
pub fn parse_entry_date(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    // asctime's shape, with the day-of-month column padded with a space ("Wed Nov  4 ...").
    // Collapse the runs of spaces so the same parser accepts both paddings.
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Ok(dt) = NaiveDateTime::parse_from_str(&flat, "%a %b %d %H:%M:%S %Y") {
        return Some(dt);
    }
    for f in [
        "%m/%d/%Y %H:%M:%S",
        "%Y.%m.%d.%H.%M.%S",
        "%Y-%m-%d %H:%M:%S",
        "%m/%d/%Y %H:%M",
        "%Y.%m.%d.%H.%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, f) {
            return Some(dt);
        }
    }
    None
}

/// Compare two asctime strings over their first 24 characters, treating a '0' in the
/// day-of-month column ("05") as equal to a space (" 5").
pub fn timestamps_equal(a: &str, b: &str) -> bool {
    let mut a: Vec<u8> = a.as_bytes().to_vec();
    let mut b: Vec<u8> = b.as_bytes().to_vec();
    if a.len() < 24 || b.len() < 24 {
        return a == b;
    }
    if a[8] == b'0' && b[8] == b' ' {
        a[8] = b' ';
    }
    if a[8] == b' ' && b[8] == b'0' {
        b[8] = b' ';
    }
    a[..24] == b[..24]
}

/// How many files could not be examined, and the first reason. A folder on a dropped
/// network share would otherwise write one warning for every file in it.
#[derive(Default)]
struct Unreadable {
    count: usize,
    first: Option<std::io::ErrorKind>,
}

fn state_for(dir: &Path, e: &Entry, unreadable: &mut Unreadable) -> State {
    if e.is_dir {
        // A "D" entry records a subfolder CVS looks after. If that folder is gone from disk it
        // is missing, exactly as a deleted file would be; saying "Folder" for something that
        // is not there hides a real difference from what was checked out.
        return if dir.join(&e.name).is_dir() {
            State::Folder
        } else {
            State::Missing
        };
    }
    let rev = e.revision.trim();
    if rev == "0" {
        // "cvs add" records revision 0 with a dummy timestamp until the first commit.
        return State::Added;
    }
    if rev.starts_with("-1") {
        return State::Removed;
    }
    if e.date.starts_with('/') {
        // The entry was already patched by an update that could not finish.
        return State::NeedsPatch;
    }
    if e.date
        .trim()
        .to_ascii_lowercase()
        .starts_with("result of merge")
    {
        // CVSNT writes "Result of merge+Tue Nov 24 18:07:25 2026" after a conflicted merge;
        // an older CVS wrote the bare words. Both mean the same thing.
        return State::Conflict;
    }
    let file = dir.join(&e.name);
    match fs::metadata(&file) {
        Err(error) => {
            // A deleted file is expected (it is what "missing" means), but a permission
            // or sharing failure looks identical in the list. Count those for one line;
            // naming every file would turn the log into an operation trail.
            if error.kind() != std::io::ErrorKind::NotFound {
                unreadable.count += 1;
                unreadable.first.get_or_insert(error.kind());
            }
            State::Missing
        }
        Ok(m) => match (
            m.modified().ok().and_then(asctime24_of),
            parse_entry_date(&e.date),
        ) {
            (Some(disk), Some(recorded)) => {
                if timestamps_equal(&disk, &asctime24(recorded)) {
                    State::Unchanged
                } else {
                    State::Modified
                }
            }
            // Unreadable clock or an unknown timestamp shape: treat as changed so the
            // person looks at it, rather than hiding a change.
            _ => State::Modified,
        },
    }
}

/// Read a control file, decoding GBK or UTF-8 as it actually is on disk.
fn read_text(path: &Path) -> Option<String> {
    read_text_noted(path).map(|(text, _)| text)
}

/// The same, also saying how the bytes were read, so the caller can mention it.
fn read_text_noted(path: &Path) -> Option<(String, crate::text::Decoded)> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            // A missing control file is normal (a plain folder has no CVS/Entries); any
            // other failure is why the list looks wrong and belongs in the log. The file
            // name is the diagnostic value here, and it is one of our own control files.
            if error.kind() != std::io::ErrorKind::NotFound {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?");
                log::warn!(
                    target: "cvsdesk::folder",
                    "phase=read-control file={} {}",
                    crate::diag::file_name(name),
                    crate::diag::io_error(&error)
                );
            }
            return None;
        }
    };
    let d = crate::text::decode(&bytes);
    Some((d.text.clone(), d))
}

/// Is this the kind of entry Windows Explorer keeps out of sight? A name beginning with
/// `.` counts everywhere, and on Windows the file's own hidden attribute counts too.
fn is_hidden(path: &Path, name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(target_os = "windows")]
    if let Ok(meta) = fs::metadata(path) {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_HIDDEN
        return meta.file_attributes() & 0x2 != 0;
    }
    #[cfg(not(target_os = "windows"))]
    let _ = path;
    false
}

/// Read a folder: its control files, its entries and any files that CVS does not know.
pub fn load(dir: &Path) -> Loaded {
    let cvs_dir = dir.join("CVS");
    let has_cvs = cvs_dir.is_dir();
    let partial_checkout = cvs_dir.join("Entries.Static").is_file();
    // `CVS/Tag` is the folder's sticky tag; its first line is the marker the update
    // protocol sends. A missing file is the normal HEAD case.
    let sticky = read_text(&cvs_dir.join("Tag"))
        .and_then(|text| text.lines().next().map(|line| line.trim().to_string()))
        .filter(|line| !line.is_empty())
        .unwrap_or_default();
    let mut notes = Vec::new();

    let entries_path = cvs_dir.join("Entries");
    // Say how the Entries file was read: CVSNT writes it in the console code page, and a
    // reader that assumed UTF-8 would show Chinese names as replacement marks. The same
    // read feeds the parser; asking for the file twice would log a failure twice.
    let entries_read = read_text_noted(&entries_path);
    if let Some((_, d)) = &entries_read {
        if let Some(note) = d.note() {
            notes.push(format!("Entries: {}", note));
        }
    }
    let (mut entries, dupes, read_ok) = match entries_read {
        Some((text, _)) => {
            let (e, d) = parse_entries(&text);
            (e, d, true)
        }
        None => (Vec::new(), 0, false),
    };
    // A command that died before merging `CVS/Entries.Log` leaves the journal behind, and
    // cvs itself applies it at the next open. The list has to see the same working copy.
    if let Some(text) = read_text(&cvs_dir.join("Entries.Log")) {
        let applied = apply_pending_log(&mut entries, &text);
        if applied > 0 {
            notes.push(format!(
                "Note: applied {} unfinished change(s) from 'CVS/Entries.Log' - an earlier cvs command did not finish",
                applied
            ));
        }
    }
    if has_cvs && !read_ok {
        notes.push(format!(
            "Warning : cannot read the 'Entries' file in '{}'",
            dir.display()
        ));
    }
    if dupes > 0 {
        notes.push(format!(
            "Warning : duplicated entry in the 'CVS/Entries' file in directory '{}'",
            dir.display()
        ));
    }

    let root = read_text(&cvs_dir.join("Root"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let repository = read_text(&cvs_dir.join("Repository"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let mut rows: Vec<Row> = Vec::new();
    let mut known: HashSet<String> = HashSet::new();
    let mut unreadable = Unreadable::default();

    for e in &entries {
        known.insert(e.name.clone());
        // A tracked file that is hidden on disk is hidden from the list too, the same
        // way Explorer treats it. Its name still counts as known, so the loose-file
        // pass cannot bring it back as "untracked".
        if is_hidden(&dir.join(&e.name), &e.name) {
            continue;
        }
        rows.push(Row {
            name: e.name.clone(),
            is_dir: e.is_dir,
            state: state_for(dir, e, &mut unreadable),
            origin: Origin::Local,
            revision: e.revision.clone(),
            tag: e.tag.clone(),
            options: e.options.clone(),
            timestamp: e.date.clone(),
            binary: e.binary(),
            unicode: e.unicode(),
        });
    }
    if unreadable.count > 0 {
        log::warn!(
            target: "cvsdesk::folder",
            "phase=state files={} kind={:?}",
            unreadable.count,
            unreadable.first
        );
    }

    // Files sitting in the folder that CVS has no entry for.
    let mut loose: Vec<Row> = Vec::new();
    match fs::read_dir(dir) {
        Ok(rd) => {
            for item in rd.flatten() {
                let name = item.file_name().to_string_lossy().to_string();
                if name == "CVS" || is_hidden(&item.path(), &name) || known.contains(&name) {
                    continue;
                }
                let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
                loose.push(Row {
                    name,
                    is_dir,
                    state: State::Unknown,
                    origin: Origin::Local,
                    revision: String::new(),
                    tag: String::new(),
                    options: String::new(),
                    timestamp: mtime_text(&item.path()),
                    binary: false,
                    unicode: false,
                });
            }
        }
        Err(error) => {
            // Without this the list comes back with only the recorded entries, looking
            // like a folder that lost every untracked file.
            log::warn!(
                target: "cvsdesk::folder",
                "phase=read-dir {}",
                crate::diag::io_error(&error)
            );
        }
    }
    rows.extend(loose);

    if !has_cvs {
        notes.push(format!(
            "Warning : no 'CVS' control folder in '{}' - this is not a project folder yet",
            dir.display()
        ));
    }

    Loaded {
        rows,
        notes,
        has_cvs,
        root,
        repository,
        sticky,
        partial_checkout,
    }
}

fn mtime_text(p: &Path) -> String {
    fs::metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(asctime24_of)
        .unwrap_or_default()
}

/// The connection kind named by a CVSROOT string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnKind {
    Pserver,
    Kserver,
    Local,
    Ext,
    Ntserver,
    Gserver,
    Sspi,
    Server,
}

impl ConnKind {
    pub const ALL: [ConnKind; 8] = [
        ConnKind::Pserver,
        ConnKind::Kserver,
        ConnKind::Local,
        ConnKind::Ext,
        ConnKind::Ntserver,
        ConnKind::Gserver,
        ConnKind::Sspi,
        ConnKind::Server,
    ];

    pub fn token(self) -> &'static str {
        match self {
            ConnKind::Pserver => "pserver",
            ConnKind::Kserver => "kserver",
            ConnKind::Local => "local",
            ConnKind::Ext => "ext",
            ConnKind::Ntserver => "ntserver",
            ConnKind::Gserver => "gserver",
            ConnKind::Sspi => "sspi",
            ConnKind::Server => "server",
        }
    }

    pub fn label(self) -> &'static str {
        self.token()
    }

    pub fn remote(self) -> bool {
        !matches!(self, ConnKind::Local)
    }

    /// The kind named by a settings value, for reading `P_ConnKind` back.
    pub fn from_token(s: &str) -> Option<ConnKind> {
        let s = s.trim().to_ascii_lowercase();
        ConnKind::ALL.iter().copied().find(|k| k.token() == s)
    }
}

/// `:kind:[user@]host[:port]:/path`, with `:pserver:proxy=HOST:PORT:` when a proxy is set.
pub fn build_cvsroot(
    kind: ConnKind,
    user: &str,
    host: &str,
    port: &str,
    path: &str,
    proxy: Option<(&str, &str)>,
) -> String {
    let mut s = String::new();
    s.push(':');
    s.push_str(kind.token());
    s.push(':');
    if let Some((ph, pp)) = proxy {
        if kind == ConnKind::Pserver && !ph.trim().is_empty() {
            s.push_str("proxy=");
            s.push_str(ph.trim());
            s.push(':');
            s.push_str(pp.trim());
            s.push(':');
        }
    }
    let user = user.trim();
    if kind.remote() && !user.is_empty() {
        s.push_str(user);
        s.push('@');
    }
    if kind.remote() {
        s.push_str(host.trim());
        let port = port.trim();
        if !port.is_empty() {
            s.push(':');
            s.push_str(port);
        }
        s.push(':');
    }
    s.push_str(path.trim());
    s
}

/// The root to keep: parsed and stripped of a password when possible, otherwise the
/// lenient redaction. A valid local path or a path containing `@` is never rewritten.
pub fn clean_root(root: &str) -> String {
    let root = root.trim();
    if root.is_empty() {
        return String::new();
    }
    if !root.starts_with(':') {
        if let Some((token, _)) = root.split_once(':') {
            if ConnKind::from_token(token).is_some_and(ConnKind::remote) {
                return redact_root_password(root);
            }
        }
    }
    crate::client::root::Root::parse(root)
        .map(|parsed| parsed.key)
        .unwrap_or_else(|_| redact_root_password(root))
}

/// Remove a password from a CVSROOT even when the rest of the root is malformed, so an
/// error message or a settings file can never carry one. The text around the password is
/// left exactly as it was.
pub fn redact_root_password(raw: &str) -> String {
    let raw = raw.trim();
    let (kind, head_len, rest) = if let Some(stripped) = raw.strip_prefix(':') {
        let Some(end) = stripped.find(':') else {
            return raw.to_string();
        };
        let token = &stripped[..end];
        let kind = ConnKind::ALL
            .iter()
            .find(|kind| kind.token() == token)
            .copied()
            .unwrap_or(ConnKind::Pserver);
        (kind, end + 2, &raw[end + 2..])
    } else {
        let Some((token, rest)) = raw.split_once(':') else {
            return raw.to_string();
        };
        let Some(kind) = ConnKind::from_token(token) else {
            return raw.to_string();
        };
        (kind, token.len() + 1, rest)
    };
    if !kind.remote() {
        return raw.to_string();
    }
    let (proxy, rest) = split_proxy_prefix(rest);
    let Some((before, after)) = redact_userinfo(rest) else {
        return raw.to_string();
    };
    format!("{}{}{}{}", &raw[..head_len], proxy, before, after)
}

/// The part before a password colon, and the part from the `@` after it.
fn redact_userinfo(rest: &str) -> Option<(String, String)> {
    let path_start = rest.find('/').unwrap_or(rest.len());
    let at = rest[..path_start].find('@')?;
    let info = &rest[..at];
    let colon = info.find(':')?;
    Some((rest[..colon].to_string(), rest[at..].to_string()))
}

/// Split a leading `proxy=HOST[:PORT]:` from a root's remaining text.
fn split_proxy_prefix(rest: &str) -> (&str, &str) {
    let Some(after) = rest.strip_prefix("proxy=") else {
        return ("", rest);
    };
    let Some(host_end) = after.find(':') else {
        return ("", rest);
    };
    let after_host = &after[host_end + 1..];
    let end = match after_host.find(':') {
        Some(second) => {
            let port = &after_host[..second];
            if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
                // proxy=HOST:PORT:
                "proxy=".len() + host_end + 1 + second + 1
            } else {
                // proxy=HOST:userinfo... (no port)
                "proxy=".len() + host_end + 1
            }
        }
        None => "proxy=".len() + host_end + 1,
    };
    (&rest[..end], &rest[end..])
}

/// Pull the pieces of an existing CVSROOT string back out, so the dialog can show them.
/// A password in the userinfo is not returned with the user: it must never be shown.
pub fn parse_cvsroot(root: &str) -> (ConnKind, String, String, String, String) {
    let trimmed = root.trim();
    if !trimmed.starts_with(':') {
        // A bare path is a local root, exactly as `Root::parse` reads it.
        return (
            ConnKind::Local,
            String::new(),
            String::new(),
            String::new(),
            trimmed.to_string(),
        );
    }
    let mut kind = ConnKind::Pserver;
    let mut rest = trimmed;
    if let Some(stripped) = rest.strip_prefix(':') {
        if let Some(end) = stripped.find(':') {
            let token = &stripped[..end];
            if let Some(k) = ConnKind::ALL.iter().find(|k| k.token() == token) {
                kind = *k;
            }
            rest = &stripped[end + 1..];
        }
    }
    if !kind.remote() {
        return (
            kind,
            String::new(),
            String::new(),
            String::new(),
            rest.to_string(),
        );
    }
    // A proxy is written before the userinfo: `proxy=HOST[:PORT]:user@host[:port]:path`.
    rest = split_proxy_prefix(rest).1;
    // The userinfo ends at the first slash. A Windows path may have no slash, so the host
    // splitter below finds the path after the host.
    let path_start = rest.find('/').unwrap_or(rest.len());
    let (userinfo, host_start) = match rest[..path_start].find('@') {
        Some(at) => (&rest[..at], at + 1),
        None => ("", 0),
    };
    // The userinfo may carry a password as `user:password`; only the account is shown.
    let user = userinfo.split(':').next().unwrap_or("").trim();
    let host_part = &rest[host_start..];
    let Some((host, port, path)) = crate::client::root::split_host_path(host_part) else {
        return (
            kind,
            user.to_string(),
            host_part.to_string(),
            String::new(),
            String::new(),
        );
    };
    (
        kind,
        user.to_string(),
        host.to_string(),
        port.unwrap_or("").to_string(),
        path.to_string(),
    )
}

/// Best-effort local-time string for display, used by the status bar.
pub fn now_text() -> String {
    let now: DateTime<Local> = Local::now();
    now.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Parse a CVS timestamp into a local datetime, for sorting and display.
pub fn localise(n: &NaiveDateTime) -> DateTime<Local> {
    Local
        .from_local_datetime(n)
        .single()
        .unwrap_or_else(|| Local.from_utc_datetime(n))
}

/// Re-read one of CVS/Entries' 24-character timestamps and show it the way a person reads
/// dates, e.g. "2026-11-24 09:14". Falls back to the raw text when it does not parse, so a
/// special field ("Result of merge", a leading '/') is never lost.
pub fn friendly_time(recorded: &str) -> String {
    let raw = recorded.trim();
    if raw.is_empty() {
        return String::new();
    }
    // cvs writes asctime's own shape, "Sat Nov 21 09:14:03 2026", with the day-of-month
    // column space-padded ("Sat Nov  5 ..."). The weekday is dropped before parsing so a
    // disagreeing one cannot lose the date.
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let parts: Vec<&str> = flat.splitn(2, ' ').collect();
    let rest = match parts.first() {
        Some(w)
            if w.len() == 3 && w.chars().all(|c| c.is_ascii_alphabetic()) && parts.len() == 2 =>
        {
            parts[1]
        }
        _ => flat.as_str(),
    };
    for shape in ["%b %d %H:%M:%S %Y", "%b %d %H:%M %Y", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(rest, shape) {
            return localise(&n).format("%Y-%m-%d %H:%M").to_string();
        }
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, rev: &str, date: &str) -> Entry {
        Entry {
            name: name.to_string(),
            revision: rev.to_string(),
            date: date.to_string(),
            options: String::new(),
            tag: String::new(),
            is_dir: false,
        }
    }

    fn state(dir: &Path, entry: &Entry) -> State {
        state_for(dir, entry, &mut Unreadable::default())
    }

    #[test]
    fn asctime_is_24_characters() {
        let n = NaiveDateTime::parse_from_str("2026-11-05 09:14:03", "%Y-%m-%d %H:%M:%S").unwrap();
        let s = asctime24(n);
        assert_eq!(s.len(), 24, "the comparison reads exactly 24 characters");
        assert_eq!(s, "Thu Nov  5 09:14:03 2026");
    }

    #[test]
    fn a_padded_day_equals_a_space_padded_one() {
        // The comparison normalises '0' against ' ' in the day column.
        assert!(timestamps_equal(
            "Thu Nov 05 09:14:03 2026",
            "Thu Nov  5 09:14:03 2026"
        ));
        assert!(timestamps_equal(
            "Thu Nov  5 09:14:03 2026",
            "Thu Nov 05 09:14:03 2026"
        ));
    }

    #[test]
    fn differing_times_are_not_equal() {
        assert!(!timestamps_equal(
            "Thu Nov  5 09:14:03 2026",
            "Thu Nov  5 09:14:04 2026"
        ));
        assert!(!timestamps_equal("short", "also short"));
        assert!(timestamps_equal("same", "same"));
    }

    #[test]
    fn states_follow_the_recorded_fields() {
        // Its own folder, so nothing already sitting in the machine's temp folder can decide
        // the answer. (An earlier version used the shared temp folder and passed or failed
        // depending on what an earlier run had left behind.)
        let dir = std::env::temp_dir().join(format!("cvsdesk-states-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            state(&dir, &entry("a.c", "0", "")),
            State::Added,
            "revision 0 means added, not yet committed"
        );
        assert_eq!(
            state(&dir, &entry("b.c", "-1.2", "")),
            State::Removed,
            "a revision starting -1 means removed"
        );
        assert_eq!(
            state(&dir, &entry("c.c", "1.4", "/patched")),
            State::NeedsPatch,
            "a date starting with / means the update could not finish"
        );
        assert_eq!(
            state(&dir, &entry("d.c", "1.4", "Result of merge")),
            State::Conflict
        );
        assert_eq!(
            state(
                &dir,
                &entry("d2.c", "1.4", "Result of merge+Tue Nov 24 18:07:25 2026")
            ),
            State::Conflict,
            "CVSNT writes the merge time after 'Result of merge+'"
        );
        let mut folder = entry("sub", "", "");
        folder.is_dir = true;
        assert_eq!(
            state(&dir, &folder),
            State::Missing,
            "a D entry whose folder is not on disk is missing, not merely a folder"
        );
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        assert_eq!(
            state(&dir, &folder),
            State::Folder,
            "a D entry is a folder while that folder is really there"
        );
        assert_eq!(
            state(&dir, &entry("gone.c", "1.4", "11/22/2026 09:14:03")),
            State::Missing,
            "a recorded file that is not on disk is missing"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn entry_dates_in_every_known_shape_parse() {
        for s in [
            "11/22/2026 09:14:03",
            "2026.11.22.09.14.03",
            "2026-11-22 09:14:03",
            "11/22/2026 09:14",
            "Tue Nov 24 09:52:53 2026",
            "Wed Nov  4 09:52:53 2026",
        ] {
            assert!(parse_entry_date(s).is_some(), "should parse: {}", s);
        }
        assert!(parse_entry_date("Result of merge").is_none());
    }

    #[test]
    fn the_recorded_time_is_utc_so_a_fresh_checkout_is_unchanged() {
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-utc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        let stamp = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let handle = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        handle
            .set_times(std::fs::FileTimes::new().set_modified(stamp))
            .unwrap();
        // The date CVSNT writes is the UTC text of the same instant, not the local one.
        let utc: DateTime<Utc> = stamp.into();
        let recorded = asctime24(utc.naive_utc());
        assert_eq!(
            state(&dir, &entry("a.txt", "1.1", &recorded)),
            State::Unchanged,
            "recorded {} should match the file's UTC time",
            recorded
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_root_password_is_not_part_of_the_displayed_user() {
        let (kind, user, host, port, path) = parse_cvsroot(":pserver:ann:secret@host:2401:/repo");
        assert_eq!(kind, ConnKind::Pserver);
        assert_eq!(user, "ann");
        assert_eq!(host, "host");
        assert_eq!(port, "2401");
        assert_eq!(path, "/repo");
    }

    #[test]
    fn a_root_with_a_port_keeps_its_path() {
        let (kind, user, host, port, path) = parse_cvsroot(":kserver:ann@cvshost:2402:/repo");
        assert_eq!(kind, ConnKind::Kserver);
        assert_eq!(user, "ann");
        assert_eq!(host, "cvshost");
        assert_eq!(port, "2402");
        assert_eq!(path, "/repo");
    }

    #[test]
    fn a_path_may_contain_a_colon() {
        let (kind, user, host, port, path) = parse_cvsroot(":ext:ann@buildhost:C:\\cvs\\repo");
        assert_eq!(kind, ConnKind::Ext);
        assert_eq!(user, "ann");
        assert_eq!(host, "buildhost");
        assert!(port.is_empty());
        assert_eq!(path, "C:\\cvs\\repo");
    }

    #[test]
    fn a_malformed_root_password_is_still_redacted() {
        assert_eq!(
            redact_root_password(":pserver:ann:secret@host"),
            ":pserver:ann@host"
        );
        assert_eq!(
            redact_root_password(":pserver:ann:secret@host:abc:/repo"),
            ":pserver:ann@host:abc:/repo"
        );
        assert_eq!(
            redact_root_password(":pserver:proxy=proxyhost:8080:ann:secret@host:/repo"),
            ":pserver:proxy=proxyhost:8080:ann@host:/repo"
        );
        assert_eq!(
            redact_root_password(":pserver:ann@host:/repo"),
            ":pserver:ann@host:/repo"
        );
        assert_eq!(redact_root_password("C:\\work"), "C:\\work");
    }

    #[test]
    fn a_valid_root_with_an_at_in_the_path_is_not_corrupted() {
        assert_eq!(
            clean_root(":pserver:ann@host:C:\\repos\\team@corp\\repo"),
            ":pserver:ann@host:C:\\repos\\team@corp\\repo"
        );
        assert_eq!(
            clean_root(":local:C:\\Users\\ann@company.com\\repo"),
            ":local:C:\\Users\\ann@company.com\\repo"
        );
    }

    #[test]
    fn a_malformed_proxy_password_is_redacted() {
        assert_eq!(
            clean_root(":pserver:proxy=proxyhost:ann:secret@host:/repo"),
            ":pserver:proxy=proxyhost:ann@host:/repo"
        );
    }

    #[test]
    fn a_root_without_its_leading_colon_is_still_redacted() {
        assert_eq!(
            clean_root("pserver:ann:secret@host:/repo"),
            "pserver:ann@host:/repo"
        );
    }

    #[test]
    fn a_path_with_an_at_sign_keeps_its_host_and_path() {
        let (kind, user, host, port, path) =
            parse_cvsroot(":pserver:ann@host:C:\\repos\\team@corp\\repo");
        assert_eq!(kind, ConnKind::Pserver);
        assert_eq!(user, "ann");
        assert_eq!(host, "host");
        assert!(port.is_empty());
        assert_eq!(path, "C:\\repos\\team@corp\\repo");
    }

    #[test]
    fn a_bare_path_is_a_local_root() {
        let (kind, user, host, port, path) = parse_cvsroot("C:\\work");
        assert_eq!(kind, ConnKind::Local);
        assert!(user.is_empty());
        assert!(host.is_empty());
        assert!(port.is_empty());
        assert_eq!(path, "C:\\work");
    }

    #[test]
    fn a_proxied_root_keeps_the_account_and_path() {
        let (kind, user, host, port, path) =
            parse_cvsroot(":pserver:proxy=proxyhost:8080:ann@host:/repo");
        assert_eq!(kind, ConnKind::Pserver);
        assert_eq!(user, "ann");
        assert_eq!(host, "host");
        assert!(port.is_empty());
        assert_eq!(path, "/repo");
    }

    #[test]
    fn friendly_time_falls_back_to_the_recorded_text() {
        assert_eq!(friendly_time(""), "");
        assert_eq!(friendly_time("Result of merge"), "Result of merge");
        assert_eq!(friendly_time("/patched"), "/patched");
        assert_eq!(
            friendly_time("Sat Nov 22 09:14:03 2026"),
            "2026-11-22 09:14"
        );
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        load(dir).rows.into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn a_folder_left_in_entries_log_is_part_of_the_project() {
        // A real working copy can contain exactly this: a folder checked out, its `D` line
        // still only in the journal of an interrupted update.
        let dir = temp_dir("entries-log");
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        std::fs::write(dir.join("CVS").join("Entries"), "/a.txt/1.1///\n").unwrap();
        std::fs::write(dir.join("CVS").join("Entries.Log"), "A D/late////\r\n").unwrap();
        std::fs::create_dir_all(dir.join("late")).unwrap();
        std::fs::write(dir.join("late").join("x.txt"), b"x").unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();

        let loaded = load(&dir);
        let row = loaded
            .rows
            .iter()
            .find(|row| row.name == "late")
            .expect("the folder must be listed");
        assert_eq!(row.state, State::Folder);
        assert!(
            loaded.notes.iter().any(|n| n.contains("Entries.Log")),
            "{:?}",
            loaded.notes
        );
        // Reading alone must not settle the journal.
        assert!(dir.join("CVS").join("Entries.Log").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("CVS").join("Entries")).unwrap(),
            "/a.txt/1.1///\n"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_file_removed_in_the_log_falls_back_to_not_in_project() {
        // The journal drops the record; the file is still on disk, so cvs itself would now
        // report it as unknown - exactly what the list does.
        let dir = temp_dir("entries-log-remove");
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        std::fs::write(
            dir.join("CVS").join("Entries"),
            "/kept.txt/1.1///\n/gone.txt/1.1///\n",
        )
        .unwrap();
        std::fs::write(dir.join("CVS").join("Entries.Log"), "R /gone.txt/1.1///\n").unwrap();
        std::fs::write(dir.join("kept.txt"), b"x").unwrap();
        std::fs::write(dir.join("gone.txt"), b"x").unwrap();

        let loaded = load(&dir);
        assert!(loaded.rows.iter().any(|row| row.name == "kept.txt"));
        let gone = loaded
            .rows
            .iter()
            .find(|row| row.name == "gone.txt")
            .expect("the file is still on disk");
        assert_eq!(gone.state, State::Unknown);
        assert!(gone.revision.is_empty(), "the record is gone");

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_folder_without_a_log_has_no_note_about_one() {
        let dir = temp_dir("entries-log-absent");
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        std::fs::write(dir.join("CVS").join("Entries"), "/a.txt/1.1///\n").unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let loaded = load(&dir);
        assert!(loaded.notes.iter().all(|n| !n.contains("Entries.Log")));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dot_names_are_not_listed() {
        let dir = temp_dir("dot-hidden");
        std::fs::write(dir.join("visible.txt"), b"x").unwrap();
        std::fs::write(dir.join(".hidden.txt"), b"x").unwrap();
        std::fs::create_dir_all(dir.join(".hidden-folder")).unwrap();

        let listed = names(&dir);
        assert!(listed.contains(&"visible.txt".to_string()), "{listed:?}");
        assert!(!listed.iter().any(|n| n.starts_with('.')), "{listed:?}");

        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(target_os = "windows")]
    fn set_hidden(path: &Path, hidden: bool) {
        let flag = if hidden { "+h" } else { "-h" };
        let status = std::process::Command::new("attrib")
            .arg(flag)
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "attrib {flag} {}", path.display());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn files_and_folders_hidden_by_windows_are_not_listed() {
        let dir = temp_dir("attr-hidden");
        let secret = dir.join("secret.txt");
        let secret_folder = dir.join("secret-folder");
        std::fs::write(dir.join("visible.txt"), b"x").unwrap();
        std::fs::write(&secret, b"x").unwrap();
        std::fs::create_dir_all(&secret_folder).unwrap();
        set_hidden(&secret, true);
        set_hidden(&secret_folder, true);

        let listed = names(&dir);
        assert!(listed.contains(&"visible.txt".to_string()), "{listed:?}");
        assert!(!listed.contains(&"secret.txt".to_string()), "{listed:?}");
        assert!(!listed.contains(&"secret-folder".to_string()), "{listed:?}");

        set_hidden(&secret, false);
        set_hidden(&secret_folder, false);
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_tracked_file_hidden_on_disk_is_not_listed() {
        let dir = temp_dir("attr-entry");
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        std::fs::write(
            dir.join("CVS").join("Entries"),
            "/secret.txt/1.1/Sat Nov 22 09:14:03 2026//\n\
             /plain.txt/1.1/Sat Nov 22 09:14:03 2026//\n",
        )
        .unwrap();
        let secret = dir.join("secret.txt");
        std::fs::write(&secret, b"x").unwrap();
        std::fs::write(dir.join("plain.txt"), b"x").unwrap();
        set_hidden(&secret, true);

        let listed = names(&dir);
        assert!(!listed.contains(&"secret.txt".to_string()), "{listed:?}");
        assert!(listed.contains(&"plain.txt".to_string()), "{listed:?}");

        set_hidden(&secret, false);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn entries_static_marks_a_partial_checkout() {
        let dir = temp_dir("partial-checkout");
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        std::fs::write(dir.join("CVS").join("Entries"), "").unwrap();
        std::fs::write(dir.join("CVS").join("Entries.Static"), "").unwrap();

        assert!(load(&dir).partial_checkout);

        let plain = temp_dir("full-checkout");
        std::fs::create_dir_all(plain.join("CVS")).unwrap();
        std::fs::write(plain.join("CVS").join("Entries"), "").unwrap();
        assert!(!load(&plain).partial_checkout);

        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(plain).ok();
    }

    #[test]
    fn a_tag_file_is_the_folders_sticky_tag() {
        let dir = temp_dir("sticky-tag");
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        std::fs::write(dir.join("CVS").join("Entries"), "").unwrap();
        std::fs::write(dir.join("CVS").join("Tag"), "TBRANCH\r\n").unwrap();

        assert_eq!(load(&dir).sticky, "TBRANCH");

        std::fs::write(dir.join("CVS").join("Tag"), "  \r\n").unwrap();
        assert!(load(&dir).sticky.is_empty(), "a blank tag file means HEAD");

        std::fs::remove_file(dir.join("CVS").join("Tag")).unwrap();
        assert!(
            load(&dir).sticky.is_empty(),
            "a missing tag file means HEAD"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_list_uses_working_copy_words() {
        assert_eq!(State::Remote.label(), "Not checked out");
        assert_eq!(State::Unknown.label(), "Untracked");
        assert_eq!(State::Missing.label(), "Missing locally");
    }

    #[test]
    fn a_server_only_row_is_marked_remote_and_not_a_local_change() {
        assert_eq!(State::Remote.label(), "Not checked out");
        assert!(!State::Remote.is_change());

        let dir = temp_dir("remote-state");
        std::fs::write(dir.join("visible.txt"), b"x").unwrap();
        let loaded = load(&dir);
        assert!(loaded.rows.iter().all(|row| row.origin == Origin::Local));
        std::fs::remove_dir_all(&dir).ok();
    }
}
