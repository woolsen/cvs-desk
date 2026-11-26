//! Applying the context diff a server sends in a `Patched` response.
//!
//! With `Rcs-diff` deliberately not advertised, CVSNT still answers a text update with
//! `Patched` — a GNU **context diff** ("*** old **** / --- new ----"), not a whole file. The
//! client must apply it to the working file before recording the new revision.
//!
//! The format has two sections per hunk: the old lines after `*** range ****`, then the new
//! lines after `--- range ----`. Each line starts with two marker characters:
//! `"  "` unchanged, `"- "`/`"! "` old only, `"+ "`/`"! "` new only.
//!
//! Applying is deliberately strict: if the old lines are not where the hunk says, the update
//! fails rather than writing a wrong file. The server built the diff from the bytes this
//! client sent a moment earlier, so an exact match is expected.

use crate::client::error::{Error, Result};

#[derive(Debug, PartialEq, Eq)]
struct Range {
    start: usize,
    count: usize,
}

#[derive(Debug)]
struct Hunk {
    old_range: Range,
    new_range: Range,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

/// Apply a GNU context diff to `original`. `original` is the repository's line-ending form
/// (LF), not the CRLF working copy.
pub fn apply_context_diff(original: &[u8], patch: &str) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(original);
    let ends_with_newline = text.ends_with('\n');
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();
    if ends_with_newline {
        lines.pop();
    }

    let hunks = parse_hunks(patch)?;
    if hunks.is_empty() {
        // An empty patch means "no change"; some servers send one for a binary file.
        return Ok(original.to_vec());
    }
    let mut offset: isize = 0;
    for hunk in hunks {
        if hunk.old_range.count != 0 && hunk.old_range.count != hunk.old_lines.len() {
            return Err(Error::new(
                "a patch hunk's old range does not match its lines",
            ));
        }
        if hunk.new_range.count != 0 && hunk.new_range.count != hunk.new_lines.len() {
            return Err(Error::new(
                "a patch hunk's new range does not match its lines",
            ));
        }
        let start = hunk.old_range.start.saturating_sub(1) as isize + offset;
        if start < 0 {
            return Err(Error::new("the patch starts before the file"));
        }
        let start = start as usize;
        if lines.len() < start + hunk.old_lines.len() {
            return Err(Error::new("the patch runs past the end of the file"));
        }
        if lines[start..start + hunk.old_lines.len()] != hunk.old_lines[..] {
            return Err(Error::new(
                "the working file no longer matches the patch's old lines",
            ));
        }
        let old_len = hunk.old_lines.len();
        let new_len = hunk.new_lines.len();
        lines.splice(start..start + old_len, hunk.new_lines);
        offset += new_len as isize - old_len as isize;
    }

    let mut out = lines.join("\n");
    if ends_with_newline {
        out.push('\n');
    }
    Ok(out.into_bytes())
}

fn parse_hunks(patch: &str) -> Result<Vec<Hunk>> {
    let mut lines: Vec<&str> = patch
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    // A patch body ends with the protocol's content boundary, often a final newline.
    while lines.last() == Some(&"") {
        lines.pop();
    }
    let mut hunks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].starts_with("***************") {
            index += 1;
            continue;
        }
        index += 1;
        let old_header = lines
            .get(index)
            .ok_or_else(|| Error::new("a patch hunk has no old header"))?;
        if !old_header.starts_with("*** ") {
            return Err(Error::new("a patch hunk has no old range"));
        }
        let old_range = parse_range(old_header, "*** ", "****")?;
        index += 1;
        let mut old_lines = Vec::new();
        while index < lines.len() && !lines[index].starts_with("--- ") {
            old_lines.push(marker_line(lines[index], "old")?);
            index += 1;
        }
        let new_header = lines
            .get(index)
            .ok_or_else(|| Error::new("a patch hunk has no new header"))?;
        if !new_header.starts_with("--- ") {
            return Err(Error::new("a patch hunk has no new range"));
        }
        let new_range = parse_range(new_header, "--- ", "----")?;
        index += 1;
        let mut new_lines = Vec::new();
        while index < lines.len() && !lines[index].starts_with("***************") {
            new_lines.push(marker_line(lines[index], "new")?);
            index += 1;
        }
        hunks.push(Hunk {
            old_range,
            new_range,
            old_lines,
            new_lines,
        });
    }
    Ok(hunks)
}

/// Strip a context-diff marker and return the line's text.
fn marker_line(line: &str, side: &str) -> Result<String> {
    if line == "\\ No newline at end of file" {
        return Ok(String::new());
    }
    let bytes = line.as_bytes();
    if bytes.len() < 2 {
        return Ok(line.to_string());
    }
    match &line[..2] {
        "  " => Ok(line[2..].to_string()),
        "- " | "! " if side == "old" => Ok(line[2..].to_string()),
        "+ " | "! " if side == "new" => Ok(line[2..].to_string()),
        _ => Err(Error::new(format!(
            "a context diff line has an unexpected marker: {:?}",
            line
        ))),
    }
}

fn parse_range(header: &str, prefix: &str, suffix: &str) -> Result<Range> {
    let inner = header
        .strip_prefix(prefix)
        .and_then(|text| text.strip_suffix(suffix))
        .ok_or_else(|| Error::new(format!("bad context diff range: {:?}", header)))?
        .trim();
    let mut parts = inner.split(',');
    let start: usize = parts
        .next()
        .unwrap_or("0")
        .trim()
        .parse()
        .map_err(|_| Error::new(format!("bad context diff range: {:?}", header)))?;
    let end: usize = match parts.next() {
        Some(value) => value
            .trim()
            .parse()
            .map_err(|_| Error::new(format!("bad context diff range: {:?}", header)))?,
        None => start,
    };
    let count = if start == 0 {
        0
    } else {
        end.saturating_sub(start) + 1
    };
    Ok(Range { start, count })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_one_line_change_applies() {
        let patch = "*** CVS/,,two.txt-1\tTue Nov 24 18:56:23 2026\n\
                     --- CVS/,,two.txt-2\tTue Nov 24 18:56:23 2026\n\
                     ***************\n\
                     *** 1 ****\n\
                     ! two-server\n\
                     --- 1 ----\n\
                     ! two-13\n";
        let out = apply_context_diff(b"two-server\n", patch).unwrap();
        assert_eq!(out, b"two-13\n");
    }

    #[test]
    fn context_lines_keep_their_place() {
        let patch = "***************\n*** 1,3 ****\n  one\n! two\n  three\n--- 1,3 ----\n  one\n! TWO\n  three\n";
        let out = apply_context_diff(b"one\ntwo\nthree\n", patch).unwrap();
        assert_eq!(out, b"one\nTWO\nthree\n");
    }

    #[test]
    fn a_patch_that_does_not_match_is_refused() {
        let patch = "***************\n*** 1 ****\n! expected\n--- 1 ----\n! changed\n";
        assert!(apply_context_diff(b"different\n", patch).is_err());
    }

    #[test]
    fn an_empty_patch_leaves_the_file_alone() {
        assert_eq!(apply_context_diff(b"same\n", "").unwrap(), b"same\n");
    }
}
