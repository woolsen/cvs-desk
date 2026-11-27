//! `CVS/Entries`: the record of what the working copy contains.
//!
//! One line per file: `/name/revision/date/options/tag`; one per folder: `D/name////`.
//! The date is the 24-character local time the file had when it was last written by CVS, or
//! one of CVS's markers (`Result of merge+…` for a conflict, a leading `/` for an update that
//! could not finish). The revision `0` means "added, not committed"; `-1` means "removed, not
//! committed".
//!
//! Lines this client does not recognise are kept as they are, so a file written by a newer
//! CVSNT is never quietly rewritten into something poorer.
//!
//! A command journals its changes to `CVS/Entries.Log` and merges that into `CVS/Entries`
//! when it finishes (see `merge_log`). An interrupted command leaves the journal behind, so
//! reading must apply it as well; otherwise a folder that really is checked out looks loose.

use std::path::Path;

use crate::client::error::Result;
use crate::client::work::codec::{self, Codec};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub revision: String,
    pub date: String,
    pub options: String,
    pub tag: String,
}

impl Entry {
    pub fn new(name: &str, revision: &str, date: &str, options: &str, tag: &str) -> Self {
        Self {
            name: name.to_string(),
            revision: revision.to_string(),
            date: date.to_string(),
            options: options.to_string(),
            tag: tag.to_string(),
        }
    }

    /// The line as it is written to `CVS/Entries`.
    pub fn line(&self) -> String {
        format!(
            "/{}/{}/{}/{}/{}",
            self.name, self.revision, self.date, self.options, self.tag
        )
    }

    /// Parse `/name/revision/date/options/tag`.
    pub fn parse(line: &str) -> Option<Entry> {
        let mut parts = line.splitn(6, '/');
        if parts.next()? != "" {
            return None;
        }
        Some(Entry {
            name: parts.next()?.to_string(),
            revision: parts.next().unwrap_or("").to_string(),
            date: parts.next().unwrap_or("").to_string(),
            options: parts.next().unwrap_or("").to_string(),
            tag: parts.next().unwrap_or("").to_string(),
        })
    }

    pub fn is_added(&self) -> bool {
        self.revision == "0"
    }

    pub fn is_removed(&self) -> bool {
        self.revision.starts_with("-1")
    }

    pub fn is_conflict(&self) -> bool {
        codec::is_conflict_date(&self.date)
    }

    pub fn is_unfinished(&self) -> bool {
        codec::is_unfinished_date(&self.date)
    }

    pub fn is_binary(&self) -> bool {
        self.options == "-kb"
    }

    pub fn is_unicode(&self) -> bool {
        self.options == "-ku"
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryLine {
    File(Entry),
    Directory(String),
    /// A line this client does not understand. Kept verbatim.
    Other(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entries {
    pub lines: Vec<EntryLine>,
}

impl Entries {
    /// Read `CVS/Entries` in `dir`, with any unfinished `CVS/Entries.Log` merged in.
    ///
    /// A cvs command journals its changes to `CVS/Entries.Log` and merges them into
    /// `CVS/Entries` when it finishes. If it is interrupted (the window is closed, the
    /// machine goes off) the journal stays behind, and cvs itself applies it the next time it
    /// opens the folder. Without that merge the `D/…` line of a folder can sit only in the
    /// journal: the list calls the folder "Untracked" and the recursive walk skips it.
    /// Reading never writes - only a real cvs command may settle the journal.
    pub fn read(dir: &Path) -> Result<Entries> {
        let path = dir.join("CVS").join("Entries");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Entries::default()),
            Err(e) => return Err(e.into()),
        };
        let text = codec::for_dir(dir).decode(&bytes);
        let mut entries = Entries::parse(&text);
        if let Ok(log) = std::fs::read(dir.join("CVS").join("Entries.Log")) {
            entries.merge_log(&codec::for_dir(dir).decode(&log));
        }
        Ok(entries)
    }

    /// Apply `CVS/Entries.Log` on top of these entries.
    ///
    /// The journal holds one change per line, `A ` (added) or `R ` (removed) followed by the
    /// same text an `Entries` line would have: `A D/name////` for a folder,
    /// `A /name/revision/date/options/tag` for a file. Lines this client does not understand
    /// are skipped, so a newer format loses nothing it is entitled to keep.
    pub fn merge_log(&mut self, text: &str) {
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            let Some((op, body)) = line.split_once(' ') else {
                continue;
            };
            let add = match op {
                "A" => true,
                "R" => false,
                _ => continue,
            };
            let entry = match body.trim_end() {
                "" => continue,
                body if body.starts_with("D/") => {
                    let name = body[2..].split('/').next().unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    EntryLine::Directory(name.to_string())
                }
                body if body.starts_with('/') => match Entry::parse(body) {
                    Some(entry) => EntryLine::File(entry),
                    None => continue,
                },
                _ => continue,
            };
            let name = match &entry {
                EntryLine::File(entry) => entry.name.clone(),
                EntryLine::Directory(name) => name.clone(),
                EntryLine::Other(_) => continue,
            };
            // The journal repeats what an earlier command already settled, so replace
            // whatever is there instead of appending a duplicate.
            self.lines.retain(|line| match (line, &entry) {
                (EntryLine::File(existing), EntryLine::File(_)) => existing.name != name,
                (EntryLine::Directory(existing), EntryLine::Directory(_)) => *existing != name,
                _ => true,
            });
            if add {
                self.insert_before_marker(entry);
            }
        }
    }

    pub fn parse(text: &str) -> Entries {
        let mut entries = Entries::default();
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("D/") {
                let name = rest.split('/').next().unwrap_or("");
                if !name.is_empty() {
                    entries.lines.push(EntryLine::Directory(name.to_string()));
                }
            } else if line.starts_with('/') {
                match Entry::parse(line) {
                    Some(entry) => entries.lines.push(EntryLine::File(entry)),
                    None => entries.lines.push(EntryLine::Other(line.to_string())),
                }
            } else {
                entries.lines.push(EntryLine::Other(line.to_string()));
            }
        }
        entries
    }

