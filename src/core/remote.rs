//! The names a repository holds, read from `cvs rlog -R`.
//!
//! `rlog -R` prints the repository's RCS file names and nothing else. It does not send a
//! file's contents, which is what lets the file list show names that are not checked out.
//! Directories are announced by the server's own `cvs rlog: Logging <path>` lines.

use std::collections::{BTreeSet, HashSet};

use crate::entries::{Origin, Row, State};

/// The direct files and immediate child folders of one repository folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub dirs: Vec<String>,
    pub files: Vec<String>,
}

/// One direct child the parser has not reported before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub is_dir: bool,
}

/// Reads the `rlog -R` lines one at a time and reports each direct child the first time
/// it appears. [`finish`](Self::finish) returns the same listing [`parse`] would.
pub struct Parser {
    requested: String,
    root: String,
    dirs: BTreeSet<String>,
    files: BTreeSet<String>,
    seen: HashSet<String>,
}

impl Parser {
    pub fn new(requested: &str, root_path: &str) -> Self {
        Self {
            requested: clean(requested),
            root: clean(root_path),
            dirs: BTreeSet::new(),
            files: BTreeSet::new(),
            seen: HashSet::new(),
        }
    }

    /// Feed one line. Returns the name only when this line is the first to show it.
    pub fn push(&mut self, raw: &str) -> Option<Found> {
        let line = raw.trim_end_matches('\r').trim();
        if let Some(path) = line.strip_prefix("cvs rlog: Logging ") {
            let relative = relative_to(path, &self.requested, &self.root)?;
            if relative.is_empty() {
                return None;
            }
            let first = relative.split('/').next()?;
            if first.is_empty() || first == "CVS" {
                return None;
            }
            return self.remember(first, true);
        }

        let path = line.strip_suffix(",v")?;
        let relative = relative_to(path, &self.requested, &self.root)?;
        if relative.is_empty()
            || relative.split('/').any(|part| part == "Attic")
            || relative.starts_with('.')
            // A file below a child folder is not part of this folder's own list. The child
            // folder is shown, and opening it runs its own `rlog`.
            || relative.contains('/')
        {
            return None;
        }
        self.remember(&relative, false)
    }

    fn remember(&mut self, name: &str, is_dir: bool) -> Option<Found> {
        if !self.seen.insert(name.to_lowercase()) {
            return None;
        }
        let name = name.to_string();
        if is_dir {
            self.dirs.insert(name.clone());
        } else {
            self.files.insert(name.clone());
        }
        Some(Found { name, is_dir })
    }

    /// The complete listing seen so far.
    pub fn finish(self) -> Listing {
        Listing {
            dirs: self.dirs.into_iter().collect(),
            files: self.files.into_iter().collect(),
        }
    }
}

/// Read the output of `cvs rlog -R <requested>`.
///
/// `root_path` is the path part of `CVS/Root`; server file names are absolute and start
/// with it. When a server reports a different prefix (a chroot or a proxy), the requested
/// folder is used as the fallback anchor.
pub fn parse(lines: &[String], requested: &str, root_path: &str) -> Listing {
    let mut parser = Parser::new(requested, root_path);
    for line in lines {
        parser.push(line);
    }
    parser.finish()
}

/// Merge a repository listing into the rows read from disk.
///
/// Local rows win: a name that is already in the working copy is never offered twice.
/// Remote rows from an earlier listing are dropped first, so applying a newer listing is
/// idempotent.
pub fn merge_rows(local: Vec<Row>, listing: &Listing) -> Vec<Row> {
    let mut rows: Vec<Row> = local
        .into_iter()
        .filter(|row| row.origin == Origin::Local)
        .collect();
    let found: Vec<Found> = listing
        .files
        .iter()
        .map(|name| Found {
            name: name.clone(),
            is_dir: false,
        })
        .chain(listing.dirs.iter().map(|name| Found {
            name: name.clone(),
            is_dir: true,
        }))
        .collect();
    add_new_rows(&mut rows, &found);
    rows
}

/// Add names the list does not already know. Local rows win: a name that is present in
/// any case is not offered twice. Returns how many rows were added.
pub fn add_new_rows(rows: &mut Vec<Row>, found: &[Found]) -> usize {
    let mut known: HashSet<String> = rows.iter().map(|row| row.name.to_lowercase()).collect();
    let mut added = 0;
    for name in found {
        if known.insert(name.name.to_lowercase()) {
            rows.push(remote_row(&name.name, name.is_dir));
            added += 1;
        }
    }
    added
}

fn remote_row(name: &str, is_dir: bool) -> Row {
    Row {
        name: name.to_string(),
        is_dir,
        state: State::Remote,
        origin: Origin::Remote,
        revision: String::new(),
        tag: String::new(),
        options: String::new(),
        timestamp: String::new(),
        binary: false,
        unicode: false,
    }
}

