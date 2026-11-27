//! The diagnostics log: what the program was doing when something went wrong.
//!
//! The log is for faults, not for history. It records phases, counts, timings and error
//! kinds; it never records the user's data: no passwords, no CVSROOT/host/user, no complete
//! paths, no commit messages and no file contents. A file name may appear when one file is
//! the failing input, and only its last path component is written.
//!
//! The `log` facade is used rather than a logging framework: eframe, wgpu and this program
//! all report through it, and a few hundred lines here own the file, its rotation and its
//! privacy rules. A program that never calls [`init`] - the `cvs` command-line client, the
//! tests - simply drops every record.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};

/// The size at which the log moves aside and a fresh one starts.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// How many files the log may span: `cvsdesk.log`, `cvsdesk.1.log`, `cvsdesk.2.log`.
pub const KEEP_LOGS: usize = 3;
/// How long an untouched log survives, in days.
pub const KEEP_DAYS: u64 = 14;

const LOG_NAME: &str = "cvsdesk.log";
const MAX_SCALAR: usize = 80;
/// The most text one record may carry. A dependency's error can quote a whole shader or
/// a file, and one such line would undo the rotation budget.
const MAX_RECORD: usize = 512;

/// True when the level came from a switch or the environment, so the Preferences check
/// may not override it.
static EXPLICIT: AtomicBool = AtomicBool::new(false);

/// The folder the program writes its diagnostics into.
///
/// `%LOCALAPPDATA%\CVSDesk\logs` is writable by the user and survives an uninstall of the
/// program, which `Program Files` next to the executable would not. The temporary folder is
/// the fallback for a machine that has no profile.
pub fn log_dir() -> PathBuf {
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(base).join("CVSDesk").join("logs");
    }
    std::env::temp_dir().join("cvsdesk").join("logs")
}

/// The current log file.
pub fn log_file() -> PathBuf {
    log_dir().join(LOG_NAME)
}

/// Start writing the log, and return the folder it lives in.
///
/// Nothing here may take the program down: a folder that cannot be created, a file that
/// cannot be opened, or a logger that was already installed all leave the caller running
/// without diagnostics. `explicit` says the level was chosen on the command line or in the
/// environment, which stops a saved preference from lowering it later.
pub fn init(level: LevelFilter, explicit: bool) -> PathBuf {
    let dir = log_dir();
    let _ = fs::create_dir_all(&dir);
    clean_old_at(&dir, SystemTime::now());
    let previous = last_line(&dir.join(LOG_NAME));

    let logger = FileLogger::at(dir.join(LOG_NAME));
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
        EXPLICIT.store(explicit, Ordering::Relaxed);
        log::info!(
            target: "cvsdesk",
            "session start version={} os={} arch={} level={} pid={}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            level,
            std::process::id()
        );
        if let Some(line) = previous {
            if !line.contains("session end") {
                log::warn!(
                    target: "cvsdesk",
                    "previous session ended unexpectedly last={}",
                    scalar(&line)
                );
            }
        }
    }
    dir
}

/// Let the saved preference turn on debug detail, unless a switch or the environment has
/// already decided the level.
pub fn enable_debug_unless_explicit() -> bool {
    if EXPLICIT.load(Ordering::Relaxed) {
        return false;
    }
    log::set_max_level(LevelFilter::Debug);
    true
}

/// Read a level name the way `--log-level` and `CVSDESK_LOG` spell it.
pub fn level_from(text: &str) -> Option<LevelFilter> {
    match text.trim().to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

/// A file name as the log carries it: the last path component, no control characters and
/// no more than eighty characters. Directories never appear.
pub fn file_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if last.is_empty() {
        return "<none>".to_string();
    }
    scalar(last)
}

/// One external string reduced to a scalar a log line can hold. Control characters become
/// `?` so a name cannot forge a line, and a long value is cut short.
pub fn scalar(text: &str) -> String {
    one_line(text, MAX_SCALAR)
}

