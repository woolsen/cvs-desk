//! Creating a working-copy folder without fetching its files.
//!
//! Clicking a folder that the repository has but this working copy has not checked out
//! writes only the control files: `CVS/Root`, `CVS/Repository`, an empty `CVS/Entries`,
//! the parent's sticky tag, and the parent's `D/<name>////` line. The folder is then a
//! real working folder whose files still have to be updated on demand.

use std::path::Path;

use crate::client::error::{Error, Result};
use crate::client::work::codec::{self, Codec};
use crate::client::work::entries::Entries;

/// Make `parent/name` a working-copy folder without fetching any file.
///
/// Returns `true` when the folder was created, and `false` when it was already a CVS
/// folder. An existing folder that is not under CVS is refused: silently adding control
/// files would make its untracked files look versioned.
pub fn ensure_child(parent: &Path, name: &str) -> Result<bool> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(Error::new(format!("invalid folder name: {name}")));
    }
    if name == "CVS" {
        return Err(Error::new("'CVS' is the control folder"));
    }

    let parent_cvs = parent.join("CVS");
    let repository = super::dir::read_repository(parent)
        .ok_or_else(|| Error::new("CVS/Repository is missing or empty"))?;
    let root = std::fs::read(parent_cvs.join("Root"))
        .map_err(|_| Error::new("CVS/Root is missing or unreadable"))?;
    let encoding = codec::for_dir(parent);
    let mut parent_entries = Entries::read(parent)?;
    let already_recorded = parent_entries
        .directories()
        .any(|recorded| recorded == name);

    let child = parent.join(name);
    if child.is_dir() {
        if !child.join("CVS").is_dir() {
            return Err(Error::new(format!(
                "the local folder '{}' is not a CVS working folder",
                child.display()
            )));
        }
        if !already_recorded {
            parent_entries.set_directory(name);
            parent_entries.write(parent, encoding)?;
        }
        return Ok(false);
    }
    if child.exists() {
        return Err(Error::new(format!(
            "a file named '{}' is already in this folder",
            name
        )));
    }

    let child_repository = format!("{}/{}", repository.trim_end_matches('/'), name);
    let tag = std::fs::read(parent_cvs.join("Tag")).ok();
    let created = (|| -> Result<()> {
        let cvs = child.join("CVS");
        std::fs::create_dir_all(&cvs)?;
        std::fs::write(cvs.join("Root"), &root)?;
        write_line(&cvs.join("Repository"), &child_repository, encoding)?;
        Entries::default().write(&child, encoding)?;
        if let Some(tag) = tag {
            std::fs::write(cvs.join("Tag"), tag)?;
        }
        if !already_recorded {
            parent_entries.set_directory(name);
            parent_entries.write(parent, encoding)?;
        }
        Ok(())
    })();

    if let Err(error) = created {
        // Do not leave half a working folder behind when the parent index could not be
        // written.
        let _ = std::fs::remove_dir_all(&child);
        return Err(error);
    }
    Ok(true)
}