    /// Write the file through `CVS/Entries.Backup`, the way CVS does, then rename it into
    /// place so an interrupted write cannot leave a half-written index.
    pub fn write(&self, dir: &Path, encoding: Codec) -> Result<()> {
        let cvs = dir.join("CVS");
        std::fs::create_dir_all(&cvs)?;
        let mut bytes = Vec::new();
        for line in &self.lines {
            let text = match line {
                EntryLine::File(entry) => entry.line(),
                EntryLine::Directory(name) => format!("D/{name}////"),
                EntryLine::Other(text) => text.clone(),
            };
            bytes.extend_from_slice(&encoding.encode(&text)?);
            bytes.extend_from_slice(crate::client::work::codec::newline().as_bytes());
        }
        let backup = cvs.join("Entries.Backup");
        std::fs::write(&backup, bytes)?;
        std::fs::rename(&backup, cvs.join("Entries"))?;
        Ok(())
    }

    pub fn file(&self, name: &str) -> Option<&Entry> {
        self.lines.iter().find_map(|line| match line {
            EntryLine::File(entry) if entry.name == name => Some(entry),
            _ => None,
        })
    }

    /// Add a file entry, replacing the one with the same name where it already sits.
    pub fn set_file(&mut self, entry: Entry) {
        if let Some(slot) = self
            .lines
            .iter_mut()
            .find(|line| matches!(line, EntryLine::File(existing) if existing.name == entry.name))
        {
            *slot = EntryLine::File(entry);
        } else {
            self.insert_before_marker(EntryLine::File(entry));
        }
    }

    pub fn remove_file(&mut self, name: &str) {
        self.lines
            .retain(|line| !matches!(line, EntryLine::File(existing) if existing.name == name));
    }

    /// Drop a folder record; `cvs remove` has no form that takes a `D/` line out.
    pub fn remove_directory(&mut self, name: &str) {
        self.lines
            .retain(|line| !matches!(line, EntryLine::Directory(existing) if existing == name));
    }

    /// Record a subfolder once, keeping its place if it is already listed.
    pub fn set_directory(&mut self, name: &str) {
        if self.directories().any(|existing| existing == name) {
            return;
        }
        self.insert_before_marker(EntryLine::Directory(name.to_string()));
    }

    /// CVSNT leaves a lone `D` line at the end of an `Entries` file that has file entries.
    /// New lines go before it, so the marker stays where CVS expects it.
    fn insert_before_marker(&mut self, line: EntryLine) {
        let position = self
            .lines
            .iter()
            .rposition(|line| matches!(line, EntryLine::Other(text) if text == "D"))
            .unwrap_or(self.lines.len());
        self.lines.insert(position, line);
    }

    pub fn files(&self) -> impl Iterator<Item = &Entry> {
        self.lines.iter().filter_map(|line| match line {
            EntryLine::File(entry) => Some(entry),
            _ => None,
        })
    }

