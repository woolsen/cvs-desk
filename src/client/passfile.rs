//! The `.cvspass` file: where `cvs login` keeps passwords.
//!
//! One line per root: `/1 <canonical root> <scrambled password>`. "Scrambled" is CVS's own
//! substitution from `lib/scramble.c`: the value starts with `A` followed by one mapped byte
//! per password byte. The same text is what the pserver handshake carries; the server
//! unscrambles it before checking. It is not encryption and must not be treated as one.
//!
//! The key is the canonical root of `normalize_cvsroot` (root.c:743-780), not the text the
//! user typed: `:pserver:user@host:port/path`, with the default port 2401 written out, the
//! host name in lower case, the user defaulting to the caller, and no trailing slashes in
//! the path. Old-style lines without the `/1 ` marker are still read; their roots are
//! canonicalized before they are compared, the way `password_entry_parseline` does.
//!
//! [CVSNT](https://www.cvsnt.org/) keeps the same passwords in
//! `HKCU\Software\Cvsnt\cvspass` instead of `.cvspass`, so `login` and `logout` update that
//! store too on Windows.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::client::error::{Error, Result};
use crate::client::root::{Method, Root};

/// CVS's substitution table, copied from `lib/scramble.c`. It maps each byte to another in
/// the same domain (control, printing, meta) and is its own inverse.
const SHIFTS: [u8; 256] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 114, 120, 53, 79, 96, 109, 72, 108, 70, 64, 76, 67, 116, 74, 68, 87,
    111, 52, 75, 119, 49, 34, 82, 81, 95, 65, 112, 86, 118, 110, 122, 105, 41, 57, 83, 43, 46, 102,
    40, 89, 38, 103, 45, 50, 42, 123, 91, 35, 125, 55, 54, 66, 124, 126, 59, 47, 92, 71, 115, 78,
    88, 107, 106, 56, 36, 121, 117, 104, 101, 100, 69, 73, 99, 63, 94, 93, 39, 37, 61, 48, 58, 113,
    32, 90, 44, 98, 60, 51, 33, 97, 62, 77, 84, 80, 85, 223, 225, 216, 187, 166, 229, 189, 222,
    188, 141, 249, 148, 200, 184, 136, 248, 190, 199, 170, 181, 204, 138, 232, 218, 183, 255, 234,
    220, 247, 213, 203, 226, 193, 174, 172, 228, 252, 217, 201, 131, 230, 197, 211, 145, 238, 161,
    179, 160, 212, 207, 221, 254, 173, 202, 146, 224, 151, 140, 196, 205, 130, 135, 133, 143, 246,
    192, 159, 244, 239, 185, 168, 215, 144, 139, 165, 180, 157, 147, 186, 214, 176, 227, 231, 219,
    169, 175, 156, 206, 198, 129, 164, 150, 210, 154, 177, 134, 127, 182, 128, 158, 208, 162, 132,
    167, 209, 149, 241, 153, 251, 237, 236, 171, 195, 243, 233, 253, 240, 194, 250, 191, 155, 142,
    137, 245, 235, 163, 242, 178, 152,
];

/// CVS's scrambled form: `A`, then every byte mapped through [`SHIFTS`].
pub fn scramble(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(b'A');
    out.extend(bytes.iter().map(|b| SHIFTS[*b as usize]));
    out
}

/// The inverse of [`scramble`]. `None` when the value is not CVS's format (no leading `A`).
pub fn unscramble(bytes: &[u8]) -> Option<Vec<u8>> {
    let payload = bytes.strip_prefix(b"A".as_slice())?;
    Some(payload.iter().map(|b| SHIFTS[*b as usize]).collect())
}

/// `$CVS_PASSFILE` (even when empty, as CVS does), else `$HOME/.cvspass`, else
/// `%USERPROFILE%\.cvspass`.
pub fn passfile_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CVS_PASSFILE") {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".cvspass"))
}

