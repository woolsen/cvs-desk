//! The diff preview inside the File History window.
//!
//! It is deliberately separate from [`crate::cvs::Runner`], like the remote listing in
//! [`super::remote`]: selecting revisions must not take the one command slot, print into
//! the console, or make the window wait. The preview runs in this process through
//! `engine::run`, and dropping the loader cancels the worker.

use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};

use cvsdesk::cvs::{Cmd, CommandRequest};
use cvsdesk::cvs_client::engine::{self, CancelToken, Request, Sink};

/// One diff the preview should show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// The selected revision against the revision before it.
    Previous,
    /// The selected revision against the working file.
    Working,
}

/// One message from the worker.
pub(super) enum Msg {
    Line(String),
    Done(i32),
    Failed(String),
}

/// A running `cvs diff` and the channel that carries its lines back.
pub(super) struct Loader {
    pub(super) rx: Receiver<Msg>,
    pub(super) revision: String,
    pub(super) target: Target,
    cancel: CancelToken,
}

impl Loader {
    pub(super) fn start(dir: &Path, args: Vec<String>, revision: String, target: Target) -> Loader {
        let (tx, rx) = channel();
        let cancel = CancelToken::default();
        let worker_cancel = cancel.clone();
        let dir = dir.to_path_buf();

        std::thread::spawn(move || {
            let mut sink = PreviewSink { tx, lines: 0 };
            let request = Request {
                cwd: dir,
                args,
                password: None,
            };
            match engine::run(request, &mut sink, &worker_cancel) {
                Ok(code) => {
                    let _ = sink.tx.send(Msg::Done(code));
                }
                Err(error) => {
                    let msg = if worker_cancel.is_cancelled() {
                        Msg::Done(-1)
                    } else {
                        Msg::Failed(error.to_string())
                    };
                    let _ = sink.tx.send(msg);
                }
            }
        });

        Loader {
            rx,
            revision,
            target,
            cancel,
        }
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// `cvs diff` exits 1 when it found differences. That is a successful preview.
pub(super) fn success_code(code: i32) -> bool {
    code == 0 || code == 1
}

/// The arguments for one preview. Kept as a function so the `-r` order and the file's
/// place at the end can be checked without starting a process.
pub(super) fn diff_args(
    file: &str,
    revision: &str,
    previous: Option<&str>,
    flags: &[String],
    root: &str,
    globals: &[String],
) -> Vec<String> {
    let mut req = CommandRequest::new(Cmd::Diff);
    req.extra = flags.to_vec();
    if let Some(previous) = previous {
        req.extra.push(format!("-r{}", previous));
    }
    req.extra.push(format!("-r{}", revision));
    req.files = vec![file.to_string()];
    req.to_args(root, globals)
}

/// The preview's sink: every line becomes a message, and a very large diff is stopped
/// with a note instead of filling the window's memory.
struct PreviewSink {
    tx: Sender<Msg>,
    lines: usize,
}

const MAX_LINES: usize = 4000;

impl PreviewSink {
    fn line(&mut self, text: String) {
        for line in text.lines() {
            if self.lines > MAX_LINES {
                return;
            }
            if self.lines == MAX_LINES {
                let _ = self.tx.send(Msg::Line("… diff truncated …".to_string()));
            } else {
                let _ = self.tx.send(Msg::Line(line.to_string()));
            }
            self.lines += 1;
        }
    }
}

impl Sink for PreviewSink {
    fn out(&mut self, line: String) {
        self.line(line);
    }

    fn err(&mut self, line: String) {
        self.line(line);
    }

    fn prompt(&mut self, text: String) {
        self.line(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diff_exit_code_of_one_is_a_successful_preview() {
        assert!(success_code(0));
        assert!(success_code(1));
        assert!(!success_code(2));
        assert!(!success_code(-1));
    }

    #[test]
    fn the_diff_args_name_the_older_revision_first_and_the_file_last() {
        let args = diff_args(
            "main.c",
            "1.4",
            Some("1.3"),
            &["-u".to_string()],
            ":pserver:guest@example.com:/repo",
            &[],
        );
        assert_eq!(
            args,
            vec![
                "-d",
                ":pserver:guest@example.com:/repo",
                "diff",
                "-u",
                "-r1.3",
                "-r1.4",
                "main.c"
            ]
        );
    }

    #[test]
    fn the_working_copy_preview_names_only_the_selected_revision() {
        let args = diff_args("main.c", "1.4", None, &[], "", &[]);
        assert_eq!(args, vec!["diff", "-r1.4", "main.c"]);
    }
}
