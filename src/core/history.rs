//! The whole `cvs log` reply for one file: its revisions, tags and branches.
//!
//! [`crate::graph`] reads the revision lines; this module reads the file header and the
//! symbolic names around them, then answers the questions the File History window asks:
//! which revision is the working copy on, what came before a revision, and which tag
//! names a branch lane.

use crate::graph::{self, RevNode};
use crate::tags::{self, TagInfo};

/// One file's revision history as a `cvs log` reply describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    pub file: String,
    /// The `head:` line: the newest revision on the trunk.
    pub head: String,
    /// The `branch:` line: the file's default branch, when it has one.
    pub default_branch: String,
    pub revisions: Vec<RevNode>,
    pub tags: Vec<TagInfo>,
}

impl History {
    /// Read one file's `cvs log` reply. `file` is the working name the window shows.
    pub fn parse(file: &str, lines: &[String]) -> Self {
        let mut history = Self {
            file: file.to_string(),
            revisions: graph::revision_nodes(lines),
            tags: tags::parse_log(lines),
            ..Default::default()
        };
        for raw in lines {
            let line = raw.trim_end_matches(['\r', '\n']);
            if let Some(rest) = line.strip_prefix("head:") {
                if history.head.is_empty() {
                    history.head = rest.trim().to_string();
                }
            } else if let Some(rest) = line.strip_prefix("branch:") {
                if history.default_branch.is_empty() {
                    history.default_branch = rest.trim().to_string();
                }
            }
        }
        // The log lists tags newest first, and filtering keeps that order, so the first
        // name a revision carries is the tag created most recently.
        for node in &mut history.revisions {
            node.tags = history
                .tags
                .iter()
                .filter(|tag| {
                    !tag.branch && tag.revision.as_deref() == Some(node.revision.as_str())
                })
                .map(|tag| tag.name.clone())
                .collect();
        }
        history
    }

    /// The plain tags that point at one revision.
    pub fn tag_names(&self, revision: &str) -> Vec<&str> {
        self.tags
            .iter()
            .filter(|tag| !tag.branch && tag.revision.as_deref() == Some(revision))
            .map(|tag| tag.name.as_str())
            .collect()
    }

    /// The tag that names a branch lane, such as `vendor_acme` for `1.2.4`.
    ///
    /// A branch tag points at `1.2.0.4`, while the revisions on that branch are
    /// `1.2.4.x`; the vendor branch tag points at the branch id itself.
    pub fn branch_label(&self, branch_id: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|tag| {
                tag.revision.as_deref().and_then(branch_tag_id).as_deref() == Some(branch_id)
            })
            .map(|tag| tag.name.as_str())
    }

    /// The branch ids that have revisions in this file, in the order the log lists them.
    pub fn branch_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for node in self.revisions.iter().filter(|node| node.branch) {
            if let Some(id) = graph::branch_id(&node.revision) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        ids
    }

    pub fn branch_count(&self) -> usize {
        self.branch_ids().len()
    }

    pub fn tag_count(&self) -> usize {
        self.tags.len()
    }

    /// The revision the working copy records, or the newest trunk revision when the
    /// working copy is on a revision this log does not contain (for example revision 0
    /// after `cvs add`).
    pub fn working_index(&self, working: &str) -> Option<usize> {
        let working = working.trim();
        if !working.is_empty() && working != "0" {
            if let Some(index) = self
                .revisions
                .iter()
                .position(|node| node.revision == working)
            {
                return Some(index);
            }
        }
        self.latest_trunk_index()
            .or_else(|| (!self.revisions.is_empty()).then_some(0))
    }

    /// The newest revision on the trunk: the `head:` revision when the log names it,
    /// otherwise the first non-branch revision in log order.
    pub fn latest_trunk_index(&self) -> Option<usize> {
        if !self.head.is_empty() {
            if let Some(index) = self
                .revisions
                .iter()
                .position(|node| node.revision == self.head)
            {
                return Some(index);
            }
        }
        self.revisions.iter().position(|node| !node.branch)
    }

    /// The revision immediately older than `index`:
    ///
    /// * on a branch, the next older revision on that branch; the fork revision when this
    ///   is the branch's first revision;
    /// * on the trunk, the next older trunk revision;
    /// * `None` for the first revision of the file.
    pub fn previous_revision(&self, index: usize) -> Option<String> {
        let node = self.revisions.get(index)?;
        if node.branch {
            let id = graph::branch_id(&node.revision)?;
            for older in &self.revisions[index + 1..] {
                if older.branch && graph::branch_id(&older.revision).as_deref() == Some(id.as_str())
                {
                    return Some(older.revision.clone());
                }
            }
            graph::fork_revision(&node.revision)
        } else {
            self.revisions[index + 1..]
                .iter()
                .find(|older| !older.branch)
                .map(|older| older.revision.clone())
        }
    }

    pub fn summary(&self) -> String {
        let branch_word = if self.branch_count() == 1 {
            "branch"
        } else {
            "branches"
        };
        let tag_word = if self.tag_count() == 1 { "tag" } else { "tags" };
        format!(
            "{} revision(s) · {} {} · {} {}",
            self.revisions.len(),
            self.branch_count(),
            branch_word,
            self.tag_count(),
            tag_word
        )
    }
}