/// The canonical key `normalize_cvsroot()` writes for a pserver root:
/// `:pserver:<user>@<host>:<port><path>`.
pub fn canonical(root: &Root) -> String {
    let port = if root.port == 0 {
        crate::client::root::DEFAULT_PSERVER_PORT
    } else {
        root.port
    };
    // `normalize_cvsroot` falls back to `getcaller()` for a root that names no user
    // (root.c:771-772).
    let user = if root.user.is_empty() {
        crate::client::root::current_user()
    } else {
        root.user.clone()
    };
    format!(
        ":pserver:{}@{}:{}{}",
        user,
        root.host.to_ascii_lowercase(),
        port,
        sanitize_path(&root.path)
    )
}

/// Canonicalize a root as it may appear on the command line or in an old passfile line.
/// Old lines have no `:pserver:` prefix, so that form is tried too.
pub fn canonical_key(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // A `/1` line already holds the canonical shape (`host:port/path`), which is not the
    // shape `Root::parse` takes (`host:[port]:/path`); read it directly and stay idempotent.
    if let Some((user, host, port, path)) = canonical_parts(text) {
        return Some(format!(
            ":pserver:{}@{}:{}{}",
            user,
            host.to_ascii_lowercase(),
            port,
            sanitize_path(&path)
        ));
    }
    let parsed = Root::parse(text)
        .ok()
        .filter(|root| root.method == Method::Pserver)
        .or_else(|| {
            Root::parse(&format!(":pserver:{}", text))
                .ok()
                .filter(|root| root.method == Method::Pserver)
        })?;
    Some(canonical(&parsed))
}

/// Split `:pserver:user@host:port[/:]path` into its four pieces. Tolerates the typed
/// `host:port:/path` spelling as well as the canonical `host:port/path` one.
fn canonical_parts(text: &str) -> Option<(String, String, u16, String)> {
    let rest = text.strip_prefix(":pserver:")?;
    let (user, host_part) = rest.split_once('@')?;
    if user.is_empty() {
        return None;
    }
    let start = if host_part.starts_with('[') {
        host_part.find(']')? + 1
    } else {
        0
    };
    let colon = start + host_part.get(start..)?.find(':')?;
    let (host, after) = host_part.split_at(colon);
    if host.is_empty() {
        return None;
    }
    let after = &after[1..];
    let digits = after.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let port: u16 = after[..digits].parse().ok()?;
    let path = after[digits..]
        .strip_prefix(':')
        .unwrap_or(&after[digits..]);
    if !path.starts_with('/') {
        return None;
    }
    Some((user.to_string(), host.to_string(), port, path.to_string()))
}

/// `Sanitize_Repository_Name` (repos.c:196-207): no trailing slashes, no trailing `/.`.
fn sanitize_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut end = bytes.len();
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1;
    }
    let mut text = &path[..end];
    if text.len() >= 2 && text.ends_with("/.") {
        text = &text[..text.len() - 2];
    }
    if text.is_empty() {
        "/".to_string()
    } else {
        text.to_string()
    }
}

/// The root a passfile line is keyed by, and its saved value. `None` for a blank line, a
/// line without a value, or a format version this client does not read.
fn entry(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let body = line.strip_suffix(b"\n").unwrap_or(line);
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    if body.is_empty() {
        return None;
    }
    let rest = if body.starts_with(b"/") {
        // `/<version><whitespace><root> <value>`; this client writes and reads version 1
        // (login.c:121-176), where the root is already canonical.
        let digits = body[1..].iter().take_while(|b| b.is_ascii_digit()).count();
        let version: u32 = std::str::from_utf8(body.get(1..1 + digits)?)
            .ok()?
            .parse()
            .ok()?;
        if version != 1 {
            return None;
        }
        let mut start = 1 + digits;
        if body.get(start).is_some_and(|b| b.is_ascii_whitespace()) {
            start += 1;
        }
        &body[start..]
    } else {
        body
    };
    let space = rest.iter().position(|b| *b == b' ' || *b == b'\t')?;
    let (root, value) = rest.split_at(space);
    Some((root, &value[1..]))
}