/// The first line that says the listing did not work. `rlog` can report a missing module
/// while still exiting successfully, so the exit code alone is not enough.
pub fn error_line(lines: &[String]) -> Option<&str> {
    lines.iter().map(|line| line.trim()).find(|line| {
        line.contains("[rlog aborted]")
            || line.contains("rlog: cannot find module")
            || line.contains("server does not support")
            || line.contains("No CVSROOT specified")
    })
}

fn clean(path: &str) -> String {
    path.replace('\\', "/")
        .trim()
        .trim_matches('/')
        .trim_start_matches("./")
        .to_string()
}

/// A path in the output made relative to `requested`.
fn relative_to(path: &str, requested: &str, root: &str) -> Option<String> {
    let path = clean(path);
    if !root.is_empty() {
        let root = root.trim_end_matches('/');
        if let Some(rest) = path.strip_prefix(root) {
            let rest = rest.trim_start_matches('/');
            return relative_below(rest, requested);
        }
    }
    // A server that does not use the root's own spelling still ends the absolute path
    // with the requested repository path.
    if requested.is_empty() {
        return Some(path.trim_start_matches('/').to_string());
    }
    // The client may print the `Logging` paths relative to the repository root.
    if path == requested {
        return Some(String::new());
    }
    if let Some(rest) = path.strip_prefix(&format!("{}/", requested)) {
        return Some(rest.to_string());
    }
    let needle = format!("/{}/", requested);
    let at = path.find(&needle)?;
    Some(path[at + needle.len()..].to_string())
}

