//! `CVS/Base`: the copy `cvs edit` keeps so `cvs unedit` can put the file back.
//!
//! The reference client writes the working file to `CVS/Base/<name>` when a file is taken
//! for editing, records `B<name>/<revision>/` in `CVS/Baserev`, and copies the base back on
//! `unedit`. Without it, `unedit` could only make the file read-only and would leave the
//! person's changes in place, which is not what "revert changes" means.

use std::path::{Path, PathBuf};

use crate::client::error::Result;
use crate::client::work::codec;
use crate::client::work::dir;

fn base_dir(folder: &Path) -> PathBuf {
    folder.join("CVS").join("Base")
}

fn baserev_path(folder: &Path) -> PathBuf {
    folder.join("CVS").join("Baserev")
}

/// Copy `folder/name` into `CVS/Base` and record its revision, replacing an older base for
/// the same name. The reference does this on every `edit`, even when a base already exists.
pub fn save(folder: &Path, name: &str, revision: &str) -> Result<()> {
    let base = base_dir(folder);
    std::fs::create_dir_all(&base)?;
    let target = base.join(name);
    if target.exists() {
        dir::clear_readonly(&target)?;
    }
    std::fs::copy(folder.join(name), &target)?;
    let mut lines = read_lines(folder);
    lines.retain(|line| !is_line_for(line, name));
    lines.push(format!("B{}/{}/", name, revision));
    write_lines(folder, &lines)
}

/// Put the base copy back and forget it. Returns false when this file has no base, which is
/// what `unedit` does for a file that was never taken with `edit`.
pub fn restore(folder: &Path, name: &str) -> Result<bool> {
    let base = base_dir(folder).join(name);
    if !base.exists() {
        return Ok(false);
    }
    let local = folder.join(name);
    if local.exists() {
        dir::clear_readonly(&local)?;
    }
    std::fs::copy(&base, &local)?;
    forget(folder, name)?;
    Ok(true)
}

/// Drop the base for one file; a commit accepts the changes, so there is nothing to revert.
pub fn forget(folder: &Path, name: &str) -> Result<()> {
    let base = base_dir(folder).join(name);
    match std::fs::remove_file(&base) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut lines = read_lines(folder);
    let before = lines.len();
    lines.retain(|line| !is_line_for(line, name));
    if lines.len() != before {
        write_lines(folder, &lines)?;
    }
    Ok(())
}

fn is_line_for(line: &str, name: &str) -> bool {
    let Some(rest) = line.strip_prefix('B') else {
        return false;
    };
    let Some((before_revision, _)) = rest.rsplit_once('/') else {
        return false;
    };
    let Some((line_name, _)) = before_revision.rsplit_once('/') else {
        return false;
    };
    line_name == name
}

fn read_lines(folder: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(baserev_path(folder)) else {
        return Vec::new();
    };
    let text = codec::for_dir(folder).decode(&bytes);
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.to_string())
        .collect()
}

fn write_lines(folder: &Path, lines: &[String]) -> Result<()> {
    let path = baserev_path(folder);
    if lines.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        return Ok(());
    }
    let codec = codec::for_dir(folder);
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(&codec.encode(line)?);
        bytes.extend_from_slice(codec::newline().as_bytes());
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-base-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        dir
    }

    #[test]
    fn a_saved_base_comes_back_on_restore() {
        let folder = scratch("restore");
        std::fs::write(folder.join("a.txt"), b"base\n").unwrap();
        save(&folder, "a.txt", "1.2").unwrap();
        assert!(base_dir(&folder).join("a.txt").is_file());
        let rev = std::fs::read_to_string(baserev_path(&folder)).unwrap();
        assert!(rev.contains("Ba.txt/1.2/"), "{}", rev);
        std::fs::write(folder.join("a.txt"), b"changed\n").unwrap();
        assert!(restore(&folder, "a.txt").unwrap());
        assert_eq!(std::fs::read(folder.join("a.txt")).unwrap(), b"base\n");
        assert!(!base_dir(&folder).join("a.txt").exists());
        assert!(!baserev_path(&folder).exists());
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn forget_keeps_the_other_files_base() {
        let folder = scratch("forget");
        std::fs::write(folder.join("a.txt"), b"a\n").unwrap();
        std::fs::write(folder.join("b.txt"), b"b\n").unwrap();
        save(&folder, "a.txt", "1.1").unwrap();
        save(&folder, "b.txt", "1.1").unwrap();
        forget(&folder, "a.txt").unwrap();
        assert!(!base_dir(&folder).join("a.txt").exists());
        assert!(base_dir(&folder).join("b.txt").is_file());
        let rev = std::fs::read_to_string(baserev_path(&folder)).unwrap();
        assert!(!rev.contains("Ba.txt/"), "{}", rev);
        assert!(rev.contains("Bb.txt/1.1/"), "{}", rev);
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_name_that_starts_the_same_is_not_dropped() {
        let folder = scratch("names");
        std::fs::write(folder.join("a.txt"), b"a\n").unwrap();
        std::fs::write(folder.join("a.txt.bak"), b"b\n").unwrap();
        save(&folder, "a.txt", "1.1").unwrap();
        save(&folder, "a.txt.bak", "1.1").unwrap();
        forget(&folder, "a.txt").unwrap();
        assert!(base_dir(&folder).join("a.txt.bak").is_file());
        let rev = std::fs::read_to_string(baserev_path(&folder)).unwrap();
        assert!(rev.contains("Ba.txt.bak/1.1/"), "{}", rev);
        std::fs::remove_dir_all(&folder).ok();
    }
}