    pub fn directories(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().filter_map(|line| match line {
            EntryLine::Directory(name) => Some(name.as_str()),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-entries-{}-{}-{}",
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
    fn a_pending_log_restores_a_folder_cvs_never_wrote() {
        // The real case: an update created `smc.smc_dict_def` and journaled its `D` line,
        // then died before merging it into `CVS/Entries`. cvs itself would apply the
        // journal at the next open, so the reader has to see the same folder.
        let dir = scratch("log-add");
        std::fs::write(dir.join("CVS").join("Entries"), "/a.txt/1.1///\n").unwrap();
        std::fs::write(
            dir.join("CVS").join("Entries.Log"),
            "A D/smc.smc_dict_def////\r\n",
        )
        .unwrap();

        let entries = Entries::read(&dir).unwrap();
        assert_eq!(
            entries.directories().collect::<Vec<_>>(),
            vec!["smc.smc_dict_def"]
        );
        // Browsing must not settle the journal: only a real cvs command may do that.
        assert!(dir.join("CVS").join("Entries.Log").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("CVS").join("Entries")).unwrap(),
            "/a.txt/1.1///\n"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pending_log_can_add_replace_and_remove_files() {
        let dir = scratch("log-files");
        std::fs::write(
            dir.join("CVS").join("Entries"),
            "/old/1.1///\n/dated/1.1///\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("CVS").join("Entries.Log"),
            "A /new/1.1///\nA /dated/1.2///\nR /old/1.1///\n",
        )
        .unwrap();

        let entries = Entries::read(&dir).unwrap();
        let mut names: Vec<&str> = entries.files().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["dated", "new"]);
        assert_eq!(entries.file("dated").unwrap().revision, "1.2");
        assert!(entries.file("old").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_log_line_this_client_does_not_understand_is_skipped() {
        let dir = scratch("log-odd");
        std::fs::write(dir.join("CVS").join("Entries"), "/kept/1.1///\n").unwrap();
        std::fs::write(
            dir.join("CVS").join("Entries.Log"),
            "? odd\nA\nR /kept/1.1///\n",
        )
        .unwrap();

        let entries = Entries::read(&dir).unwrap();
        assert!(entries.file("kept").is_none(), "the R line still applies");
        assert_eq!(entries.files().count(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_without_a_log_reads_as_before() {
        let dir = scratch("log-absent");
        std::fs::write(dir.join("CVS").join("Entries"), "D/sub////\n").unwrap();
        let entries = Entries::read(&dir).unwrap();
        assert_eq!(entries.directories().collect::<Vec<_>>(), vec!["sub"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_line_keeps_every_field() {
        let entry = Entry::parse("/hello.txt/1.2/Tue Nov 24 09:52:53 2026/-kb/branch").unwrap();
        assert_eq!(entry.name, "hello.txt");
        assert_eq!(entry.revision, "1.2");
        assert_eq!(entry.date, "Tue Nov 24 09:52:53 2026");
        assert_eq!(entry.options, "-kb");
        assert_eq!(entry.tag, "branch");
        assert_eq!(
            entry.line(),
            "/hello.txt/1.2/Tue Nov 24 09:52:53 2026/-kb/branch"
        );
    }

    #[test]
    fn the_short_forms_are_understood() {
        let entry = Entry::parse("/a/0///").unwrap();
        assert!(entry.is_added());
        assert_eq!(entry.options, "");
        assert_eq!(entry.tag, "");
        assert!(Entry::parse("/b/-1/date//").unwrap().is_removed());
        assert!(
            Entry::parse("/c/1.1/Result of merge+Tue Nov 24 09:52:53 2026//")
                .unwrap()
                .is_conflict()
        );
        assert!(Entry::parse("/d/1.1//-ku/").unwrap().is_unicode());
    }

    #[test]
    fn a_directory_line_and_an_unknown_line_survive_a_round_trip() {
        let text = "D/sub////\n/kept/1.1///\n?odd line\n";
        let entries = Entries::parse(text);
        assert_eq!(entries.directories().collect::<Vec<_>>(), vec!["sub"]);
        assert_eq!(entries.files().count(), 1);
        assert_eq!(entries.lines.len(), 3);
        let dir = scratch("roundtrip");
        entries.write(&dir, Codec::Utf8).unwrap();
        let back = Entries::read(&dir).unwrap();
        assert_eq!(back, entries);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_chinese_name_is_written_in_gbk() {
        let dir = scratch("gbk");
        let mut entries = Entries::default();
        entries.set_file(Entry::new("知识库.txt", "1.1", "", "", ""));
        entries.write(&dir, Codec::Gbk).unwrap();
        let bytes = std::fs::read(dir.join("CVS").join("Entries")).unwrap();
        assert!(std::str::from_utf8(&bytes).is_err(), "must be GBK bytes");
        let back = Entries::read(&dir).unwrap();
        assert_eq!(back.file("知识库.txt").unwrap().revision, "1.1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn setting_a_file_replaces_it_in_place() {
        let mut entries = Entries::parse("/a/1.1///\n/b/1.1///\n");
        entries.set_file(Entry::new("a", "1.2", "", "", ""));
        assert_eq!(
            entries.files().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(entries.file("a").unwrap().revision, "1.2");
        entries.remove_file("a");
        assert_eq!(entries.files().count(), 1);
    }

    #[test]
    fn removing_a_directory_drops_only_its_line() {
        let mut entries = Entries::parse("D/sub////\nD/keep////\n/kept/1.1///\n");
        entries.remove_directory("sub");
        assert_eq!(entries.directories().collect::<Vec<_>>(), vec!["keep"]);
        assert_eq!(entries.files().count(), 1);
    }

    #[test]
    fn new_lines_go_before_cvsnts_trailing_marker() {
        let mut entries = Entries::parse("/a/1.1///\nD\n");
        entries.set_file(Entry::new("b", "1.1", "", "", ""));
        entries.set_directory("sub");
        let text = entries
            .lines
            .iter()
            .map(|line| match line {
                EntryLine::File(entry) => entry.line(),
                EntryLine::Directory(name) => format!("D/{name}////"),
                EntryLine::Other(text) => text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.ends_with("\nD"), "{}", text);
        assert!(text.contains("/b/1.1///"), "{}", text);
    }
}
