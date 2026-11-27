//! The computer side of the side-by-side diff window.
//!
//! This module takes two complete files, not a `cvs diff` reply: the window fetches
//! each revision in full, so the comparison can pair every line and the view can
//! highlight the whole file before slicing it into rows. [`similar`] does the line
//! diff; this module turns its operations into the rows the view draws.
//!
//! The line model is deliberately exact. A line keeps its own terminator flag, a
//! file keeps its final-newline flag, and the text is split by `similar` rather than
//! by [`str::lines`], which would drop a trailing newline and every trailing blank
//! line. The view can therefore show "no newline at end of file" without guessing.

use std::collections::VecDeque;

use similar::{Algorithm, DiffTag, TextDiff};

/// How the raw bytes ended their lines. The displayed text is always normalized to
/// `\n`; these counts are kept so the window can say that two otherwise equal files
/// differ only in their line endings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineEndings {
    pub crlf: usize,
    pub lf: usize,
    pub cr: usize,
}

impl LineEndings {
    pub fn is_empty(self) -> bool {
        self.crlf == 0 && self.lf == 0 && self.cr == 0
    }
}

/// What the loader learned about the bytes before they became text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileMeta {
    pub encoding: String,
    pub has_bom: bool,
    pub line_endings: LineEndings,
    /// The last byte of the file was a line terminator.
    pub final_newline: bool,
    pub binary: bool,
    pub too_large: bool,
    /// Decoding had to replace bytes; the text below is not exact.
    pub lossy: bool,
    /// The size of the original bytes.
    pub bytes: usize,
}

/// One complete side of the comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileText {
    /// The text drawn above the side, e.g. "main.c @ 1.4" or "local".
    pub title: String,
    /// The complete text, normalized to `\n` line endings.
    pub text: String,
    pub meta: FileMeta,
    /// A syntect token or extension, e.g. `"c"` or `"rs"`.
    pub language: String,
}

impl FileText {
    /// Build a side from decoded text. The loader uses this after it has decoded the
    /// bytes; tests use it directly.
    pub fn from_text(
        title: impl Into<String>,
        text: impl Into<String>,
        language: impl Into<String>,
    ) -> Self {
        let raw = text.into();
        let meta = FileMeta {
            encoding: "UTF-8".to_string(),
            has_bom: raw.starts_with('\u{feff}'),
            line_endings: count_line_endings(&raw),
            final_newline: raw.ends_with('\n'),
            binary: raw.contains('\0'),
            too_large: false,
            lossy: false,
            bytes: raw.len(),
        };
        let text = normalize_line_endings(strip_bom(&raw));
        Self {
            title: title.into(),
            text,
            meta,
            language: language.into(),
        }
    }
}

/// The options the window passes to [`DiffModel::build`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffOptions {
    pub algorithm: Algorithm,
    /// How many unchanged rows may sit between two changes before they become two
    /// hunks for the previous/next buttons. The rows themselves are always kept in
    /// full: this only groups jumps, it never hides a line.
    pub context: usize,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            algorithm: Algorithm::Patience,
            context: 3,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub added: usize,
    pub removed: usize,
}

/// A metadata difference the content diff itself cannot show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Binary,
    TooLarge,
    EncodingDiffers,
    BomDiffers,
    LineEndingsDiffer,
    FinalNewlineDiffers,
    DecodeLossy,
}

impl Notice {
    pub fn message(self) -> &'static str {
        match self {
            Self::Binary => "Binary files cannot be shown as text.",
            Self::TooLarge => "This file is too large to compare in the window.",
            Self::EncodingDiffers => "The two versions use different text encodings.",
            Self::BomDiffers => "One version starts with a byte-order mark and the other does not.",
            Self::LineEndingsDiffer => "The content is the same, but the line endings differ.",
            Self::FinalNewlineDiffers => "One version has no newline at the end of the file.",
            Self::DecodeLossy => "Some bytes could not be decoded; the text may be incomplete.",
        }
    }
}

/// One numbered line. `text` does not include its terminator; `terminated` is false
/// only for a final line that has no newline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub no: usize,
    pub text: String,
    pub terminated: bool,
}

/// One row of the two-column view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// Both sides show a line. `Replace` is used when the lines differ.
    Both {
        left: Line,
        right: Line,
        kind: RowKind,
    },
    /// A line that exists only in the older file.
    Left(Line),
    /// A line that exists only in the newer file.
    Right(Line),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Equal,
    Replace,
}