fn write_line(path: &Path, text: &str, encoding: Codec) -> Result<()> {
    let mut bytes = encoding.encode(text)?;
    bytes.extend_from_slice(codec::newline().as_bytes());
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::client::work::codec::{self, Codec};
    use crate::client::work::entries::Entries;

    use super::ensure_child;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-skeleton-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        dir
    }

    fn parent(tag: &str) -> PathBuf {
        let dir = scratch(tag);
        std::fs::write(dir.join("CVS").join("Root"), ":pserver:ann@host:/repo\n").unwrap();
        std::fs::write(dir.join("CVS").join("Repository"), "Develop/Design\n").unwrap();
        std::fs::write(
            dir.join("CVS").join("Entries"),
            "/a.txt/1.1/Sat Nov 22 09:14:03 2026//\nD\n",
        )
        .unwrap();
        dir
    }

    fn text(path: &Path) -> String {
        codec::for_dir(path.parent().unwrap().parent().unwrap())
            .decode(&std::fs::read(path).unwrap())
    }

    #[test]
    fn a_new_child_gets_control_files_and_a_parent_record() {
        let parent = parent("new");
        let child = parent.join("Child");

        assert!(ensure_child(&parent, "Child").unwrap());

        assert!(child.join("CVS").is_dir());
        assert_eq!(
            std::fs::read(child.join("CVS").join("Root")).unwrap(),
            std::fs::read(parent.join("CVS").join("Root")).unwrap()
        );
        assert_eq!(
            text(&child.join("CVS").join("Repository")).trim_end(),
            "Develop/Design/Child"
        );
        assert!(Entries::read(&child).unwrap().lines.is_empty());

        let entries = text(&parent.join("CVS").join("Entries"));
        assert!(entries.contains("/a.txt/1.1/"), "{entries}");
        assert!(entries.contains("D/Child////"), "{entries}");
        assert!(
            entries.trim_end().ends_with('D'),
            "the marker stays last: {entries}"
        );

        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn asking_again_does_not_duplicate_the_folder_line() {
        let parent = parent("again");
        assert!(ensure_child(&parent, "Child").unwrap());
        assert!(!ensure_child(&parent, "Child").unwrap());

        let entries = text(&parent.join("CVS").join("Entries"));
        assert_eq!(entries.matches("D/Child////").count(), 1, "{entries}");

        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn an_existing_plain_folder_is_not_turned_into_a_working_folder() {
        let parent = parent("plain");
        let child = parent.join("Child");
        std::fs::create_dir_all(&child).unwrap();

        let error = ensure_child(&parent, "Child").unwrap_err();

        assert!(error.text().contains("not a CVS"), "{error}");
        assert!(!child.join("CVS").exists());
        assert!(!text(&parent.join("CVS").join("Entries")).contains("D/Child////"));

        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn a_file_with_the_same_name_is_refused() {
        let parent = parent("file");
        std::fs::write(parent.join("Child"), b"x").unwrap();

        let error = ensure_child(&parent, "Child").unwrap_err();

        assert!(error.text().contains("file"), "{error}");
        assert!(!text(&parent.join("CVS").join("Entries")).contains("D/Child////"));

        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn a_parent_without_a_repository_is_refused() {
        let parent = scratch("no-repo");
        std::fs::write(parent.join("CVS").join("Root"), ":pserver:ann@host:/repo\n").unwrap();

        let error = ensure_child(&parent, "Child").unwrap_err();

        assert!(error.text().contains("Repository"), "{error}");
        assert!(!parent.join("Child").exists());

        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn the_parent_sticky_tag_is_copied() {
        let parent = parent("tag");
        std::fs::write(parent.join("CVS").join("Tag"), "TBRANCH\n").unwrap();

        ensure_child(&parent, "Child").unwrap();

        assert_eq!(
            std::fs::read(parent.join("Child").join("CVS").join("Tag")).unwrap(),
            b"TBRANCH\n"
        );

        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn gbk_parents_keep_their_encoding_and_chinese_names() {
        let parent = scratch("gbk");
        std::fs::write(parent.join("CVS").join("Root"), ":pserver:ann@host:/repo\n").unwrap();
        let mut repository = Codec::Gbk.encode("模块").unwrap();
        repository.extend_from_slice(b"\r\n");
        std::fs::write(parent.join("CVS").join("Repository"), repository).unwrap();
        let mut entries = Entries::default();
        entries.set_file(crate::client::work::entries::Entry::new(
            "模块.txt",
            "1.1",
            "",
            "",
            "",
        ));
        entries.write(&parent, Codec::Gbk).unwrap();

        assert!(ensure_child(&parent, "知识库").unwrap());

        let child = parent.join("知识库");
        let repository = std::fs::read(child.join("CVS").join("Repository")).unwrap();
        assert!(std::str::from_utf8(&repository).is_err(), "must stay GBK");
        assert_eq!(Codec::Gbk.decode(&repository).trim_end(), "模块/知识库");
        let parent_entries = std::fs::read(parent.join("CVS").join("Entries")).unwrap();
        assert!(
            std::str::from_utf8(&parent_entries).is_err(),
            "must stay GBK"
        );
        assert!(Codec::Gbk.decode(&parent_entries).contains("D/知识库////"));

        std::fs::remove_dir_all(&parent).ok();
    }
}