/// Turn a branch tag's revision into the id its revisions use: `1.3.0.2` -> `1.3.2`.
/// A tag that already points at a branch id (`1.2.4`, the vendor branch) is kept.
fn branch_tag_id(revision: &str) -> Option<String> {
    if revision.contains(".0.") {
        Some(revision.replace(".0.", "."))
    } else if revision.split('.').count() > 2 {
        Some(revision.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(|line| line.to_string()).collect()
    }

    const LOG: &str = "\
RCS file: /var/lib/cvs/sampleproj/main.c,v
Working file: main.c
head: 1.5
branch:
symbolic names:
\trelease_1_4: 1.4
\tvendor_acme: 1.2.4
\tfeature_x: 1.3.0.2
keyword substitution: kv
total revisions: 8
=============================================================================
revision 1.5
date: 2026/11/22 10:04:11;  author: ann;  state: Exp;  lines: +2 -1
Tidy the message text
----------------------------
revision 1.4
date: 2026/11/21 16:20:02;  author: ann;  state: Exp;  lines: +9 -0
branches:  1.4.2.1;
Refactor upload

The second line is part of the message.
----------------------------
revision 1.3
date: 2026/11/20 11:02:40;  author: bob;  state: Exp;  lines: +1 -1
Fix the misplaced brace
----------------------------
revision 1.2
date: 2026/11/19 09:41:13;  author: bob;  state: Exp;  lines: +1 -0
Tag the vendor import
----------------------------
revision 1.2.4.2
date: 2026/11/20 15:10:00;  author: ann;  state: Exp;  lines: +1 -0
Vendor fix, second round
----------------------------
revision 1.2.4.1
date: 2026/11/19 14:30:00;  author: ann;  state: Exp;  lines: +3 -0
Vendor fix, first round
----------------------------
revision 1.3.2.1
date: 2026/11/20 18:00:00;  author: bob;  state: Exp;  lines: +2 -0
Experiment on a side branch
----------------------------
revision 1.1
date: 2026/11/18 08:15:30;  author: ann;  state: Exp;
Initial revision
=============================================================================
";

    fn parsed() -> History {
        History::parse("main.c", &lines(LOG))
    }

    #[test]
    fn reads_state_lines_and_child_branches_without_polluting_the_message() {
        let history = parsed();
        let node = history
            .revisions
            .iter()
            .find(|node| node.revision == "1.4")
            .expect("revision 1.4 is in the log");

        assert_eq!(node.state, "Exp");
        assert_eq!(node.lines, Some((9, 0)));
        assert_eq!(node.branches, vec!["1.4.2.1"]);
        assert_eq!(node.date_time, "2026-11-21 16:20:02");
        assert_eq!(
            node.message,
            "Refactor upload The second line is part of the message."
        );
        assert_eq!(
            node.message_full,
            "Refactor upload\nThe second line is part of the message."
        );
        assert!(
            !node.message_full.contains("branches:"),
            "the branches line is metadata, not the commit message"
        );
    }

    #[test]
    fn attaches_plain_tags_and_names_branch_lanes() {
        let history = parsed();
        assert_eq!(history.tag_names("1.4"), vec!["release_1_4"]);
        assert_eq!(history.branch_label("1.2.4"), Some("vendor_acme"));
        assert_eq!(history.branch_label("1.3.2"), Some("feature_x"));
        assert_eq!(history.branch_label("9.9.9"), None);
        assert_eq!(history.branch_ids(), vec!["1.2.4", "1.3.2"]);
        assert_eq!(history.branch_count(), 2);
        assert_eq!(history.tag_count(), 3);
    }

    #[test]
    fn previous_revision_follows_the_trunk_and_each_branch() {
        let history = parsed();
        let index = |revision: &str| {
            history
                .revisions
                .iter()
                .position(|node| node.revision == revision)
                .expect("the revision is in the log")
        };

        assert_eq!(
            history.previous_revision(index("1.3")).as_deref(),
            Some("1.2")
        );
        assert_eq!(
            history.previous_revision(index("1.2.4.2")).as_deref(),
            Some("1.2.4.1")
        );
        assert_eq!(
            history.previous_revision(index("1.2.4.1")).as_deref(),
            Some("1.2"),
            "a branch's first revision follows its fork"
        );
        assert_eq!(history.previous_revision(index("1.1")), None);
    }

    #[test]
    fn the_working_revision_falls_back_to_the_latest_trunk() {
        let history = parsed();
        assert_eq!(history.working_index("1.3"), Some(2));
        assert_eq!(
            history.working_index("0"),
            Some(0),
            "an added file has no revision 0 in the log"
        );
        assert_eq!(
            history.working_index("9.9"),
            Some(0),
            "a revision missing from the log falls back to the head"
        );
    }

    #[test]
    fn the_sample_log_used_for_the_window_labels_its_branches() {
        let log: Vec<String> = crate::demo::SAMPLE_LOG
            .lines()
            .map(str::to_string)
            .collect();
        let history = History::parse("main.c", &log);
        assert_eq!(history.branch_label("1.2.4"), Some("vendor_acme"));
        assert_eq!(history.branch_label("1.3.2"), Some("feature_x"));
    }

    #[test]
    fn a_real_cvsnt_reply_reads_its_vendor_branch_and_tags() {
        let log = lines(
            "RCS file: C:/repo/proj/file.txt,v\n\
             Working file: file.txt\n\
             head: 1.2\n\
             branch:\n\
             locks: strict\n\
             access list:\n\
             symbolic names:\n\
             \tbranch_x: 1.2.0.2\n\
             \trelease_1_0: 1.2\n\
             \tstart: 1.1.1.1\n\
             \tvendor: 1.1.1\n\
             keyword substitution: kv\n\
             total revisions: 4;\tselected revisions: 4\n\
             description:\n\
             ----------------------------\n\
             revision 1.2\n\
             date: 2026/11/27 02:13:22;  author: Woolsen;  state: Exp;  lines: +1 -0\n\
             branches:  1.2.2;\n\
             second\n\
             ----------------------------\n\
             revision 1.1\n\
             date: 2026/11/27 02:13:20;  author: Woolsen;  state: Exp;\n\
             branches:  1.1.1;\n\
             Initial revision\n\
             ----------------------------\n\
             revision 1.1.1.1\n\
             date: 2026/11/27 02:13:20;  author: Woolsen;  state: Exp;  lines: +0 -0\n\
             initial\n\
             ----------------------------\n\
             revision 1.2.2.1\n\
             date: 2026/11/27 02:13:23;  author: Woolsen;  state: Exp;  lines: +1 -0\n\
             branch change\n\
             =============================================================================\n",
        );
        let history = History::parse("file.txt", &log);
        assert_eq!(history.branch_label("1.2.2"), Some("branch_x"));
        assert_eq!(history.branch_label("1.1.1"), Some("vendor"));
        assert_eq!(history.tag_names("1.2"), vec!["release_1_0"]);
        assert_eq!(history.tag_names("1.1.1.1"), vec!["start"]);
        assert_eq!(history.revisions.len(), 4);
        assert_eq!(
            history.previous_revision(3).as_deref(),
            Some("1.2"),
            "the first revision on a branch follows its fork"
        );
    }

    #[test]
    fn a_reply_without_revisions_has_no_working_row() {
        let history = History::parse("main.c", &lines("RCS file: main.c,v\n"));
        assert!(history.revisions.is_empty());
        assert_eq!(history.working_index("1.1"), None);
        assert_eq!(history.latest_trunk_index(), None);
    }
}