impl Row {
    pub fn is_change(&self) -> bool {
        match self {
            Self::Both { kind, .. } => *kind == RowKind::Replace,
            Self::Left(_) | Self::Right(_) => true,
        }
    }
}

/// A run of changed rows, used by the previous/next buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub first_row: usize,
    pub last_row: usize,
}

/// The byte ranges that changed inside a replaced row. The ranges point into the
/// row's own line text, so the view can paint them without splitting the line again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordDiff {
    pub left: Vec<std::ops::Range<usize>>,
    pub right: Vec<std::ops::Range<usize>>,
}

/// Two complete files, paired line by line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffModel {
    pub left: FileText,
    pub right: FileText,
    pub rows: Vec<Row>,
    /// One entry per row. Only a replaced row has word ranges.
    pub words: Vec<Option<WordDiff>>,
    pub hunks: Vec<Hunk>,
    pub stats: Stats,
    pub notices: Vec<Notice>,
}

impl DiffModel {
    pub fn build(left: FileText, right: FileText, options: DiffOptions) -> Self {
        let mut left = left;
        let mut right = right;
        let mut notices = Vec::new();
        if left.meta.binary || right.meta.binary {
            notices.push(Notice::Binary);
            return Self::empty(left, right, notices);
        }
        if left.meta.too_large || right.meta.too_large {
            notices.push(Notice::TooLarge);
            return Self::empty(left, right, notices);
        }
        if left.meta.encoding != right.meta.encoding {
            notices.push(Notice::EncodingDiffers);
        }
        if left.meta.has_bom != right.meta.has_bom {
            notices.push(Notice::BomDiffers);
        }
        if left.meta.lossy || right.meta.lossy {
            notices.push(Notice::DecodeLossy);
        }
        if left.meta.final_newline != right.meta.final_newline {
            notices.push(Notice::FinalNewlineDiffers);
        }
        let left_endings = left.meta.line_endings;
        let right_endings = right.meta.line_endings;

        left.text = normalize_line_endings(&left.text);
        right.text = normalize_line_endings(&right.text);

        // A CRLF/LF-only change would otherwise turn every line into a replacement.
        // The content comparison ignores the terminators; if the normalized texts
        // are equal, the metadata notice tells the reader what really differed.
        if left.text == right.text && left_endings != right_endings {
            notices.push(Notice::LineEndingsDiffer);
        }

        // No deadline: the worker thread, not the window, runs this comparison. A
        // timeout here would silently turn a hard file into an approximate diff.
        let diff = TextDiff::configure()
            .algorithm(options.algorithm)
            .diff_lines(&left.text, &right.text);

        let mut rows = Vec::new();
        let mut stats = Stats::default();
        for op in diff.ops() {
            let old = op.old_range();
            let new = op.new_range();
            match op.tag() {
                DiffTag::Equal => {
                    for offset in 0..old.len() {
                        let left_line = line_at(&diff, true, old.start + offset);
                        let right_line = line_at(&diff, false, new.start + offset);
                        rows.push(Row::Both {
                            left: left_line,
                            right: right_line,
                            kind: RowKind::Equal,
                        });
                    }
                }
                DiffTag::Delete => {
                    stats.removed += old.len();
                    for index in old {
                        rows.push(Row::Left(line_at(&diff, true, index)));
                    }
                }
                DiffTag::Insert => {
                    stats.added += new.len();
                    for index in new {
                        rows.push(Row::Right(line_at(&diff, false, index)));
                    }
                }
                DiffTag::Replace => {
                    stats.removed += old.len();
                    stats.added += new.len();
                    let count = old.len().max(new.len());
                    for offset in 0..count {
                        let left_line =
                            (offset < old.len()).then(|| line_at(&diff, true, old.start + offset));
                        let right_line =
                            (offset < new.len()).then(|| line_at(&diff, false, new.start + offset));
                        match (left_line, right_line) {
                            (Some(left_line), Some(right_line)) => rows.push(Row::Both {
                                left: left_line,
                                right: right_line,
                                kind: RowKind::Replace,
                            }),
                            (Some(left_line), None) => rows.push(Row::Left(left_line)),
                            (None, Some(right_line)) => rows.push(Row::Right(right_line)),
                            (None, None) => {}
                        }
                    }
                }
            }
        }

        // `similar` normally compacts an adjacent delete+insert into a replacement,
        // but not every algorithm/version does. Pair the two runs here so the two
        // columns stay aligned even when the operations arrive separately.
        let rows = pair_replacements(rows);
        let words = word_diffs(&rows);
        let hunks = group_hunks(&rows, options.context);
        Self {
            left,
            right,
            rows,
            words,
            hunks,
            stats,
            notices,
        }
    }