/// A source location as the log carries it.
///
/// A path inside this program keeps its relative form. A dependency's absolute path keeps
/// only its last three components, so no drive letter, UNC share or profile name reaches
/// the file when a font parser or a GPU driver panics.
pub fn source_file(file: &str) -> String {
    let absolute =
        file.as_bytes().get(1) == Some(&b':') || file.starts_with("\\\\") || file.starts_with('/');
    if !absolute {
        return scalar(file);
    }
    let mut parts: Vec<&str> = file.rsplit(['/', '\\']).take(3).collect();
    parts.reverse();
    scalar(&parts.join("/"))
}

/// External text with everything from its first path-like start replaced by `<path>`.
///
/// A panic message or a dependency's error can name a file. Where a path ends cannot be
/// known in general (spaces are legal in one), so the rest of the message is dropped: what
/// came before it still says what failed, and nothing after it can leak.
pub fn redact_paths(text: &str) -> String {
    match path_start(text) {
        Some(start) => {
            let mut kept = String::with_capacity(start + 8);
            kept.push_str(&text[..start]);
            kept.push_str("<path>");
            scalar(&kept)
        }
        None => scalar(text),
    }
}

/// The byte index of the first path-looking token: `C:\`, `C:/`, `\\server`, or a POSIX
/// home or system root.
fn path_start(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    for index in 0..bytes.len() {
        if index + 1 < bytes.len()
            && bytes[index].is_ascii_alphabetic()
            && bytes[index + 1] == b':'
            && matches!(bytes.get(index + 2), Some(&b'\\') | Some(&b'/'))
        {
            return Some(index);
        }
        if index + 1 < bytes.len() && bytes[index] == b'\\' && bytes[index + 1] == b'\\' {
            return Some(index);
        }
    }
    let mut earliest = None;
    for needle in [
        "/home/",
        "/Users/",
        "/root/",
        "/var/",
        "/tmp/",
        "/opt/",
        "/mnt/",
        "/Volumes/",
        "/private/",
        "~/",
    ] {
        if let Some(index) = text.find(needle) {
            earliest = Some(earliest.map_or(index, |best: usize| best.min(index)));
        }
    }
    earliest
}

/// One line of text, control characters replaced and the length capped. Used for whole
/// records as well as scalars, so a dependency cannot forge a line or flood the file.
fn one_line(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max));
    for (taken, ch) in text.chars().enumerate() {
        if taken == max {
            out.push_str("...");
            break;
        }
        out.push(if ch.is_control() { '?' } else { ch });
    }
    out
}

/// An I/O error as the log carries it: the kind and the system code, never the message,
/// which can name a path.
pub fn io_error(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("kind={:?} code={}", error.kind(), code),
        None => format!("kind={:?}", error.kind()),
    }
}

/// The header and the last `max_lines` lines of the log, for the clipboard.
pub fn diagnostics_tail(max_lines: usize) -> String {
    let mut text = format!(
        "CVS Desk {} ({} {})\n\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    text.push_str(&tail_of(&log_file(), max_lines));
    text
}

/// Install a panic hook that records where the program died.
///
/// The hook writes the file directly instead of going through [`log`]: a panic can happen
/// while the logger holds its lock, and waiting for that lock would deadlock the process
/// instead of reporting it. A panic is rare enough that a separate append is cheap.
pub fn install_panic_hook() {
    let path = log_file();
    // The standard hook still runs: it prints the thread name and the backtrace when
    // `RUST_BACKTRACE` asks for one, which a file line cannot replace.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    source_file(location.file()),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown".to_string());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic payload");
        let line = format!(
            "{} ERROR s={} cvsdesk::panic location={} message={}\n",
            stamp(),
            session(),
            scalar(&location),
            redact_paths(message)
        );
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = open_append(&path) {
            let _ = file.write_all(line.as_bytes());
        }
        previous(info);
    }));
}

