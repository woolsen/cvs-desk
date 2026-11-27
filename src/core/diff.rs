//! Reading the unified diff text that `cvs diff -u` prints.
//!
//! The window does not compute diffs itself: the server already compared the two
//! revisions and sent the text. This module only gives that text a shape the diff view
//! can draw: which file each line belongs to, whether it was added, removed or kept,
//! and the old/new line number on each side.
//!
//! Anything that is not a hunk body (the `Index:` line, RCS headers, `---`/`+++`,
//! `@@` hunk markers and `\ No newline at end of file`) is kept as metadata, so a reply
//! that is not a diff at all still shows every line instead of disappearing.

/// One `cvs diff` reply, split into the files it mentions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub files: Vec<FileDiff>,
}

/// One file's part of a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// The path named by `Index:`, `+++`, `diff` or `RCS file:`, when there is one.
    pub path: String,
    pub lines: Vec<Line>,
}

/// What a line in a diff means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An unchanged line shown for context.
    Context,
    /// A line that exists only in the newer revision.
    Added,
    /// A line that exists only in the older revision.
    Removed,
    /// An `@@ -a,b +c,d @@` hunk marker.
    Hunk,
    /// A header, a binary-file note, a `\ No newline` marker, or any non-diff text.
    Meta,
}

/// One drawable line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    /// The line number in the older revision. `None` on an added line.
    pub old: Option<usize>,
    /// The line number in the newer revision. `None` on a removed line.
    pub new: Option<usize>,
    /// The line's text without its `+`, `-` or space marker. Metadata keeps its text
    /// as written, which is what makes headers readable.
    pub text: String,
}

impl Diff {
    /// How many lines the view has to draw.
    pub fn len(&self) -> usize {
        self.files.iter().map(|file| file.lines.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The line at a flat index, with the file it belongs to.
    ///
    /// The view draws one scrolling list across all files, so it indexes rows rather
    /// than walking each file's own list. A reply usually names one file, so the walk
    /// stays short.
    pub fn line(&self, mut index: usize) -> Option<(usize, &FileDiff, &Line)> {
        for (file_index, file) in self.files.iter().enumerate() {
            if index < file.lines.len() {
                return Some((file_index, file, &file.lines[index]));
            }
            index -= file.lines.len();
        }
        None
    }
}

/// Read a whole diff reply.
pub fn parse(text: &str) -> Diff {
    let mut parser = Parser::default();
    for line in text.lines() {
        parser.feed(line.strip_suffix('\r').unwrap_or(line));
    }
    parser.finish()
}

#[derive(Default)]
struct Parser {
    files: Vec<FileDiff>,
    /// The file new lines go into. `None` until a line arrives.
    current: Option<usize>,
    /// True while a hunk's body is being read. Headers after a hunk must not be read
    /// as context lines.
    in_hunk: bool,
    old_next: usize,
    new_next: usize,
    old_left: usize,
    new_left: usize,
}

impl Parser {
    fn feed(&mut self, line: &str) {
        if let Some(path) = line.strip_prefix("Index: ") {
            self.start_file(path.trim());
            self.push(Kind::Meta, None, None, line);
            return;
        }

        if self.in_hunk {
            if let Some(kind) = body_kind(line) {
                self.body(kind, line);
                return;
            }
            if line.starts_with('\\') {
                // The marker belongs to the line above it and has no number of its own.
                self.push(Kind::Meta, None, None, line);
                return;
            }
            // Anything else ends the hunk. A blank separator between files is the
            // common case; a wrong count in a hunk header must not swallow the header
            // that follows it.
            self.in_hunk = false;
        }

        if let Some((old_start, old_len, new_start, new_len)) = hunk_header(line) {
            self.ensure_file();
            self.in_hunk = true;
            self.old_next = old_start;
            self.new_next = new_start;
            self.old_left = old_len;
            self.new_left = new_len;
            self.push(Kind::Hunk, None, None, line);
            return;
        }

        self.meta(line);
    }

    fn finish(self) -> Diff {
        Diff { files: self.files }
    }

    fn start_file(&mut self, path: &str) {
        self.in_hunk = false;
        self.files.push(FileDiff {
            path: path.to_string(),
            lines: Vec::new(),
        });
        self.current = Some(self.files.len() - 1);
    }

    fn ensure_file(&mut self) {
        if self.current.is_none() {
            self.start_file("");
        }
    }

    fn push(&mut self, kind: Kind, old: Option<usize>, new: Option<usize>, text: &str) {
        self.ensure_file();
        let index = self.current.expect("ensure_file set a file");
        self.files[index].lines.push(Line {
            kind,
            old,
            new,
            text: text.to_string(),
        });
    }

    fn body(&mut self, kind: Kind, line: &str) {
        let text = &line[1..];
        match kind {
            Kind::Context => {
                let old = self.old_next;
                let new = self.new_next;
                self.old_next += 1;
                self.new_next += 1;
                self.old_left = self.old_left.saturating_sub(1);
                self.new_left = self.new_left.saturating_sub(1);
                self.push(kind, Some(old), Some(new), text);
            }
            Kind::Removed => {
                let old = self.old_next;
                self.old_next += 1;
                self.old_left = self.old_left.saturating_sub(1);
                self.push(kind, Some(old), None, text);
            }
            Kind::Added => {
                let new = self.new_next;
                self.new_next += 1;
                self.new_left = self.new_left.saturating_sub(1);
                self.push(kind, None, Some(new), text);
            }
            Kind::Hunk | Kind::Meta => {}
        }
        if self.old_left == 0 && self.new_left == 0 {
            self.in_hunk = false;
        }
    }

    fn meta(&mut self, line: &str) {
        self.ensure_file();
        if let Some(path) = new_path(line) {
            let index = self.current.expect("ensure_file set a file");
            if self.files[index].path.is_empty() {
                self.files[index].path = path;
            }
        }
        if self.files[self.current.expect("ensure_file set a file")]
            .path
            .is_empty()
        {
            if let Some(path) = path_from_rcs(line).or_else(|| path_from_diff(line)) {
                let index = self.current.expect("ensure_file set a file");
                self.files[index].path = path;
            }
        }
        self.push(Kind::Meta, None, None, line);
    }
}

/// The marker that turns a line into a hunk body line.
fn body_kind(line: &str) -> Option<Kind> {
    match line.as_bytes().first()? {
        b' ' => Some(Kind::Context),
        b'-' => Some(Kind::Removed),
        b'+' => Some(Kind::Added),
        _ => None,
    }
}

/// `@@ -old_start,old_len +new_start,new_len @@`.
///
/// The length is 1 when the comma is absent, as unified diff writes single-line hunks.
fn hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_spec, rest) = rest.split_once(" +")?;
    let (new_spec, _) = rest.split_once(" @@")?;
    let (old_start, old_len) = range(old_spec)?;
    let (new_start, new_len) = range(new_spec)?;
    Some((old_start, old_len, new_start, new_len))
}