/// Whether `line` belongs to `canonical`.
fn entry_matches(line: &[u8], canonical: &str) -> bool {
    let Some((key, _)) = entry(line) else {
        return false;
    };
    let Ok(key) = std::str::from_utf8(key) else {
        return false;
    };
    canonical_key(key).as_deref() == Some(canonical)
}

/// The scrambled password recorded for `root`, ready for the pserver handshake.
pub fn scrambled_for(root: &str) -> Option<Vec<u8>> {
    let path = passfile_path()?;
    scrambled_for_at(&path, root)
}

pub fn scrambled_for_at(path: &Path, root: &str) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    find_scrambled(&bytes, root)
}

/// The plain password recorded for `root`, if the passfile has one.
pub fn password_for(root: &str) -> Option<String> {
    let path = passfile_path()?;
    password_for_at(&path, root)
}

pub fn password_for_at(path: &Path, root: &str) -> Option<String> {
    let scrambled = scrambled_for_at(path, root)?;
    let plain = unscramble(&scrambled)?;
    Some(String::from_utf8_lossy(&plain).into_owned())
}

fn find_scrambled(bytes: &[u8], root: &str) -> Option<Vec<u8>> {
    let wanted = canonical_key(root)?;
    for line in bytes.split(|b| *b == b'\n') {
        let Some((key, value)) = entry(line) else {
            continue;
        };
        if value.first() != Some(&b'A') {
            continue;
        }
        let Ok(key) = std::str::from_utf8(key) else {
            continue;
        };
        if canonical_key(key).as_deref() == Some(wanted.as_str()) {
            return Some(value.to_vec());
        }
    }
    None
}

/// Record `password` for `root`, replacing an earlier line for the same root. The line is
/// written the way CVS writes it, `/1 <canonical root> <scrambled>`, and the CVSNT registry
/// is updated on Windows. Nothing is written when the file already holds that password.
pub fn set_password(root: &str, password: &str) -> Result<PathBuf> {
    set_password_with(root, password, &mut |_| {})
}

/// The same, reporting trouble that CVS would only warn about (login.c:313-320) instead of
/// failing the login. A failed CVSNT registry update is a warning too: `.cvspass` is the
/// store CVS itself owns.
pub fn set_password_with(
    root: &str,
    password: &str,
    warn: &mut dyn FnMut(String),
) -> Result<PathBuf> {
    let path = passfile_path().ok_or_else(|| Error::new("could not find out home directory"))?;
    let canonical = canonical_key(root).unwrap_or_else(|| root.to_string());
    let scrambled = scramble(password.as_bytes());
    set_password_at_with(&path, &canonical, password, warn)?;
    if let Err(error) = write_registry(&canonical, &scrambled) {
        warn(error.text().to_string());
    }
    // CVSNT reads its own store by the exact key, so keep the typed spelling too when it
    // differs from the canonical one. The alias never replaces the canonical entry.
    if canonical_key(root).is_some() && root != canonical {
        if let Err(error) = write_registry(root, &scrambled) {
            warn(error.text().to_string());
        }
    }
    Ok(path)
}

pub fn set_password_at(path: &Path, root: &str, password: &str) -> Result<()> {
    set_password_at_with(path, root, password, &mut |_| {})
}

