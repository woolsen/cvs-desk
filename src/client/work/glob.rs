//! Wildcard expansion for command-line file names.
//!
//! On Windows the shell does not expand `*.c`; the reference client does it itself in
//! `windows-NT/filesubr.c` (`expand_wild`). Every command that takes file names goes through
//! [`expand_args`], so `cvs add *.txt` names the files the user meant.

use std::path::{Path, PathBuf};

/// Replace each argument that contains a wildcard with the names it matches in its folder.
/// A pattern that matches nothing is left as typed, so the server can report it the way the
/// reference does.
pub fn expand_args(cwd: &Path, names: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        out.extend(expand_one(cwd, name));
    }
    out
}

fn expand_one(cwd: &Path, name: &str) -> Vec<String> {
    let (prefix, pattern) = split_dir(name);
    if !has_wildcard(pattern) {
        return vec![name.to_string()];
    }
    let dir = cwd.join(&prefix);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![name.to_string()];
    };
    let mut matches: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|file| wildcard_match(pattern, file))
        .collect();
    if matches.is_empty() {
        return vec![name.to_string()];
    }
    // Command output and the protocol arguments stay stable from run to run.
    matches.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
    matches
        .into_iter()
        .map(|file| {
            if prefix.is_empty() {
                file
            } else {
                format!(
                    "{}{}{}",
                    prefix,
                    if prefix.ends_with(['/', '\\']) {
                        ""
                    } else {
                        "/"
                    },
                    file
                )
            }
        })
        .collect()
}

/// Split `sub/dir/*.c` into `sub/dir/` and `*.c`. A name with no folder is all pattern.
fn split_dir(name: &str) -> (String, &str) {
    match name.rfind(['/', '\\']) {
        Some(index) => (name[..=index].to_string(), &name[index + 1..]),
        None => (String::new(), name),
    }
}

fn has_wildcard(text: &str) -> bool {
    text.contains(['*', '?', '['])
}

/// `*`, `?` and `[...]` classes, the subset cmd leaves to the program. Windows file names
/// compare without case.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().flat_map(|c| c.to_lowercase()).collect();
    let name: Vec<char> = name.chars().flat_map(|c| c.to_lowercase()).collect();
    matches_from(&pattern, 0, &name, 0)
}

fn matches_from(pattern: &[char], mut p: usize, name: &[char], mut n: usize) -> bool {
    while p < pattern.len() {
        match pattern[p] {
            '*' => {
                // Collapse runs of `*`; try every split point.
                while p + 1 < pattern.len() && pattern[p + 1] == '*' {
                    p += 1;
                }
                if p + 1 == pattern.len() {
                    return true;
                }
                for skip in n..=name.len() {
                    if matches_from(pattern, p + 1, name, skip) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if n >= name.len() {
                    return false;
                }
                p += 1;
                n += 1;
            }
            '[' => {
                if n >= name.len() {
                    return false;
                }
                let Some((matched, next)) = match_class(pattern, p, name[n]) else {
                    // An unterminated `[` is a literal.
                    if name[n] != '[' {
                        return false;
                    }
                    p += 1;
                    n += 1;
                    continue;
                };
                if !matched {
                    return false;
                }
                p = next;
                n += 1;
            }
            literal => {
                if n >= name.len() || name[n] != literal {
                    return false;
                }
                p += 1;
                n += 1;
            }
        }
    }
    n == name.len()
}

/// Match `[...]` at `start`, returning whether the character matched and where the class ends.
fn match_class(pattern: &[char], start: usize, ch: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negated = matches!(pattern.get(i), Some('!') | Some('^'));
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pattern.len() {
        if pattern[i] == ']' && !first {
            return Some((matched != negated, i + 1));
        }
        first = false;
        if i + 2 < pattern.len() && pattern[i + 1] == '-' && pattern[i + 2] != ']' {
            if pattern[i] <= ch && ch <= pattern[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if pattern[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

/// The folder a pattern is rooted in, for callers that need to know.
pub fn pattern_dir(cwd: &Path, name: &str) -> PathBuf {
    let (prefix, _) = split_dir(name);
    cwd.join(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_and_questions_match_a_name() {
        assert!(wildcard_match("*.txt", "notes.txt"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "abbc"));
    }

    #[test]
    fn classes_and_negation() {
        assert!(wildcard_match("[abc].c", "b.c"));
        assert!(wildcard_match("[a-c].c", "b.c"));
        assert!(!wildcard_match("[!a-c].c", "b.c"));
        assert!(wildcard_match("[!a-c].c", "d.c"));
        assert!(
            wildcard_match("[abc", "[abc"),
            "an unterminated class is a literal"
        );
    }

    #[test]
    fn matching_ignores_case_like_windows() {
        assert!(wildcard_match("*.TXT", "Notes.txt"));
    }

    #[test]
    fn a_name_without_wildcards_is_kept() {
        let cwd = std::env::temp_dir();
        assert_eq!(expand_args(&cwd, &["one.c".to_string()]), ["one.c"]);
    }
}
