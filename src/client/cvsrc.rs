//! `~/.cvsrc`: options the user wants every run to start with.
//!
//! One line per command, first word the command (`cvs` for global options), the rest passed
//! to it as if typed before the user's own arguments. The client reads the file twice:
//! once for `cvs` before the global options, once for the command after it has been looked
//! up. `-f` on the command line turns the file off. Comments start with `#`.

/// The parsed file: command name and options, in file order.
#[derive(Clone, Debug, Default)]
pub struct Cvsrc {
    lines: Vec<(String, Vec<String>)>,
}

impl Cvsrc {
    /// Parse the file's text. Blank lines and lines starting with `#` are skipped.
    pub fn parse(text: &str) -> Cvsrc {
        let mut lines = Vec::new();
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut words = line.split_whitespace();
            let Some(command) = words.next() else {
                continue;
            };
            lines.push((
                command.to_string(),
                words.map(str::to_string).collect::<Vec<_>>(),
            ));
        }
        Cvsrc { lines }
    }

    /// The options for `command` from the first matching line, as the reference does.
    pub fn options_for(&self, command: &str) -> Option<&[String]> {
        self.lines
            .iter()
            .find(|(name, _)| name == command)
            .map(|(_, options)| options.as_slice())
    }

    /// Read the file from the home folder, or `None` when there is no home or no file.
    pub fn read() -> Option<Cvsrc> {
        let home = Self::home()?;
        let bytes = std::fs::read(home.join(".cvsrc")).ok()?;
        let text = crate::client::work::codec::detect(&bytes).decode(&bytes);
        Some(Self::parse(&text))
    }

    /// The home folder in the reference's order: `HOME`, then `HOMEDRIVE` + `HOMEPATH`.
    pub fn home() -> Option<std::path::PathBuf> {
        if let Some(home) = std::env::var_os("HOME") {
            if !home.is_empty() {
                return Some(std::path::PathBuf::from(home));
            }
        }
        let drive = std::env::var_os("HOMEDRIVE")?;
        let path = std::env::var_os("HOMEPATH")?;
        let mut home = std::path::PathBuf::from(drive);
        home.push(path);
        Some(home)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let file = Cvsrc::parse("# a comment\n\ncvs -q\n   \ndiff -u\n");
        assert_eq!(file.options_for("cvs"), Some(&["-q".to_string()][..]));
        assert_eq!(file.options_for("diff"), Some(&["-u".to_string()][..]));
        assert_eq!(file.options_for("status"), None);
    }

    #[test]
    fn only_the_first_matching_line_counts() {
        let file = Cvsrc::parse("diff -u\ndiff -c\n");
        assert_eq!(file.options_for("diff"), Some(&["-u".to_string()][..]));
    }

    #[test]
    fn a_command_must_be_a_whole_word() {
        let file = Cvsrc::parse("diff -u\ndifftool -x\n");
        assert_eq!(file.options_for("diff"), Some(&["-u".to_string()][..]));
        assert_eq!(file.options_for("difftool"), Some(&["-x".to_string()][..]));
        assert_eq!(file.options_for("dif"), None);
    }

    #[test]
    fn a_line_without_options_has_an_empty_list() {
        let file = Cvsrc::parse("log\n");
        assert_eq!(file.options_for("log"), Some(&[][..]));
    }
}