pub fn set_password_at_with(
    path: &Path,
    root: &str,
    password: &str,
    warn: &mut dyn FnMut(String),
) -> Result<()> {
    let canonical = canonical_key(root).unwrap_or_else(|| root.to_string());
    let scrambled = scramble(password.as_bytes());
    let existing = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            warn(format!(
                "warning: failed to open {} for reading: {}",
                path.display(),
                error
            ));
            // CVS appends in add mode without ever truncating a file it could not read
            // (login.c:472-480), so a new entry cannot destroy the old ones.
            return append_line(path, &canonical, &scrambled);
        }
    };

    let mut kept = Vec::new();
    // CVS compares the new password with the first matching line only; if it is the same
    // there, the whole file is left untouched (login.c:438-458).
    let mut found: Option<Vec<u8>> = None;
    if let Some(bytes) = &existing {
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if entry_matches(line, &canonical) {
                if found.is_none() {
                    let (_, value) = entry(line).expect("a matching line has a value");
                    found = Some(value.to_vec());
                }
                continue;
            }
            kept.extend_from_slice(line);
        }
    }
    if found.as_deref() == Some(scrambled.as_slice()) {
        return Ok(());
    }

    if !kept.is_empty() && !kept.ends_with(b"\n") {
        kept.push(b'\n');
    }
    kept.extend_from_slice(b"/1 ");
    kept.extend_from_slice(canonical.as_bytes());
    kept.push(b' ');
    kept.extend_from_slice(&scrambled);
    kept.push(b'\n');
    write_bytes(path, &kept)
}

/// What happened to a password when `logout` asked for its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveOutcome {
    /// A line (or a CVSNT registry value) was removed.
    Removed,
    /// The file was read and had no line for this root.
    NotFound,
    /// There is no passfile to read.
    Missing,
    /// The passfile exists but could not be read or rewritten.
    Unreadable(String),
}

/// Remove the line for `root` from `.cvspass` and the CVSNT registry.
pub fn remove_password(root: &str) -> RemoveOutcome {
    let canonical = canonical_key(root).unwrap_or_else(|| root.to_string());
    let Some(path) = passfile_path() else {
        return RemoveOutcome::Unreadable("could not find out home directory".to_string());
    };
    let outcome = remove_password_at(&path, &canonical);
    let mut registry_removed = remove_registry(&canonical);
    if canonical_key(root).is_some() && root != canonical {
        registry_removed |= remove_registry(root);
    }
    match outcome {
        RemoveOutcome::NotFound | RemoveOutcome::Missing if registry_removed => {
            RemoveOutcome::Removed
        }
        other => other,
    }
}

pub fn remove_password_at(path: &Path, root: &str) -> RemoveOutcome {
    let canonical = canonical_key(root).unwrap_or_else(|| root.to_string());
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return RemoveOutcome::Missing,
        Err(error) => return RemoveOutcome::Unreadable(error.to_string()),
    };

    let mut kept = Vec::with_capacity(bytes.len());
    let mut removed = false;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if entry_matches(line, &canonical) {
            removed = true;
            continue;
        }
        kept.extend_from_slice(line);
    }
    if !removed {
        return RemoveOutcome::NotFound;
    }
    match write_bytes(path, &kept) {
        Ok(()) => RemoveOutcome::Removed,
        Err(error) => RemoveOutcome::Unreadable(error.text().to_string()),
    }
}

/// Add one `/1` line without reading or rewriting the rest of the file.
fn append_line(path: &Path, canonical: &str, scrambled: &[u8]) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(Error::new(
            "could not open an empty passfile path for writing",
        ));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(b"/1 ")?;
    file.write_all(canonical.as_bytes())?;
    file.write_all(b" ")?;
    file.write_all(scrambled)?;
    Ok(file.write_all(b"\n")?)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(Error::new(
            "could not open an empty passfile path for writing",
        ));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut temp_name = path.as_os_str().to_os_string();
    temp_name.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp_name);
    fs::write(&temp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&temp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        Error::new(error.to_string())
    })
}

#[cfg(windows)]
const REGISTRY_KEY: &str = r"Software\Cvsnt\cvspass";

/// Save the scrambled password the way CVSNT does: one string value per root.
#[cfg(windows)]
fn write_registry(root: &str, scrambled: &[u8]) -> Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(REGISTRY_KEY)
        .map_err(|error| {
            Error::new(format!(
                "could not open the CVSNT password store: {}",
                error
            ))
        })?;
    let value = String::from_utf8_lossy(scrambled).into_owned();
    key.set_value(root, &value).map_err(|error| {
        Error::new(format!(
            "could not save the CVSNT password for {}: {}",
            root, error
        ))
    })?;
    Ok(())
}

