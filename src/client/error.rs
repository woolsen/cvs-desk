//! Errors, worded the way CVS words them.
//!
//! CVS uses two shapes:
//!
//! * `cvs [<command> aborted]: <what went wrong>` — the command stops, exit code 1;
//! * `cvs <command>: <what happened>` — an ordinary complaint, the command carries on.
//!
//! The difference matters to callers: the desktop program treats a non-zero exit as a failed
//! command, and its console shows the first line as the reason.

use std::fmt;

/// A finished error message. The text is complete on purpose: the command name is already in
/// it, so nothing downstream has to guess which command it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    text: String,
}

impl Error {
    /// A message with no CVS prefix, for errors raised before a command is known.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// `cvs [<command> aborted]: <text>`.
    pub fn aborted(command: &str, text: impl Into<String>) -> Self {
        Self::new(format!("cvs [{} aborted]: {}", command, text.into()))
    }

    /// `cvs <command>: <text>`.
    pub fn plain(command: &str, text: impl Into<String>) -> Self {
        Self::new(format!("cvs {}: {}", command, text.into()))
    }

    /// `cvs [<command> aborted]: stopped by the user` — a stop the user asked for, not a
    /// failure of the command.
    pub fn interrupted(command: &str) -> Self {
        Self::aborted(command, "stopped by the user")
    }

    /// True when this error is the one [`Error::interrupted`] builds.
    pub fn is_interrupted(&self) -> bool {
        self.text.starts_with("cvs [") && self.text.ends_with(" aborted]: stopped by the user")
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_shapes_carry_the_command() {
        assert_eq!(
            Error::aborted("update", "no such file").text(),
            "cvs [update aborted]: no such file"
        );
        assert_eq!(
            Error::plain("status", "warning").text(),
            "cvs status: warning"
        );
    }

    #[test]
    fn an_interrupted_error_says_the_user_stopped_it() {
        let error = Error::interrupted("update");
        assert_eq!(error.text(), "cvs [update aborted]: stopped by the user");
        assert!(error.is_interrupted());
        assert!(!Error::aborted("update", "connection lost").is_interrupted());
        assert!(!Error::plain("update", "warning").is_interrupted());
    }
}
