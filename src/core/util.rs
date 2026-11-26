//! The small helpers the panes share.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Shorten text to `max` characters for display, ending in an ellipsis when cut.
///
/// Counts characters rather than bytes, so a Chinese name is shortened by how it looks
/// rather than by how many bytes it takes up. When the result has to line up in a column,
/// use [`truncate_columns`] instead: a Chinese letter takes two cells.
pub fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = chars[..max.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

/// The number of character cells a string takes: one for a Latin letter, two for a Chinese
/// one. This is the unit a console's columns are measured in.
pub fn columns(s: &str) -> usize {
    s.width()
}

/// Shorten text to `max` character cells, ending in an ellipsis when cut.
///
/// Unlike [`truncate`], this counts how much room the text really takes, so a Chinese name
/// is not allowed to run past the column it belongs in.
pub fn truncate_columns(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max - 1; // the ellipsis itself takes one cell
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

/// Fill each row up to `max_width`, breaking at the last place that looks right.
///
/// egui's own wrapping treats a Chinese letter as a word boundary and prefers it over any
/// later punctuation, so `D:\example\projects\示例项目\repos\cvs\src` breaks right after
/// `示例项目` and leaves the rest of the row empty. Here the break is the last separator
/// that fits - a space, a folder separator, a colon, or a Chinese letter - so every row
/// stays full, and a name is only cut when there is nowhere else to cut.
pub fn wrap_path(text: &str, max_width: f32, measure: impl Fn(&str) -> f32) -> String {
    if text.is_empty() || !(max_width > 0.0) || !max_width.is_finite() {
        return text.to_string();
    }
    if measure(text) <= max_width {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut rows: Vec<String> = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let rest: String = chars[start..].iter().collect();
        if measure(&rest) <= max_width {
            rows.push(rest);
            break;
        }

        // The longest prefix of what is left that still fits: halve the room each time.
        let mut low = start + 1;
        let mut high = chars.len() - 1;
        let mut fits = start + 1;
        while low <= high {
            let mid = low + (high - low) / 2;
            let prefix: String = chars[start..mid].iter().collect();
            if measure(&prefix) <= max_width {
                fits = mid;
                low = mid + 1;
            } else {
                high = mid - 1;
            }
        }

        // Back up to the last separator. Without one, the name is cut where it overflows.
        let mut cut = fits;
        while cut > start + 1 && !is_break_after(chars[cut - 1]) {
            cut -= 1;
        }
        if cut == start + 1 && !is_break_after(chars[start]) {
            cut = fits;
        }
        let mut row: String = chars[start..cut].iter().collect();
        while row.ends_with(' ') {
            row.pop();
        }
        if row.is_empty() {
            // Only spaces fitted; keep them rather than draw an empty row.
            cut = fits;
            row = chars[start..cut].iter().collect();
            while row.ends_with(' ') {
                row.pop();
            }
            if row.is_empty() {
                row = chars[start].to_string();
                cut = start + 1;
            }
        }
        rows.push(row);

        start = cut;
        while start < chars.len() && chars[start] == ' ' {
            start += 1;
        }
    }
    rows.join("\n")
}

/// Whether a row may end just after `c`.
///
/// A space, a folder separator and a colon end a name, so a break there keeps every name
/// whole. Anything wide counts too: Chinese, Japanese, Korean and the full-width forms can
/// be broken between two letters, which is how those scripts are set. Punctuation inside a
/// name (`_ - .`) is not a break: `NG_CCS` is one name, not two.
fn is_break_after(c: char) -> bool {
    c.is_whitespace() || matches!(c, '/' | '\\' | ':') || (c as u32) >= 0x2E80
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One character is one unit, so a test can talk about columns.
    fn cells(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn short_text_is_left_alone() {
        assert_eq!(truncate("main.c", 10), "main.c");
    }

    #[test]
    fn long_text_is_cut_and_marked() {
        let got = truncate("abcdefghij", 4);
        assert_eq!(got, "abc…");
        assert_eq!(got.chars().count(), 4);
    }

    #[test]
    fn a_chinese_name_is_cut_by_characters_not_bytes() {
        let got = truncate("知识库文档", 3);
        assert_eq!(got, "知识…");
    }

    #[test]
    fn chinese_letters_are_two_cells_wide() {
        assert_eq!(columns("ab中"), 4);
        assert_eq!(columns("中文"), 4);
        assert_eq!(columns("a-b_c"), 5);
    }

    #[test]
    fn a_chinese_name_is_cut_by_the_room_it_takes() {
        // Three cells hold one Chinese letter and the ellipsis, not two letters.
        assert_eq!(truncate_columns("知识库文档", 3), "知…");
        assert_eq!(truncate_columns("知识库文档", 6), "知识…");
        assert_eq!(truncate_columns("知识库文档", 7), "知识库…");
        assert_eq!(truncate_columns("main.c", 4), "mai…");
        assert_eq!(truncate_columns("main.c", 10), "main.c");
    }

    #[test]
    fn text_that_fits_one_row_is_not_broken() {
        assert_eq!(wrap_path("main.c", 20.0, cells), "main.c");
        assert_eq!(wrap_path("", 20.0, cells), "");
    }

    #[test]
    fn spaces_end_a_row_and_do_not_start_the_next_one() {
        assert_eq!(wrap_path("one two three", 8.0, cells), "one two\nthree");
    }

    #[test]
    fn a_chinese_run_breaks_between_letters() {
        assert_eq!(
            wrap_path("示例项目文件夹目录", 4.0, cells),
            "示例项目\n文件夹目\n录"
        );
    }

    #[test]
    fn a_name_with_nowhere_to_break_is_cut_at_the_row_width() {
        assert_eq!(wrap_path("abcdefghij", 4.0, cells), "abcd\nefgh\nij");
    }

    #[test]
    fn punctuation_inside_a_name_is_not_a_break() {
        // `APP_MAIN` is one name: the row ends at the folder separator before it instead.
        assert_eq!(
            wrap_path(r"D:\aaa\APP_MAIN\src", 13.0, cells),
            "D:\\aaa\\\nAPP_MAIN\\src"
        );
    }

    /// egui would end the row at the last Chinese letter when it treats one as the best
    /// break, which leaves the right of the window empty.
    #[test]
    fn a_path_ends_its_row_at_the_last_separator_that_fits() {
        let path = r"Working folder: D:\example\projects\示例项目\repos\cvs\SourceCode\src";
        let filled = wrap_path(path, 60.0, cells);
        let lines: Vec<&str> = filled.lines().collect();

        assert_eq!(lines.concat(), path, "no character may be lost");
        for line in &lines {
            assert!(cells(line) <= 60.0, "{line} is wider than the row");
        }
        assert!(
            lines[0].ends_with('\\'),
            "the first row should end on a separator, not on a Chinese letter: {:?}",
            lines[0]
        );
        assert!(
            cells(lines[0]) > 50.0,
            "the first row should be filled, not shortened: {:?}",
            lines[0]
        );
    }
}
