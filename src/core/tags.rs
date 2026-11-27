//! The tags a `cvs log` reply lists, and the names cvs accepts for a new one.
//!
//! `cvs log -h` prints, for each file, a `branch:` header and a `symbolic names:` block
//! whose rows are `\tname: revision`. A tag that points at a `...0.N` revision is a branch
//! tag; so is the file's own vendor branch, named by the `branch:` header. The window shows
//! one row per tag for the files in scope, so the same name seen in several files is merged.
//!
//! The rows keep the log's order. That order carries the only hint of age a tag has: CVS
//! writes a new tag at the top of the RCS symbol table, so the log lists tags newest first.
//! There is no tag time anywhere in the reply, so this order is all a window can go on.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagInfo {
    pub name: String,
    pub branch: bool,
    /// The revision all files agree on, or `None` when the tag sits on different revisions.
    pub revision: Option<String>,
}

impl TagInfo {
    pub fn type_label(&self) -> &'static str {
        if self.branch {
            "branch"
        } else {
            "tag"
        }
    }

    pub fn revision_label(&self) -> &str {
        self.revision.as_deref().unwrap_or("—")
    }
}

/// Read the `symbolic names:` blocks of a `cvs log` reply, merging repeated names.
///
/// The rows come back in the order the log prints them, newest tag first. A name that
/// another file lists again keeps the place it was first seen at.
pub fn parse_log(lines: &[String]) -> Vec<TagInfo> {
    struct Seen {
        branch: bool,
        revision: String,
        varies: bool,
    }

    // A `Vec` keeps the log's order; the map only finds a name already collected, so a
    // tag seen in several files is merged where it first appeared instead of moved.
    let mut ordered: Vec<(String, Seen)> = Vec::new();
    let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut in_symbolic = false;
    let mut file_branch = String::new();
    for raw in lines {
        let line = raw.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        if trimmed.starts_with("RCS file:") {
            in_symbolic = false;
            file_branch.clear();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("branch:") {
            file_branch = rest.trim().to_string();
            in_symbolic = false;
            continue;
        }
        if trimmed.starts_with("symbolic names:") {
            in_symbolic = true;
            continue;
        }
        if !in_symbolic || line.is_empty() {
            continue;
        }
        // The block ends at the first line that starts in column 0, such as
        // `keyword substitution:`. A log message further down must never be read as a tag.
        if !line.starts_with(|c: char| c.is_whitespace()) {
            in_symbolic = false;
            continue;
        }
        let Some((name, revision)) = trimmed.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let revision = revision.trim();
        if name.is_empty() || revision.is_empty() {
            continue;
        }
        let branch =
            revision.contains(".0.") || (!file_branch.is_empty() && revision == file_branch);
        if let Some(&index) = at.get(name) {
            let entry = &mut ordered[index].1;
            entry.branch |= branch;
            if entry.revision != revision {
                entry.varies = true;
            }
        } else {
            at.insert(name.to_string(), ordered.len());
            ordered.push((
                name.to_string(),
                Seen {
                    branch,
                    revision: revision.to_string(),
                    varies: false,
                },
            ));
        }
    }

    ordered
        .into_iter()
        .map(|(name, entry)| TagInfo {
            name,
            branch: entry.branch,
            revision: if entry.varies {
                None
            } else {
                Some(entry.revision)
            },
        })
        .collect()
}

/// Why `name` cannot be handed to `cvs tag`, or `None` when it can. The rules are cvs's:
/// a letter first, then letters, digits, `-` and `_`. Refusing a leading `-` also keeps a
/// typed name from being read as a command switch.
pub fn name_problem(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some("A tag name is needed".to_string());
    }
    let mut chars = name.chars();
    let first = chars
        .next()
        .expect("a non-empty name has a first character");
    if !first.is_alphabetic() {
        return Some("A tag name must start with a letter".to_string());
    }
    if !chars.all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Some("Tag names can use letters, digits, - and _ only".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(|line| line.to_string()).collect()
    }

    fn tag(name: &str, branch: bool, revision: Option<&str>) -> TagInfo {
        TagInfo {
            name: name.to_string(),
            branch,
            revision: revision.map(|r| r.to_string()),
        }
    }

    #[test]
    fn a_plain_tag_a_branch_tag_and_the_vendor_branch_are_told_apart() {
        let log = lines(
            "RCS file: /repo/a.txt,v\n\
             Working file: a.txt\n\
             head: 1.4\n\
             branch: 1.1.1\n\
             symbolic names:\n\
             \tRelease_1_0: 1.2\n\
             \tFeature_X: 1.3.0.2\n\
             \tvendor_acme: 1.1.1\n\
             keyword substitution: kv\n\
             total revisions: 4\n",
        );

        assert_eq!(
            parse_log(&log),
            vec![
                tag("Release_1_0", false, Some("1.2")),
                tag("Feature_X", true, Some("1.3.0.2")),
                tag("vendor_acme", true, Some("1.1.1")),
            ]
        );
    }

    #[test]
    fn the_log_order_is_kept_so_the_newest_tag_comes_first() {
        // CVS writes a new symbol at the top of the RCS table: `some_random_tag` was
        // created before `ANOTHER-TAG`, and the log lists ANOTHER-TAG first.
        let log = lines(
            "RCS file: foo.jpg,v\n\
             head: 1.1\n\
             branch:\n\
             symbolic names:\n\
             \tANOTHER-TAG: 1.1\n\
             \tsome_random_tag: 1.1\n\
             keyword substitution: b\n",
        );

        assert_eq!(
            parse_log(&log),
            vec![
                tag("ANOTHER-TAG", false, Some("1.1")),
                tag("some_random_tag", false, Some("1.1")),
            ],
            "sorting by name would hide which tag is the newer one"
        );
    }

    #[test]
    fn a_tag_seen_in_several_files_keeps_the_place_it_was_first_seen_at() {
        let log = lines(
            "RCS file: a.txt,v\n\
             branch:\n\
             symbolic names:\n\
             \tT_COMMON: 1.1\n\
             \tT_ONLY_A: 1.1\n\
             keyword substitution: kv\n\
             =============================================================================\n\
             RCS file: b.txt,v\n\
             branch:\n\
             symbolic names:\n\
             \tZ_NEWER: 1.2\n\
             \tT_COMMON: 1.2\n\
             keyword substitution: kv\n",
        );

        assert_eq!(
            parse_log(&log),
            vec![
                tag("T_COMMON", false, None),
                tag("T_ONLY_A", false, Some("1.1")),
                tag("Z_NEWER", false, Some("1.2")),
            ]
        );
    }

    #[test]
    fn a_colon_line_after_the_block_is_not_a_tag() {
        let log = lines(
            "RCS file: a.txt,v\n\
             branch:\n\
             symbolic names:\n\
             \tT_REAL: 1.1\n\
             keyword substitution: kv\n\
             =============================================================================\n\
             revision 1.1\n\
             \tT_FAKE: 1.0\n",
        );

        assert_eq!(parse_log(&log), vec![tag("T_REAL", false, Some("1.1"))]);
    }

    #[test]
    fn a_reply_without_tags_yields_no_rows() {
        assert!(parse_log(&[]).is_empty());
        assert!(parse_log(&lines(
            "RCS file: a.txt,v\nsymbolic names:\nkeyword substitution: kv\n"
        ))
        .is_empty());
    }

    #[test]
    fn carriage_returns_do_not_stick_to_tag_names() {
        let log = vec![
            "symbolic names:".to_string(),
            "\tT_CRLF: 1.1\r".to_string(),
            "keyword substitution: kv".to_string(),
        ];

        assert_eq!(parse_log(&log), vec![tag("T_CRLF", false, Some("1.1"))]);
    }

    #[test]
    fn the_sample_log_used_for_the_screenshot_reads_as_tags_and_branches() {
        let log: Vec<String> = crate::demo::SAMPLE_TAGS_LOG
            .lines()
            .map(|line| line.to_string())
            .collect();
        let tags = parse_log(&log);
        assert_eq!(
            tags.iter()
                .map(|tag| (tag.name.as_str(), tag.branch))
                .collect::<Vec<_>>(),
            vec![
                ("release_1_4", false),
                ("vendor_acme", true),
                ("feature_x", true),
            ]
        );
    }

    #[test]
    fn a_tag_name_starts_with_a_letter_and_uses_only_word_characters() {
        assert_eq!(name_problem("Release_1_0"), None);
        assert_eq!(name_problem("T"), None);
        assert_eq!(name_problem("  Release_1_0  "), None);
        assert_eq!(name_problem("发布_1"), None);
        assert_eq!(name_problem(""), Some("A tag name is needed".to_string()));
        assert_eq!(
            name_problem("   "),
            Some("A tag name is needed".to_string())
        );
        assert_eq!(
            name_problem("-d"),
            Some("A tag name must start with a letter".to_string())
        );
        assert_eq!(
            name_problem("1T"),
            Some("A tag name must start with a letter".to_string())
        );
        assert_eq!(
            name_problem("T 1"),
            Some("Tag names can use letters, digits, - and _ only".to_string())
        );
        assert_eq!(
            name_problem("T.1"),
            Some("Tag names can use letters, digits, - and _ only".to_string())
        );
    }
}