    fn empty(left: FileText, right: FileText, notices: Vec<Notice>) -> Self {
        Self {
            left,
            right,
            rows: Vec::new(),
            words: Vec::new(),
            hunks: Vec::new(),
            stats: Stats::default(),
            notices,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

fn line_at(diff: &TextDiff<'_, '_, str>, old: bool, index: usize) -> Line {
    let raw = if old {
        diff.old_slice(index)
    } else {
        diff.new_slice(index)
    }
    .expect("a diff operation always names lines inside its own file");
    let terminated = raw.ends_with('\n');
    let text = raw.strip_suffix('\n').unwrap_or(raw);
    let text = text.strip_suffix('\r').unwrap_or(text);
    Line {
        no: index + 1,
        text: text.to_string(),
        terminated,
    }
}

/// Compute the changed character ranges for each replaced row. Inserted and
/// deleted rows are already one block of change, so only paired lines need this.
fn word_diffs(rows: &[Row]) -> Vec<Option<WordDiff>> {
    rows.iter()
        .map(|row| match row {
            Row::Both {
                left,
                right,
                kind: RowKind::Replace,
            } => Some(word_diff(&left.text, &right.text)),
            _ => None,
        })
        .collect()
}

fn word_diff(left: &str, right: &str) -> WordDiff {
    let diff = TextDiff::from_chars(left, right);
    let mut left_ranges = Vec::new();
    let mut right_ranges = Vec::new();
    for op in diff.ops() {
        if op.tag() == DiffTag::Equal {
            continue;
        }
        let old = op.old_range();
        let new = op.new_range();
        if !old.is_empty() {
            left_ranges.push(char_range_to_bytes(left, old));
        }
        if !new.is_empty() {
            right_ranges.push(char_range_to_bytes(right, new));
        }
    }
    WordDiff {
        left: merge_ranges(left_ranges),
        right: merge_ranges(right_ranges),
    }
}

fn char_range_to_bytes(text: &str, range: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let start = text
        .char_indices()
        .nth(range.start)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len());
    let end = text
        .char_indices()
        .nth(range.end)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len());
    start..end
}

fn merge_ranges(ranges: Vec<std::ops::Range<usize>>) -> Vec<std::ops::Range<usize>> {
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if range.start >= range.end {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

/// Turn adjacent delete and insert runs into paired replacement rows.
fn pair_replacements(rows: Vec<Row>) -> Vec<Row> {
    let mut out = Vec::with_capacity(rows.len());
    let mut rows = rows.into_iter().peekable();
    while let Some(row) = rows.next() {
        match row {
            Row::Left(left) => {
                let mut lefts = vec![left];
                while let Some(Row::Left(_)) = rows.peek() {
                    if let Some(Row::Left(next)) = rows.next() {
                        lefts.push(next);
                    }
                }
                let mut rights = Vec::new();
                while let Some(Row::Right(_)) = rows.peek() {
                    if let Some(Row::Right(next)) = rows.next() {
                        rights.push(next);
                    }
                }
                emit_replacement(lefts, rights, &mut out);
            }
            Row::Right(right) => {
                let mut rights = vec![right];
                while let Some(Row::Right(_)) = rows.peek() {
                    if let Some(Row::Right(next)) = rows.next() {
                        rights.push(next);
                    }
                }
                let mut lefts = Vec::new();
                while let Some(Row::Left(_)) = rows.peek() {
                    if let Some(Row::Left(next)) = rows.next() {
                        lefts.push(next);
                    }
                }
                emit_replacement(lefts, rights, &mut out);
            }
            other => out.push(other),
        }
    }
    out
}

fn emit_replacement(lefts: Vec<Line>, rights: Vec<Line>, out: &mut Vec<Row>) {
    let mut lefts = VecDeque::from(lefts);
    let mut rights = VecDeque::from(rights);
    while !lefts.is_empty() || !rights.is_empty() {
        match (lefts.pop_front(), rights.pop_front()) {
            (Some(left), Some(right)) => out.push(Row::Both {
                left,
                right,
                kind: RowKind::Replace,
            }),
            (Some(left), None) => out.push(Row::Left(left)),
            (None, Some(right)) => out.push(Row::Right(right)),
            (None, None) => {}
        }
    }
}

fn group_hunks(rows: &[Row], context: usize) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut current: Option<Hunk> = None;
    let mut unchanged_after_change = 0usize;
    for (index, row) in rows.iter().enumerate() {
        if row.is_change() {
            match &mut current {
                Some(hunk) => hunk.last_row = index,
                None => {
                    current = Some(Hunk {
                        first_row: index,
                        last_row: index,
                    })
                }
            }
            unchanged_after_change = 0;
        } else if current.is_some() {
            unchanged_after_change += 1;
            if unchanged_after_change > context {
                if let Some(hunk) = current.take() {
                    hunks.push(hunk);
                }
                unchanged_after_change = 0;
            }
        }
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    hunks
}

/// Count the terminators in raw text. A `\r\n` pair is one CRLF, a lone `\r` is CR,
/// and a lone `\n` is LF.
pub fn count_line_endings(text: &str) -> LineEndings {
    let bytes = text.as_bytes();
    let mut endings = LineEndings::default();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                endings.crlf += 1;
                index += 2;
            }
            b'\r' => {
                endings.cr += 1;
                index += 1;
            }
            b'\n' => {
                endings.lf += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    endings
}

/// Normalize every terminator to `\n` without touching the number of terminators.
/// This preserves a final newline and every trailing blank line.
pub fn normalize_line_endings(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                out.push('\n');
                index += 2;
            }
            b'\r' => {
                out.push('\n');
                index += 1;
            }
            _ => {
                let start = index;
                while index < bytes.len() && bytes[index] != b'\r' {
                    index += 1;
                }
                out.push_str(&text[start..index]);
            }
        }
    }
    out
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(title: &str, body: &str) -> FileText {
        FileText::from_text(title, body, "c")
    }