/// Open the log for appending, letting another instance rotate it while this one holds it.
///
/// Windows refuses to rename a file that another handle opened without `FILE_SHARE_DELETE`,
/// and a second window would otherwise make rotation fail on every write. Two windows still
/// interleave lines, which is acceptable for a diagnostics file.
fn open_append(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        options.share_mode(0x1 | 0x2 | 0x4);
    }
    options.open(path)
}

// -- the file -----------------------------------------------------------------

struct FileLogger {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    file: Option<File>,
    bytes: u64,
    /// Set when a rotation could not make room. Writing on would break the size budget
    /// and retry the renames on every line, so this run stops logging instead.
    full: bool,
}

impl FileLogger {
    fn at(path: PathBuf) -> Self {
        Self {
            path,
            state: Mutex::new(State::default()),
        }
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let target = metadata.target();
        if target == "cvsdesk" || target.starts_with("cvsdesk::") {
            return true;
        }
        // A dependency's text is outside this module's privacy rules: one `Path::display()`
        // in a warning would put a full path in a bug report. GPU failures are the only
        // dependency reports a user needs, and only at warning or error.
        metadata.level() <= Level::Warn
            && (target.starts_with("wgpu") || target.starts_with("naga"))
    }

    fn log(&self, record: &Record<'_>) {
        // `log` calls `log` directly without asking `enabled` first, so the filter has
        // to run here as well as in `enabled`.
        if !self.enabled(record.metadata()) {
            return;
        }
        let args = one_line(&record.args().to_string(), MAX_RECORD);
        let mut line = String::with_capacity(128 + args.len());
        let _ = write!(
            line,
            "{} {:<5} s={} {} ",
            stamp(),
            record.level(),
            session(),
            record.target()
        );
        line.push_str(&args);
        line.push('\n');
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.write(&self.path, line.as_bytes());
    }

    fn flush(&self) {}
}

impl State {
    fn write(&mut self, path: &Path, line: &[u8]) {
        if self.full {
            return;
        }
        if self.file.is_none() {
            self.open(path);
        }
        if self.bytes > 0 && self.bytes + line.len() as u64 > MAX_BYTES {
            self.rotate(path);
            self.open(path);
            if self.bytes + line.len() as u64 > MAX_BYTES {
                // Something else holds the file (a viewer, a second build without
                // delete sharing, a scanner). Keep what is there instead of growing
                // without bound and retrying the rename for every line.
                self.full = true;
                self.file = None;
                return;
            }
        }
        if let Some(file) = self.file.as_mut() {
            if file.write_all(line).is_ok() {
                self.bytes += line.len() as u64;
            }
        }
    }

    fn open(&mut self, path: &Path) {
        if let Ok(file) = open_append(path) {
            self.bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
            self.file = Some(file);
        }
    }

    /// Move `cvsdesk.log` to `cvsdesk.1.log`, the old `.1` to `.2`, and drop the oldest.
    /// A rename that fails is not worth reporting: the next line still gets written.
    fn rotate(&mut self, path: &Path) {
        self.file = None;
        self.bytes = 0;
        let newest = KEEP_LOGS.saturating_sub(1);
        if newest == 0 {
            let _ = fs::remove_file(path);
            return;
        }
        let _ = fs::remove_file(rotated(path, newest));
        for number in (1..newest).rev() {
            let _ = fs::rename(rotated(path, number), rotated(path, number + 1));
        }
        let _ = fs::rename(path, rotated(path, 1));
    }
}

/// `cvsdesk.log` -> `cvsdesk.1.log`.
fn rotated(path: &Path, number: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("cvsdesk");
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{}.{}.log", stem, number))
}

fn clean_old_at(dir: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let max_age = Duration::from_secs(KEEP_DAYS * 24 * 60 * 60);
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("cvsdesk") || !name.ends_with(".log") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(now);
        if now.duration_since(modified).unwrap_or_default() >= max_age {
            let _ = fs::remove_file(&path);
        }
    }
}

fn last_line(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.to_string())
}

