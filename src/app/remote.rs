//! The server-name listing that is merged into the working copy's file list.
//!
//! It is deliberately separate from [`crate::cvs::Runner`]: browsing must not take the one
//! command slot, print into the console, or make the window wait. `cvs rlog -R` sends only
//! names, so the transfer is small even for a folder with many files. The listing runs in
//! this process through `engine::run`.

use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use cvsdesk::cvs_client::engine::{self, CancelToken, Request, Sink};
use cvsdesk::entries::{self, Origin};

use crate::app::App;

/// One message from the worker.
pub(super) enum Msg {
    Line(String),
    Done(i32),
    Failed(String),
}

/// A running `cvs rlog -R` and the channel that carries its lines back.
pub(super) struct Loader {
    pub(super) rx: Receiver<Msg>,
    pub(super) repo: String,
    pub(super) seq: u64,
    cancel: CancelToken,
}

impl Loader {
    pub(super) fn start(dir: &Path, repo: &str, seq: u64) -> Loader {
        let (tx, rx) = channel();
        let cancel = CancelToken::default();
        let worker_cancel = cancel.clone();
        let dir = dir.to_path_buf();
        let command_repo = repo.to_string();

        std::thread::spawn(move || {
            let mut sink = RemoteSink { tx };
            let request = Request {
                cwd: dir,
                args: vec!["rlog".to_string(), "-R".to_string(), command_repo],
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
            repo: repo.to_string(),
            seq,
            cancel,
        }
    }

    fn cancel(&mut self) {
        self.cancel.cancel();
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The listing's sink: every line goes to the window's channel.
struct RemoteSink {
    tx: Sender<Msg>,
}

impl RemoteSink {
    fn line(&self, text: String) {
        for line in text.lines() {
            let _ = self.tx.send(Msg::Line(line.to_string()));
        }
    }
}

impl Sink for RemoteSink {
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
    use std::time::Duration;

    #[test]
    fn an_in_process_listing_reports_a_missing_working_copy() {
        let dir = std::env::temp_dir().join(format!("cvsdesk-remote-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let loader = Loader::start(&dir, "mod", 1);
        let mut failure = None;
        let mut code = None;
        loop {
            match loader.rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Msg::Line(_)) => {}
                Ok(Msg::Failed(error)) => {
                    failure = Some(error);
                    break;
                }
                Ok(Msg::Done(done)) => {
                    code = Some(done);
                    break;
                }
                Err(error) => panic!("the listing did not finish: {}", error),
            }
        }
        assert!(
            failure.is_some() || code == Some(1),
            "a folder without CVS must fail: failure={failure:?} code={code:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

impl App {
    /// Ask the repository for the current folder's names, in the background.
    pub(super) fn start_remote_listing(&mut self) {
        self.cancel_remote();
        if !self.remote_enabled || self.shot.is_some() {
            return;
        }
        if !self.has_cvs || self.repository.trim().is_empty() {
            return;
        }
        self.remote_seq = self.remote_seq.wrapping_add(1);
        self.remote = Some(Loader::start(&self.dir, &self.repository, self.remote_seq));
    }

    /// Drop the running listing, if any. `Loader::drop` cancels its worker.
    pub(super) fn cancel_remote(&mut self) {
        self.remote = None;
        self.remote_lines.clear();
    }

    /// Take whatever the worker has said so far, and merge the names when it finishes.
    pub(super) fn poll_remote(&mut self) {
        let Some(loader) = self.remote.as_mut() else {
            return;
        };
        let mut finished: Option<Result<i32, String>> = None;
        loop {
            match loader.rx.try_recv() {
                Ok(Msg::Line(line)) => self.remote_lines.push(line),
                Ok(Msg::Done(code)) => {
                    finished = Some(Ok(code));
                    break;
                }
                Ok(Msg::Failed(error)) => {
                    finished = Some(Err(error));
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("the server listing stopped".into()));
                    break;
                }
            }
        }
        let Some(finished) = finished else {
            return;
        };

        let loader = self.remote.take().expect("the loader was just borrowed");
        let lines = std::mem::take(&mut self.remote_lines);
        if loader.seq != self.remote_seq || loader.repo != self.repository {
            return;
        }
        match finished {
            Ok(0) => {
                if let Some(error) = cvsdesk::remote::error_line(&lines) {
                    self.status = format!("Server list unavailable: {}", error);
                    return;
                }
                let root_path = entries::parse_cvsroot(&self.root).4;
                let listing = cvsdesk::remote::parse(&lines, &loader.repo, &root_path);
                self.apply_remote_listing(listing);
            }
            Ok(code) => {
                self.status = format!("Server list unavailable (cvs returned {})", code);
            }
            Err(error) => {
                self.status = format!("Server list unavailable: {}", error);
            }
        }
    }

    /// Merge repository names into the rows on screen. Local rows win; any remote rows
    /// from an earlier listing are replaced.
    pub(super) fn apply_remote_listing(&mut self, listing: cvsdesk::remote::Listing) {
        let had_remote_selection = self.selected.iter().any(|index| {
            self.rows
                .get(*index)
                .is_some_and(|row| row.origin == Origin::Remote)
        });
        let local = std::mem::take(&mut self.rows);
        self.rows = cvsdesk::remote::merge_rows(local, &listing);
        if had_remote_selection {
            self.selected.clear();
            self.anchor = None;
        }
        self.order_dirty = true;
        self.state_dirty = true;
        self.counts = self.count_states();
        self.status = format!(
            "{} file(s) in this folder",
            self.rows.iter().filter(|row| !row.is_dir).count()
        );
    }
}