    fn build(left: &str, right: &str) -> DiffModel {
        DiffModel::build(
            text("old", left),
            text("new", right),
            DiffOptions::default(),
        )
    }

    #[test]
    fn identical_files_have_no_changes() {
        let model = build("a\nb\n", "a\nb\n");
        assert_eq!(model.rows.len(), 2);
        assert_eq!(model.stats, Stats::default());
        assert!(model.hunks.is_empty());
        assert!(model.notices.is_empty());
        assert!(matches!(
            model.rows[0],
            Row::Both {
                kind: RowKind::Equal,
                ..
            }
        ));
    }

    #[test]
    fn an_added_line_has_only_a_right_number() {
        let model = build("a\nb\n", "a\nb\nc\n");
        assert_eq!(
            model.stats,
            Stats {
                added: 1,
                removed: 0
            }
        );
        assert_eq!(model.rows.len(), 3);
        match &model.rows[2] {
            Row::Right(line) => {
                assert_eq!(line.no, 3);
                assert_eq!(line.text, "c");
                assert!(line.terminated);
            }
            other => panic!("expected an inserted line, got {other:?}"),
        }
    }

    #[test]
    fn a_removed_line_has_only_a_left_number() {
        let model = build("a\nb\n", "a\n");
        assert_eq!(
            model.stats,
            Stats {
                added: 0,
                removed: 1
            }
        );
        match &model.rows[1] {
            Row::Left(line) => {
                assert_eq!(line.no, 2);
                assert_eq!(line.text, "b");
            }
            other => panic!("expected a deleted line, got {other:?}"),
        }
    }

    #[test]
    fn a_replacement_pairs_lines_and_fills_the_short_side() {
        let model = build("a\nb\n", "a\nB\nC\n");
        assert_eq!(
            model.stats,
            Stats {
                added: 2,
                removed: 1
            }
        );
        assert_eq!(model.rows.len(), 3);
        match &model.rows[1] {
            Row::Both {
                left,
                right,
                kind: RowKind::Replace,
            } => {
                assert_eq!((left.no, left.text.as_str()), (2, "b"));
                assert_eq!((right.no, right.text.as_str()), (2, "B"));
            }
            other => panic!("expected a replacement, got {other:?}"),
        }
        match &model.rows[2] {
            Row::Right(line) => assert_eq!((line.no, line.text.as_str()), (3, "C")),
            other => panic!("expected the extra inserted line, got {other:?}"),
        }
    }