fn relative_below(relative: &str, requested: &str) -> Option<String> {
    if requested.is_empty() {
        return Some(relative.to_string());
    }
    if relative == requested {
        return Some(String::new());
    }
    let prefix = format!("{}/", requested);
    relative.strip_prefix(&prefix).map(|rest| rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::{Origin, Row, State};

    fn row(name: &str, is_dir: bool) -> Row {
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

    #[test]
    fn parses_direct_files_and_immediate_folders() {
        let lines = vec![
            "cvs rlog: Logging Develop/Design".to_string(),
            "cvs rlog: Logging Develop/Design/DatabaseDesign".to_string(),
            "/srv/cvs/project/Develop/Design/DatabaseDesign/schema.txt,v".to_string(),
            "/srv/cvs/project/Develop/Design/接口设计.txt,v".to_string(),
            "/srv/cvs/project/Develop/Design/HLD/doc.txt,v".to_string(),
            "cvs rlog: Logging Develop/Design/HLD/Review".to_string(),
        ];

        let listing = parse(&lines, "Develop/Design", "/srv/cvs/project");

        assert_eq!(listing.files, vec!["接口设计.txt"]);
        assert_eq!(listing.dirs, vec!["DatabaseDesign", "HLD"]);
    }

    #[test]
    fn the_requested_folder_itself_is_not_a_child() {
        let lines = vec!["cvs rlog: Logging Develop/Design".to_string()];
        let listing = parse(&lines, "Develop/Design", "/srv/cvs/project");
        assert!(listing.dirs.is_empty());
        assert!(listing.files.is_empty());
    }

    #[test]
    fn a_server_path_that_does_not_match_the_root_is_still_anchored_to_the_request() {
        let lines = vec![
            "cvs rlog: Logging Develop/Design".to_string(),
            "/srv/other/Develop/Design/one.txt,v".to_string(),
        ];
        let listing = parse(&lines, "Develop/Design", "/srv/cvs/project");
        assert_eq!(listing.files, vec!["one.txt"]);
    }

    #[test]
    fn removed_attic_files_and_dot_names_are_not_listed() {
        let lines = vec![
            "/srv/cvs/project/Develop/Design/Attic/gone.txt,v".to_string(),
            "/srv/cvs/project/Develop/Design/.cvsignore,v".to_string(),
            "/srv/cvs/project/Develop/Design/kept.txt,v".to_string(),
        ];
        let listing = parse(&lines, "Develop/Design", "/srv/cvs/project");
        assert_eq!(listing.files, vec!["kept.txt"]);
    }

    #[test]
    fn names_with_spaces_commas_and_chinese_survive() {
        let lines = vec![
            "/srv/cvs/project/Develop/Design/项目 说明, v1.doc,v".to_string(),
            "/srv/cvs/project/Develop/Design/a,b.txt,v".to_string(),
        ];
        let listing = parse(&lines, "Develop/Design", "/srv/cvs/project");
        assert_eq!(listing.files, vec!["a,b.txt", "项目 说明, v1.doc"]);
    }

    #[test]
    fn merging_keeps_local_rows_and_appends_only_new_names() {
        let local = vec![row("a.txt", false), row("sub", true)];
        let listing = Listing {
            files: vec!["a.txt".to_string(), "b.txt".to_string()],
            dirs: vec!["sub".to_string(), "more".to_string()],
        };

        let merged = merge_rows(local, &listing);

        assert_eq!(merged.len(), 4);
        assert_eq!(merged[0].name, "a.txt");
        assert_eq!(merged[0].origin, Origin::Local);
        assert_eq!(merged[2].name, "b.txt");
        assert_eq!(merged[2].origin, Origin::Remote);
        assert_eq!(merged[2].state, State::Remote);
        assert!(!merged[2].is_dir);
        assert_eq!(merged[3].name, "more");
        assert!(merged[3].is_dir);
        assert_eq!(merged[3].origin, Origin::Remote);
    }

    #[test]
    fn a_local_name_wins_regardless_of_case() {
        let local = vec![row("Readme.txt", false)];
        let listing = Listing {
            files: vec!["readme.txt".to_string()],
            dirs: Vec::new(),
        };
        let merged = merge_rows(local, &listing);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origin, Origin::Local);
    }

    #[test]
    fn a_new_listing_replaces_the_previous_remote_rows() {
        let local = vec![row("a.txt", false)];
        let first = Listing {
            files: vec!["b.txt".to_string()],
            dirs: Vec::new(),
        };
        let second = Listing {
            files: vec!["c.txt".to_string()],
            dirs: Vec::new(),
        };

        let once = merge_rows(local, &first);
        let twice = merge_rows(once, &second);

        assert_eq!(twice.len(), 2);
        assert_eq!(twice[0].name, "a.txt");
        assert_eq!(twice[1].name, "c.txt");
        assert!(!twice.iter().any(|row| row.name == "b.txt"));
    }

    #[test]
    fn a_missing_module_is_an_error_even_when_the_exit_code_is_zero() {
        let lines = vec!["cvs rlog: cannot find module `Develop/Design' - ignored".to_string()];
        assert!(error_line(&lines).is_some());
        assert!(error_line(&["cvs rlog: Logging Develop".to_string()]).is_none());
    }

    #[test]
    fn a_parser_reports_each_new_name_once_as_it_arrives() {
        let mut parser = Parser::new("Develop/Design", "/srv/cvs/project");
        let lines = [
            "cvs rlog: Logging Develop/Design",
            "/srv/cvs/project/Develop/Design/a.txt,v",
            "/srv/cvs/project/Develop/Design/a.txt,v",
            "cvs rlog: Logging Develop/Design/HLD",
            "/srv/cvs/project/Develop/Design/HLD/doc.txt,v",
            "cvs rlog: Logging Develop/Design/HLD",
            "/srv/cvs/project/Develop/Design/b.txt,v",
        ];
        let found: Vec<(String, bool)> = lines
            .iter()
            .filter_map(|line| parser.push(line).map(|name| (name.name, name.is_dir)))
            .collect();

        assert_eq!(
            found,
            vec![
                ("a.txt".to_string(), false),
                ("HLD".to_string(), true),
                ("b.txt".to_string(), false),
            ]
        );
        let listing = parser.finish();
        assert_eq!(listing.files, vec!["a.txt", "b.txt"]);
        assert_eq!(listing.dirs, vec!["HLD"]);
    }

    #[test]
    fn a_parser_skips_the_requested_folder_attic_and_dot_names() {
        let mut parser = Parser::new("Develop/Design", "/srv/cvs/project");
        for line in [
            "cvs rlog: Logging Develop/Design",
            "/srv/cvs/project/Develop/Design/Attic/gone.txt,v",
            "/srv/cvs/project/Develop/Design/.cvsignore,v",
        ] {
            assert!(parser.push(line).is_none(), "{line}");
        }
        assert_eq!(parser.finish(), Listing::default());
    }

    #[test]
    fn names_found_by_the_parser_can_be_added_to_rows_as_they_arrive() {
        let mut rows = vec![row("local.txt", false)];
        let found = vec![
            Found {
                name: "local.txt".to_string(),
                is_dir: false,
            },
            Found {
                name: "server.txt".to_string(),
                is_dir: false,
            },
            Found {
                name: "more".to_string(),
                is_dir: true,
            },
        ];

        let added = add_new_rows(&mut rows, &found);

        assert_eq!(added, 2);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].name, "server.txt");
        assert_eq!(rows[1].origin, Origin::Remote);
        assert_eq!(rows[1].state, State::Remote);
        assert!(rows[2].is_dir);
        assert_eq!(rows[2].origin, Origin::Remote);
    }
}