fn tail_of(path: &Path, max_lines: usize) -> String {
    match fs::read(path) {
        Ok(bytes) => {
            // A crash in the middle of a write can leave half a UTF-8 character; the
            // rest of the file is still worth copying.
            let body = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = body.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            let mut text = lines[start..].join("\n");
            if !text.is_empty() {
                text.push('\n');
            }
            text
        }
        Err(error) => format!("(the log could not be read: {})\n", io_error(&error)),
    }
}

fn stamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// A short id for this run of the program, so two sessions in one log can be told apart.
fn session() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let mixed = (std::process::id() as u128).wrapping_mul(2_654_435_761) ^ nanos;
        format!("{:06x}", (mixed & 0xff_ffff) as u32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Level;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-diag-{}-{name}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_record(logger: &FileLogger, text: &str) {
        let args = format_args!("{}", text);
        let record = log::Record::builder()
            .args(args)
            .level(Level::Info)
            .target("cvsdesk::test")
            .build();
        Log::log(logger, &record);
    }

    #[test]
    fn a_file_name_keeps_only_its_last_component() {
        assert_eq!(file_name("C:\\work\\src\\main.c"), "main.c");
        assert_eq!(file_name("/home/ann/repo/a b.txt"), "a b.txt");
        assert_eq!(file_name("main.c"), "main.c");
    }

    #[test]
    fn a_file_name_cannot_forge_a_log_line() {
        let name = file_name("bad\nINFO forged\r\tentry");
        assert!(!name.contains('\n'), "{:?}", name);
        assert!(!name.contains('\r'), "{:?}", name);
        assert!(!name.contains('\t'), "{:?}", name);
    }

    #[test]
    fn a_long_file_name_is_cut_short() {
        let name = file_name(&"a".repeat(200));
        assert!(name.chars().count() <= 83, "{:?}", name);
        assert!(name.ends_with("..."), "{:?}", name);
    }

    #[test]
    fn an_empty_file_name_is_still_something() {
        assert_eq!(file_name(""), "<none>");
        assert_eq!(file_name("C:\\work\\"), "<none>");
    }

    #[test]
    fn an_io_error_is_described_without_its_message() {
        let error = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "C:\\Users\\ann\\secret.txt",
        );
        let text = io_error(&error);
        assert!(text.contains("PermissionDenied"), "{}", text);
        assert!(!text.contains("secret"), "{}", text);
        assert!(!text.contains("Users"), "{}", text);
    }

    #[test]
    fn the_log_level_is_read_without_case() {
        assert_eq!(level_from("DEBUG"), Some(LevelFilter::Debug));
        assert_eq!(level_from("warn"), Some(LevelFilter::Warn));
        assert_eq!(level_from(" off "), Some(LevelFilter::Off));
        assert_eq!(level_from("nonsense"), None);
    }

    #[test]
    fn the_log_rotates_into_numbered_files() {
        let dir = scratch("rotate");
        let path = dir.join("cvsdesk.log");
        // The record writer caps one line; rotation is the file's own rule, so it is
        // exercised directly with a line that fills the budget.
        let mut state = State::default();
        state.write(&path, &vec![b'x'; MAX_BYTES as usize]);
        state.write(&path, b"after rotation\n");
        assert!(dir.join("cvsdesk.1.log").is_file());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("after rotation"), "{}", body);
        assert!(std::fs::metadata(&path).unwrap().len() < MAX_BYTES / 2);
    }

    #[test]
    fn old_logs_are_removed() {
        let dir = scratch("clean");
        let path = dir.join("cvsdesk.log");
        std::fs::write(&path, "old").unwrap();
        let later = SystemTime::now() + Duration::from_secs(KEEP_DAYS * 24 * 60 * 60 + 60);
        clean_old_at(&dir, later);
        assert!(!path.exists());
    }

    #[test]
    fn a_dependency_warning_outside_the_gpu_is_not_kept() {
        let dir = scratch("filter");
        let path = dir.join("cvsdesk.log");
        let logger = FileLogger::at(path.clone());
        let enabled = |target: &'static str, level: Level| {
            let metadata = Metadata::builder().level(level).target(target).build();
            Log::enabled(&logger, &metadata)
        };
        assert!(
            !enabled("naga::valid::interface", Level::Debug),
            "a dependency's shader dump would fill the file"
        );
        assert!(enabled("naga::valid::interface", Level::Warn));
        assert!(enabled("wgpu_core::device", Level::Error));
        assert!(
            !enabled("eframe::native::file_storage", Level::Warn),
            "another dependency's warning may name a path"
        );
        assert!(enabled("cvsdesk::app::command", Level::Debug));
        assert!(enabled("cvsdesk::app::command", Level::Info));

        // `log` calls `log` without asking `enabled`, so the filter must hold there too.
        let record = log::Record::builder()
            .args(format_args!("shader dump"))
            .level(Level::Debug)
            .target("naga::valid::interface")
            .build();
        Log::log(&logger, &record);
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(!written.contains("shader dump"), "{}", written);
    }

    #[test]
    fn a_source_file_keeps_no_drive_or_profile() {
        assert_eq!(source_file("src/app/remote.rs"), "src/app/remote.rs");
        assert_eq!(
            source_file(r"C:\Users\ann\.cargo\registry\src\egui-0.37.0\src\lib.rs"),
            "egui-0.37.0/src/lib.rs"
        );
        assert_eq!(
            source_file(r"\\server\share\crate\src\lib.rs"),
            "crate/src/lib.rs"
        );
        let unix = source_file("/home/ann/project/src/main.rs");
        assert_eq!(unix, "project/src/main.rs");
        assert!(!unix.contains("ann"), "{}", unix);
    }

    #[test]
    fn a_panic_message_cannot_carry_a_path() {
        let text = redact_paths("cannot read C:\\Users\\ann\\secret.txt: access denied");
        assert!(text.starts_with("cannot read"), "{}", text);
        assert!(!text.contains("Users"), "{}", text);
        assert!(!text.contains("secret"), "{}", text);
        assert!(text.contains("<path>"), "{}", text);

        let unix = redact_paths("cannot read /home/ann/secret.txt");
        assert!(!unix.contains("secret"), "{}", unix);
        assert!(!unix.contains("/home"), "{}", unix);
        assert_eq!(redact_paths("index out of bounds"), "index out of bounds");
    }

    #[test]
    fn one_record_cannot_fill_the_file() {
        let dir = scratch("cap");
        let path = dir.join("cvsdesk.log");
        let logger = FileLogger::at(path.clone());
        write_record(&logger, &"x".repeat(MAX_RECORD * 4));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.len() < MAX_RECORD + 200, "{}", body.len());
        assert!(body.contains("..."), "{}", body);
        assert!(
            !body.contains("\nINFO"),
            "a record cannot forge a second line: {}",
            body
        );
    }

    #[test]
    fn a_record_cannot_forge_a_line() {
        let dir = scratch("forge");
        let path = dir.join("cvsdesk.log");
        let logger = FileLogger::at(path.clone());
        write_record(&logger, "first\nINFO forged");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("first?INFO forged"), "{}", body);
        assert!(!body.contains("\nINFO forged"), "{}", body);
    }

    #[test]
    fn a_torn_utf8_tail_is_still_copied() {
        let dir = scratch("torn");
        let path = dir.join("cvsdesk.log");
        std::fs::write(&path, [0xff, b'o', b'k', b'\n']).unwrap();
        let tail = tail_of(&path, 10);
        assert!(tail.contains("ok"), "{:?}", tail);
    }

    #[test]
    fn the_diagnostics_tail_keeps_the_last_lines() {
        let dir = scratch("tail");
        let path = dir.join("cvsdesk.log");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let tail = tail_of(&path, 2);
        assert!(tail.contains("two"), "{}", tail);
        assert!(tail.contains("three"), "{}", tail);
        assert!(!tail.contains("one"), "{}", tail);
    }
}