    #[test]
    fn a_replaced_line_marks_only_the_changed_words() {
        let model = build(
            "int main(void) { return 0; }\n",
            "int main(void) { return 1; }\n",
        );
        let words = model.words[0]
            .as_ref()
            .expect("a replaced row has word ranges");
        let (left, right) = match &model.rows[0] {
            Row::Both { left, right, .. } => (left, right),
            other => panic!("expected a replacement, got {other:?}"),
        };
        assert_eq!(words.left.len(), 1);
        assert_eq!(words.right.len(), 1);
        assert_eq!(&left.text[words.left[0].clone()], "0");
        assert_eq!(&right.text[words.right[0].clone()], "1");
    }

    #[test]
    fn word_ranges_keep_unicode_on_character_boundaries() {
        let model = build("a中b\n", "a中文b\n");
        let words = model.words[0]
            .as_ref()
            .expect("a replaced row has word ranges");
        let (left, right) = match &model.rows[0] {
            Row::Both { left, right, .. } => (left, right),
            other => panic!("expected a replacement, got {other:?}"),
        };
        assert!(words.left.is_empty());
        assert_eq!(&right.text[words.right[0].clone()], "文");
        assert!(right.text.get(words.right[0].clone()).is_some());
        assert!(left
            .text
            .get(words.left.first().cloned().unwrap_or(0..0))
            .is_some());
    }

    #[test]
    fn an_inserted_line_has_no_word_ranges() {
        let model = build("a\n", "a\nb\n");
        assert_eq!(model.words.len(), model.rows.len());
        assert!(model.words[1].is_none());
    }

    #[test]
    fn a_whitespace_only_change_still_has_a_word_range() {
        let model = build("a b\n", "a  b\n");
        let words = model.words[0]
            .as_ref()
            .expect("a replaced row has word ranges");
        let (left, right) = match &model.rows[0] {
            Row::Both { left, right, .. } => (left, right),
            other => panic!("expected a replacement, got {other:?}"),
        };
        assert!(words.left.is_empty());
        assert_eq!(&right.text[words.right[0].clone()], " ");
    }

    #[test]
    fn a_missing_final_newline_is_visible() {
        let model = build("a\nb", "a\nb\n");
        assert_eq!(model.rows.len(), 2);
        let last = &model.rows[1];
        match last {
            Row::Both { left, right, .. } => {
                assert!(!left.terminated, "the left file has no final newline");
                assert!(right.terminated, "the right file has one");
            }
            other => panic!("expected the final lines, got {other:?}"),
        }
        assert!(model.notices.contains(&Notice::FinalNewlineDiffers));
    }

    #[test]
    fn trailing_blank_lines_are_rows_not_trimmed() {
        let model = build("a\n", "a\n\n");
        assert_eq!(model.rows.len(), 2);
        match &model.rows[1] {
            Row::Right(line) => {
                assert_eq!(line.text, "");
                assert!(line.terminated);
                assert_eq!(line.no, 2);
            }
            other => panic!("expected the extra blank line, got {other:?}"),
        }
        assert_eq!(
            model.stats,
            Stats {
                added: 1,
                removed: 0
            }
        );
    }

    #[test]
    fn an_empty_file_differs_from_one_blank_line() {
        let model = build("", "\n");
        assert_eq!(model.rows.len(), 1);
        match &model.rows[0] {
            Row::Right(line) => {
                assert_eq!(line.text, "");
                assert!(line.terminated);
            }
            other => panic!("expected one added blank line, got {other:?}"),
        }
    }

    #[test]
    fn crlf_is_normalized_for_content_but_reported() {
        let model = build("a\r\nb\r\n", "a\nb\n");
        assert_eq!(model.rows.len(), 2);
        assert_eq!(model.stats, Stats::default());
        assert!(model.notices.contains(&Notice::LineEndingsDiffer));
        assert!(!model.left.text.contains('\r'));
        assert!(!model.right.text.contains('\r'));
    }

