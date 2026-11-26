//! The ignore list `import` uses: default patterns, `~/.cvsignore`, `CVSIGNORE`, `-I`, and
//! the `.cvsignore` of each folder being imported.
//!
//! The reference keeps one list. A folder's `.cvsignore` is *temporary*: its patterns apply
//! to that folder and below, replace whatever a parent folder added, and disappear again on
//! the way up. A line holding a single `!` (or `*`) clears the whole list back to `CVS`
//! first. Patterns are matched against the file name alone with shell wildcards.

use std::path::Path;

use crate::client::work::codec;

/// The reference's `ign_default`: build output, editor leftovers and CVS's own folders.
const DEFAULTS: &str = ". .. core RCSLOG tags TAGS RCS SCCS .make.state .nse_depinfo #* .#* \
                       cvslog.* ,* CVS CVS.adm .del-* *.a *.olb *.o *.obj *.so *.Z *~ *.old \
                       *.elc *.ln *.bak *.BAK *.orig *.rej *.exe _$* *$";

/// One folder's effective list, plus the base a child `.cvsignore` starts from.
#[derive(Clone, Debug)]
pub struct Ignore {
    base: Vec<String>,
    effective: Vec<String>,
    /// A lone `!` appeared in the base sources, so the server must not apply its own
    /// `CVSROOT/cvsignore` on top of this list.
    inhibit: bool,
}

impl Ignore {
    /// The list for the folder `import` starts in.
    pub fn new(extra: &[String]) -> Ignore {
        let home = home_dir().and_then(|dir| read_ignore_file(&dir));
        let env = std::env::var("CVSIGNORE").ok();
        Self::from_sources(extra, home.as_deref(), env.as_deref())
    }

    /// The same, with the home file and the environment variable named directly, so tests
    /// do not depend on the machine they run on.
    pub(crate) fn from_sources(extra: &[String], home: Option<&str>, env: Option<&str>) -> Ignore {
        let mut base = Vec::new();
        let mut inhibit = false;
        for text in [Some(DEFAULTS), home, env] {
            if let Some(text) = text {
                inhibit |= apply(&mut base, text);
            }
        }
        for pattern in extra {
            inhibit |= apply(&mut base, pattern);
        }
        Ignore {
            effective: base.clone(),
            base,
            inhibit,
        }
    }

    /// The list for a child folder: its own `.cvsignore` when it has one, otherwise the
    /// parent's list.
    pub fn enter(&self, dir: &Path) -> Ignore {
        match read_ignore_file(dir) {
            Some(text) => {
                let mut effective = self.base.clone();
                apply(&mut effective, &text);
                Ignore {
                    effective,
                    base: self.base.clone(),
                    inhibit: self.inhibit,
                }
            }
            None => self.clone(),
        }
    }

    pub fn is_ignored(&self, name: &str) -> bool {
        self.effective.iter().any(|pattern| glob(pattern, name))
    }

    /// True when a `!` in the base sources asked the server to leave its own list alone.
    pub fn inhibits_server(&self) -> bool {
        self.inhibit
    }
}

/// Add the whitespace-separated patterns in `text`. A lone `!` or `*` resets the list to
/// `CVS`; `!` also means the server should not apply its own ignore file. Returns whether a
/// `!` was seen.
fn apply(list: &mut Vec<String>, text: &str) -> bool {
    let mut inhibit = false;
    for token in text.split_whitespace() {
        if token == "!" || token == "*" {
            list.clear();
            list.push("CVS".to_string());
            if token == "!" {
                inhibit = true;
                continue;
            }
        }
        list.push(token.to_string());
    }
    inhibit
}

