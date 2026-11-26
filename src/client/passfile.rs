//! The `.cvspass` file: where `cvs login` keeps passwords.
//!
//! One line per root: `<root> <scrambled password>`. "Scrambled" is CVS's own substitution
//! from `lib/scramble.c`: the value starts with `A` followed by one mapped byte per password
//! byte. The same text is what the pserver handshake carries; the server unscrambles it
//! before checking. It is not encryption and must not be treated as one.
//!
//! The root is the key exactly as it was typed (port included or not), which is why
//! [`crate::client::root::Root::key`] keeps the typed text instead of rebuilding it. A
//! leading `/1 ` (the format marker some CVS clients write) is accepted when reading.

use std::fs;
use std::path::{Path, PathBuf};

use crate::client::error::{Error, Result};

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

/// `$CVS_PASSFILE`, else `$HOME/.cvspass`, else `%USERPROFILE%\.cvspass`.
pub fn passfile_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CVS_PASSFILE") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".cvspass"))
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
    let key = root.as_bytes();
    for line in bytes.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let line = line.strip_prefix(b"/1 ".as_slice()).unwrap_or(line);
        if let Some(rest) = line.strip_prefix(key) {
            if rest.first() == Some(&b' ') {
                let value = &rest[1..];
                if value.first() == Some(&b'A') {
                    return Some(value.to_vec());
                }
            }
        }
    }
    None
}

/// Record `password` for `root`, replacing an earlier line for the same root.
pub fn set_password(root: &str, password: &str) -> Result<PathBuf> {
    let path = passfile_path()
        .ok_or_else(|| Error::new("no .cvspass location: neither CVS_PASSFILE nor HOME is set"))?;
    set_password_at(&path, root, password)?;
    Ok(path)
}

pub fn set_password_at(path: &Path, root: &str, password: &str) -> Result<()> {
    let prefix = format!("{} ", root);
    let mut lines: Vec<Vec<u8>> = fs::read(path)
        .unwrap_or_default()
        .split(|b| *b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line).to_vec())
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let entry = line
                .strip_prefix(b"/1 ".as_slice())
                .unwrap_or(line.as_slice());
            !entry.starts_with(prefix.as_bytes())
        })
        .collect();
    let mut line = root.as_bytes().to_vec();
    line.push(b' ');
    line.extend_from_slice(&scramble(password.as_bytes()));
    lines.push(line);
    write_lines(path, &lines)
}

/// Remove the line for `root`. Returns whether a line was removed.
pub fn remove_password(root: &str) -> Result<bool> {
    let Some(path) = passfile_path() else {
        return Ok(false);
    };
    remove_password_at(&path, root)
}

pub fn remove_password_at(path: &Path, root: &str) -> Result<bool> {
    let Ok(bytes) = fs::read(path) else {
        return Ok(false);
    };
    let mut removed = false;
    let prefix = format!("{} ", root);
    let lines: Vec<Vec<u8>> = bytes
        .split(|b| *b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line).to_vec())
        .filter(|line| {
            let entry = line
                .strip_prefix(b"/1 ".as_slice())
                .unwrap_or(line.as_slice());
            let keep = !entry.starts_with(prefix.as_bytes());
            if !keep {
                removed = true;
            }
            keep && !line.is_empty()
        })
        .collect();
    if removed {
        write_lines(path, &lines)?;
    }
    Ok(removed)
}

fn write_lines(path: &Path, lines: &[Vec<u8>]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line);
        bytes.push(b'\n');
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&temp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&temp, path)?;
    Ok(())
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
    fn logout_removes_a_line_with_the_format_marker() {
        let dir = scratch("remove-marker");
        let path = dir.join(".cvspass");
        let root = ":pserver:ann@host:/repo";
        std::fs::write(&path, format!("/1 {} {}\n", root, "AZdh d,")).unwrap();
        assert!(remove_password_at(&path, root).unwrap());
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
        assert!(remove_password_at(&path, root).unwrap());
        assert_eq!(password_for_at(&path, root), None);
        assert!(!remove_password_at(&path, root).unwrap());
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