    #[test]
    fn a_real_crlf_change_still_shows_the_content() {
        let model = build("a\r\nb\r\n", "a\nB\n");
        assert_eq!(
            model.stats,
            Stats {
                added: 1,
                removed: 1
            }
        );
        match &model.rows[1] {
            Row::Both {
                left,
                right,
                kind: RowKind::Replace,
            } => {
                assert_eq!(left.text, "b");
                assert_eq!(right.text, "B");
            }
            other => panic!("expected a replacement, got {other:?}"),
        }
    }

    #[test]
    fn trailing_spaces_are_compared() {
        let model = build("a \n", "a\n");
        assert_eq!(
            model.stats,
            Stats {
                added: 1,
                removed: 1
            }
        );
    }

    #[test]
    fn empty_files_are_empty_models() {
        let model = build("", "");
        assert!(model.rows.is_empty());
        assert!(model.hunks.is_empty());
        assert_eq!(model.stats, Stats::default());
    }

    #[test]
    fn punctuation_and_cr_only_files_are_normalized() {
        assert_eq!(normalize_line_endings("a\rb\r"), "a\nb\n");
        assert_eq!(
            count_line_endings("a\r\nb\rc\n"),
            LineEndings {
                crlf: 1,
                lf: 1,
                cr: 1
            }
        );
    }

    #[test]
    fn changes_far_apart_become_separate_hunks() {
        let old = "0\n1\n2\n3\n4\n5\n6\n7\n8\n";
        let new = "0\nX\n2\n3\n4\n5\n6\n7\nY\n";
        let model = build(old, new);
        assert_eq!(model.hunks.len(), 2);
        assert_eq!(model.hunks[0].first_row, 1);
        assert_eq!(model.hunks[1].first_row, 8);
        assert!(model.hunks[1].first_row > model.hunks[0].last_row);
    }

    #[test]
    fn adjacent_delete_and_insert_runs_are_paired() {
        let line = |no, text: &str| Line {
            no,
            text: text.to_string(),
            terminated: true,
        };
        let rows = pair_replacements(vec![
            Row::Left(line(1, "a")),
            Row::Left(line(2, "b")),
            Row::Right(line(1, "A")),
        ]);
        assert_eq!(rows.len(), 2);
        match &rows[0] {
            Row::Both {
                left,
                right,
                kind: RowKind::Replace,
            } => {
                assert_eq!((left.no, left.text.as_str()), (1, "a"));
                assert_eq!((right.no, right.text.as_str()), (1, "A"));
            }
            other => panic!("expected a paired replacement, got {other:?}"),
        }
        assert!(matches!(&rows[1], Row::Left(line) if line.text == "b"));
    }

    #[test]
    fn an_equal_row_keeps_delete_and_insert_runs_apart() {
        let line = |no, text: &str| Line {
            no,
            text: text.to_string(),
            terminated: true,
        };
        let rows = pair_replacements(vec![
            Row::Left(line(1, "a")),
            Row::Both {
                left: line(2, "same"),
                right: line(2, "same"),
                kind: RowKind::Equal,
            },
            Row::Right(line(2, "A")),
        ]);
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], Row::Left(_)));
        assert!(matches!(rows[1], Row::Both { .. }));
        assert!(matches!(rows[2], Row::Right(_)));
    }

    #[test]
    fn encoding_and_bom_differences_are_notices() {
        let mut left = text("old", "a\n");
        let mut right = text("new", "a\n");
        left.meta.encoding = "GBK".into();
        right.meta.encoding = "UTF-8".into();
        right.meta.has_bom = true;
        let model = DiffModel::build(left, right, DiffOptions::default());
        assert!(model.notices.contains(&Notice::EncodingDiffers));
        assert!(model.notices.contains(&Notice::BomDiffers));
    }

    #[test]
    fn a_binary_side_has_a_notice_and_no_rows() {
        let mut left = text("old", "a\n");
        left.meta.binary = true;
        let model = DiffModel::build(left, text("new", "a\n"), DiffOptions::default());
        assert!(model.rows.is_empty());
        assert!(model.notices.contains(&Notice::Binary));
    }

    #[test]
    fn an_over_large_side_has_a_notice_and_no_rows() {
        let mut left = text("old", "a\n");
        left.meta.too_large = true;
        let model = DiffModel::build(left, text("new", "a\n"), DiffOptions::default());
        assert!(model.rows.is_empty());
        assert!(model.notices.contains(&Notice::TooLarge));
    }
}