fn read_ignore_file(dir: &Path) -> Option<String> {
    let bytes = std::fs::read(dir.join(".cvsignore")).ok()?;
    Some(codec::detect(&bytes).decode(&bytes))
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

/// `fnmatch(pattern, name, 0)`: `*`, `?` and `[...]` classes, the whole name, case-sensitive.
fn glob(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches(&pattern, &name)
}

fn matches(pattern: &[char], name: &[char]) -> bool {
    let Some(&first) = pattern.first() else {
        return name.is_empty();
    };
    match first {
        '*' => (0..=name.len()).any(|skip| matches(&pattern[1..], &name[skip..])),
        '?' => !name.is_empty() && matches(&pattern[1..], &name[1..]),
        '[' => {
            let Some(close) = pattern.iter().skip(1).position(|&c| c == ']') else {
                return name.first() == Some(&'[') && matches(&pattern[1..], &name[1..]);
            };
            let class = &pattern[1..=close];
            let Some(&candidate) = name.first() else {
                return false;
            };
            class_matches(class, candidate) && matches(&pattern[close + 2..], &name[1..])
        }
        other => name.first() == Some(&other) && matches(&pattern[1..], &name[1..]),
    }
}

/// `class` is the text between the brackets. A leading `!` or `^` negates it; `a-z` is a
/// range.
fn class_matches(class: &[char], candidate: char) -> bool {
    let (negated, class) = match class.first() {
        Some('!') | Some('^') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut hit = false;
    let mut i = 0;
    while i < class.len() {
        if i + 2 < class.len() && class[i + 1] == '-' {
            if class[i] <= candidate && candidate <= class[i + 2] {
                hit = true;
            }
            i += 3;
        } else {
            if class[i] == candidate {
                hit = true;
            }
            i += 1;
        }
    }
    hit != negated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-ignore-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_default_list_hides_build_output_and_cvs_folders() {
        let ignore = Ignore::from_sources(&[], None, None);
        for name in ["main.o", "lib.a", "notes.BAK", "#temp#", "CVS", "a.exe"] {
            assert!(ignore.is_ignored(name), "{} should be ignored", name);
        }
        for name in ["hello.txt", "main.c", ".cvsignore", "readme"] {
            assert!(!ignore.is_ignored(name), "{} should be kept", name);
        }
    }

    #[test]
    fn the_environment_variable_and_command_line_patterns_are_added() {
        let ignore = Ignore::from_sources(&["*.tmp".to_string()], None, Some("private.txt *.log"));
        for name in ["a.tmp", "private.txt", "run.log"] {
            assert!(ignore.is_ignored(name), "{} should be ignored", name);
        }
    }

    #[test]
    fn a_lone_bang_starts_a_fresh_list() {
        let ignore = Ignore::from_sources(&["!".to_string()], None, None);
        assert!(ignore.is_ignored("CVS"), "CVS is always ignored");
        assert!(!ignore.is_ignored("main.o"), "the defaults were cleared");
    }

    #[test]
    fn a_folders_cvsignore_applies_to_it_and_below() {
        let root = dir("tree");
        std::fs::write(root.join(".cvsignore"), "secret.txt\n").unwrap();
        let child = root.join("child");
        std::fs::create_dir_all(&child).unwrap();

        let top = Ignore::from_sources(&[], None, None).enter(&root);
        assert!(top.is_ignored("secret.txt"));
        assert!(!top.is_ignored("other.txt"));

        let below = top.enter(&child);
        assert!(
            below.is_ignored("secret.txt"),
            "a child without its own file keeps the parent's patterns"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_childs_cvsignore_replaces_the_parents() {
        let root = dir("replace");
        std::fs::write(root.join(".cvsignore"), "secret.txt\n").unwrap();
        let child = root.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(child.join(".cvsignore"), "local.txt\n").unwrap();

        let below = Ignore::from_sources(&[], None, None)
            .enter(&root)
            .enter(&child);
        assert!(below.is_ignored("local.txt"));
        assert!(
            !below.is_ignored("secret.txt"),
            "the parent's temporary patterns are dropped"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_bang_in_a_cvsignore_clears_the_defaults_too() {
        let root = dir("bang");
        std::fs::write(root.join(".cvsignore"), "!\n").unwrap();
        let ignore = Ignore::from_sources(&[], None, None).enter(&root);
        assert!(!ignore.is_ignored("main.o"), "the defaults were cleared");
        assert!(!ignore.is_ignored("keep.txt"));
        assert!(ignore.is_ignored("CVS"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_bang_on_the_command_line_also_inhibits_the_servers_list() {
        let cleared = Ignore::from_sources(&["!".to_string()], None, None);
        assert!(cleared.inhibits_server());
        let ordinary = Ignore::from_sources(&[], None, None);
        assert!(!ordinary.inhibits_server());
    }

    #[test]
    fn patterns_after_a_bang_are_still_ignored() {
        let root = dir("bang-more");
        std::fs::write(root.join(".cvsignore"), "! secret.txt\n").unwrap();
        let ignore = Ignore::from_sources(&[], None, None).enter(&root);
        assert!(ignore.is_ignored("secret.txt"));
        assert!(!ignore.is_ignored("main.o"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn wildcards_match_the_whole_name() {
        assert!(glob("*.BAK", "notes.BAK"));
        assert!(!glob("*.BAK", "notes.bak"), "matching is case-sensitive");
        assert!(!glob("*.BAK", "notes.BAK.keep"));
        assert!(glob("#*", "#temp#"));
        assert!(glob("[abc]*", "beta"));
        assert!(!glob("[!abc]*", "beta"));
        assert!(glob("file?.txt", "file1.txt"));
    }
}