#[cfg(not(windows))]
fn write_registry(_root: &str, _scrambled: &[u8]) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn remove_registry(root: &str) -> bool {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(REGISTRY_KEY, KEY_READ | KEY_WRITE)
    else {
        return false;
    };
    key.delete_value(root).is_ok()
}

#[cfg(not(windows))]
fn remove_registry(_root: &str) -> bool {
    false
}

/// Ask for a password without echoing it. The prompt goes to stderr. On Windows the console
/// echo is turned off through `SetConsoleMode`; when there is no console, the line is read
/// from stdin, which is also what a redirected run expects.
#[cfg(windows)]
pub fn read_password(prompt: &str) -> Option<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }

    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;

    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut mode = 0;
    let console = handle != 0 && handle != -1 && unsafe { GetConsoleMode(handle, &mut mode) } != 0;
    if console {
        unsafe {
            SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT);
        }
    }

    eprint!("{}", prompt);
    let _ = io::stderr().flush();
    let mut line = String::new();
    let read = io::stdin().read_line(&mut line);

    if console {
        unsafe {
            SetConsoleMode(handle, mode);
        }
        eprintln!();
    }

    match read {
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
        Err(_) => None,
    }
}

#[cfg(not(windows))]
pub fn read_password(prompt: &str) -> Option<String> {
    eprint!("{}", prompt);
    let _ = io::stderr().flush();
    let mut line = String::new();
    match io::stdin().read_line(&mut line) {
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-pass-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_scramble_is_cvs_own_substitution() {
        // The strings CVS's lib/scramble.c produces for these passwords.
        assert_eq!(scramble(b"secret"), b"AZdh d,".to_vec());
        assert_eq!(scramble(b"foobar"), b"AE00uy ".to_vec());
        assert_eq!(scramble(b""), b"A".to_vec());
    }

    #[test]
    fn unscramble_is_the_inverse_and_needs_the_a_marker() {
        assert_eq!(unscramble(&scramble(b"secret")), Some(b"secret".to_vec()));
        assert_eq!(unscramble(b"secret"), None);
        assert_eq!(unscramble(b""), None);
    }

    #[test]
    fn the_stored_value_is_the_one_the_server_expects() {
        let dir = scratch("wire");
        let path = dir.join(".cvspass");
        let root = ":pserver:ann@host:/repo";
        set_password_at(&path, root, "secret").unwrap();
        assert_eq!(
            scrambled_for_at(&path, root).as_deref(),
            Some(&b"AZdh d,"[..])
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_line_with_the_format_marker_still_reads() {
        let dir = scratch("marker");
        let path = dir.join(".cvspass");
        let root = ":pserver:ann@host:/repo";
        std::fs::write(&path, format!("/1 {} {}\n", root, "AZdh d,")).unwrap();
        assert_eq!(password_for_at(&path, root).as_deref(), Some("secret"));
        assert_eq!(
            scrambled_for_at(&path, root).as_deref(),
            Some(&b"AZdh d,"[..])
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_marker_may_be_followed_by_another_whitespace_character() {
        let dir = scratch("marker-tab");
        let path = dir.join(".cvspass");
        fs::write(&path, "/1\t:pserver:ann@host:2401/repo AZdh d,\n").unwrap();
        assert_eq!(
            password_for_at(&path, ":pserver:ann@host:/repo").as_deref(),
            Some("secret")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn logout_removes_a_line_with_the_format_marker() {
        let dir = scratch("remove-marker");
        let path = dir.join(".cvspass");
        let root = ":pserver:ann@host:/repo";
        std::fs::write(&path, format!("/1 {} {}\n", root, "AZdh d,")).unwrap();
        assert_eq!(remove_password_at(&path, root), RemoveOutcome::Removed);
        assert_eq!(password_for_at(&path, root), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn setting_again_replaces_a_line_with_the_format_marker() {
        let dir = scratch("replace-marker");
        let path = dir.join(".cvspass");
        let root = ":pserver:ann@host:/repo";
        std::fs::write(&path, format!("/1 {} {}\n", root, "AZdh d,")).unwrap();
        set_password_at(&path, root, "second").unwrap();
        assert_eq!(password_for_at(&path, root).as_deref(), Some("second"));
        assert_eq!(
            fs::read(&path)
                .unwrap()
                .iter()
                .filter(|b| **b == b'\n')
                .count(),
            1,
            "one line per root"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_password_is_read_back_for_its_own_root_only() {
        let dir = scratch("read");
        let path = dir.join(".cvspass");
        let a = ":pserver:ann@host:/repo";
        let b = ":pserver:ann@host:2402:/repo";
        set_password_at(&path, a, "first").unwrap();
        set_password_at(&path, b, "second").unwrap();
        assert_eq!(password_for_at(&path, a).as_deref(), Some("first"));
        assert_eq!(password_for_at(&path, b).as_deref(), Some("second"));
        assert_eq!(password_for_at(&path, ":pserver:other@host:/repo"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn setting_again_replaces_the_line_and_logout_removes_it() {
        let dir = scratch("replace");
        let path = dir.join(".cvspass");
        let root = ":pserver:ann@host:/repo";
        set_password_at(&path, root, "one").unwrap();
        set_password_at(&path, root, "two").unwrap();
        assert_eq!(password_for_at(&path, root).as_deref(), Some("two"));
        assert_eq!(
            fs::read(&path)
                .unwrap()
                .iter()
                .filter(|b| **b == b'\n')
                .count(),
            1,
            "one line per root"
        );
        assert_eq!(remove_password_at(&path, root), RemoveOutcome::Removed);
        assert_eq!(password_for_at(&path, root), None);
        assert_eq!(remove_password_at(&path, root), RemoveOutcome::NotFound);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_written_line_is_canonical_with_the_format_marker() {
        let dir = scratch("canonical");
        let path = dir.join(".cvspass");
        set_password_at(&path, ":pserver:ANN@CVS.Example:/repo///", "secret").unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            b"/1 :pserver:ANN@cvs.example:2401/repo AZdh d,\n".to_vec()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_old_style_line_is_found_by_a_different_spelling() {
        let dir = scratch("legacy");
        let path = dir.join(".cvspass");
        fs::write(&path, "ann@CVS.Example:/repo AZdh d,\n").unwrap();
        let typed = ":pserver:ann@cvs.example:2401/repo/";
        assert_eq!(password_for_at(&path, typed).as_deref(), Some("secret"));
        assert_eq!(remove_password_at(&path, typed), RemoveOutcome::Removed);
        assert_eq!(fs::read(&path).unwrap(), Vec::<u8>::new());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unrelated_lines_are_kept_byte_for_byte() {
        let dir = scratch("preserve");
        let path = dir.join(".cvspass");
        let other = b"/1 :pserver:other@host:2401/repo A\n\r\n# note\r\n";
        fs::write(&path, other).unwrap();
        set_password_at(&path, ":pserver:ann@host:/repo", "secret").unwrap();
        let text = fs::read(&path).unwrap();
        assert!(
            text.starts_with(other),
            "{}",
            String::from_utf8_lossy(&text)
        );
        assert!(text.ends_with(b"/1 :pserver:ann@host:2401/repo AZdh d,\n"));
        assert_eq!(
            remove_password_at(&path, ":pserver:ann@host:/repo"),
            RemoveOutcome::Removed
        );
        assert_eq!(fs::read(&path).unwrap(), other.to_vec());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_stored_password_is_not_stored_in_plain_text() {
        let dir = scratch("plain");
        let path = dir.join(".cvspass");
        set_password_at(&path, ":pserver:ann@host:/repo", "secret").unwrap();
        let text = fs::read(&path).unwrap();
        assert!(!text.windows(6).any(|w| w == b"secret"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