fn range(spec: &str) -> Option<(usize, usize)> {
    match spec.split_once(',') {
        Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
        None => Some((spec.parse().ok()?, 1)),
    }
}

/// The newer file name from `+++ b/path<TAB>date`.
fn new_path(line: &str) -> Option<String> {
    let path = line.strip_prefix("+++ ")?;
    let path = path.split('\t').next().unwrap_or(path).trim();
    if path.is_empty() || path == "/dev/null" {
        return None;
    }
    let path = path
        .strip_prefix("b/")
        .or_else(|| path.strip_prefix("a/"))
        .unwrap_or(path);
    Some(path.replace('\\', "/"))
}

/// `RCS file: /repo/path/file.c,v` names the file even when no `Index:` line came.
fn path_from_rcs(line: &str) -> Option<String> {
    let path = line.strip_prefix("RCS file: ")?.trim();
    let path = path.strip_suffix(",v")?;
    Some(path.rsplit('/').next().unwrap_or(path).replace('\\', "/"))
}

/// `diff -u -r1.1 -r1.2 path` names the file as its last word.
fn path_from_diff(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff ")?;
    let path = rest.split_whitespace().last()?;
    if path.starts_with('-') || path.is_empty() {
        return None;
    }
    Some(path.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(diff: &Diff) -> Vec<&Line> {
        diff.files
            .iter()
            .flat_map(|file| &file.lines)
            .filter(|line| {
                matches!(
                    line.kind,
                    Kind::Context | Kind::Added | Kind::Removed | Kind::Hunk
                )
            })
            .collect()
    }

    #[test]
    fn a_unified_diff_numbers_both_sides() {
        let text = "\
Index: main.c
===================================================================
RCS file: /repo/main.c,v
retrieving revision 1.1
retrieving revision 1.2
diff -u -r1.1 -r1.2 main.c
--- main.c\t2026/11/21 16:20:02
+++ main.c\t2026/11/22 10:04:11
@@ -1,3 +1,3 @@
 #include <stdio.h>
-int main(void) { return 0; }
+int main(void) { return 1; }
 /* end */
";
        let diff = parse(text);
        assert_eq!(diff.files.len(), 1, "one file");
        assert_eq!(diff.files[0].path, "main.c");

        let lines = body(&diff);
        assert_eq!(lines.len(), 5, "a hunk marker and four body lines");
        assert_eq!(lines[0].kind, Kind::Hunk);
        assert_eq!(lines[0].old, None);
        assert_eq!(lines[0].new, None);

        assert_eq!(lines[1].kind, Kind::Context);
        assert_eq!((lines[1].old, lines[1].new), (Some(1), Some(1)));
        assert_eq!(lines[1].text, "#include <stdio.h>");

        assert_eq!(lines[2].kind, Kind::Removed);
        assert_eq!((lines[2].old, lines[2].new), (Some(2), None));
        assert_eq!(lines[2].text, "int main(void) { return 0; }");

        assert_eq!(lines[3].kind, Kind::Added);
        assert_eq!((lines[3].old, lines[3].new), (None, Some(2)));
        assert_eq!(lines[3].text, "int main(void) { return 1; }");

        assert_eq!(lines[4].kind, Kind::Context);
        assert_eq!((lines[4].old, lines[4].new), (Some(3), Some(3)));
    }

    #[test]
    fn two_files_are_kept_apart() {
        let text = "\
Index: a.c
===================================================================
@@ -1 +1 @@
-old
+new
Index: b.c
===================================================================
@@ -7,2 +7,2 @@
 same
-old
+new
";
        let diff = parse(text);
        assert_eq!(diff.files.len(), 2);
        assert_eq!(diff.files[0].path, "a.c");
        assert_eq!(diff.files[1].path, "b.c");
        assert_eq!(
            diff.files[1]
                .lines
                .iter()
                .filter(|line| matches!(line.kind, Kind::Context | Kind::Added | Kind::Removed))
                .count(),
            3
        );
        assert_eq!(diff.len(), diff.files.iter().map(|f| f.lines.len()).sum());
    }

    #[test]
    fn a_file_without_an_index_line_takes_the_new_path() {
        let text = "\
--- a/src/main.c\t2026/11/21
+++ b/src/main.c\t2026/11/22
@@ -1 +1 @@
-old
+new
";
        let diff = parse(text);
        assert_eq!(diff.files[0].path, "src/main.c");
    }

    #[test]
    fn a_file_named_by_rcs_when_the_diff_header_is_missing() {
        let text = "\
RCS file: /repo/docs/main.c,v
retrieving revision 1.1
";
        let diff = parse(text);
        assert_eq!(diff.files[0].path, "main.c");
    }

    #[test]
    fn the_no_newline_marker_does_not_change_the_numbers() {
        let text = "\
--- main.c
+++ main.c
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
        let diff = parse(text);
        let markers: Vec<&Line> = diff.files[0]
            .lines
            .iter()
            .filter(|line| line.text.starts_with('\\'))
            .collect();
        assert_eq!(markers.len(), 2, "both markers are kept");
        assert!(markers.iter().all(|line| line.kind == Kind::Meta));
        assert!(markers
            .iter()
            .all(|line| line.old.is_none() && line.new.is_none()));
    }

    #[test]
    fn a_plain_reply_is_metadata() {
        let diff = parse("No output yet.\nChoose Query > Diff to see it here.\n");
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "");
        assert_eq!(diff.files[0].lines.len(), 2);
        assert!(diff.files[0]
            .lines
            .iter()
            .all(|line| line.kind == Kind::Meta));
    }

    #[test]
    fn the_hunk_lengths_stop_the_body_before_the_next_header() {
        let text = "\
Index: a.c
@@ -1 +1 @@
-old
+new

Index: b.c
@@ -1 +1 @@
-old
+new
";
        let diff = parse(text);
        assert_eq!(diff.files.len(), 2);
        let blank = diff.files[0]
            .lines
            .iter()
            .find(|line| line.text.is_empty())
            .expect("the blank separator is kept");
        assert_eq!(blank.kind, Kind::Meta);
        assert_eq!(blank.text, "");
    }

    #[test]
    fn a_binary_file_note_stays_visible() {
        let text = "\
Index: logo.png
===================================================================
RCS file: /repo/logo.png,v
retrieving revision 1.1
retrieving revision 1.2
Binary files /tmp/cvsAB12 and /tmp/cvsCD34 differ
";
        let diff = parse(text);
        assert_eq!(diff.files[0].path, "logo.png");
        assert!(diff.files[0]
            .lines
            .iter()
            .any(|line| line.text.starts_with("Binary files")));
    }
}
