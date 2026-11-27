//! The server-name listing that is merged into the working copy's file list.
//!
//! It is deliberately separate from [`crate::cvs::Runner`]: browsing must not take the one
//! command slot, print into the console, or make the window wait. `cvs rlog -R` sends only
//! names, so the transfer is small even for a folder with many files. The listing runs in
//! this process through `engine::run`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use cvsdesk::cvs_client::engine::{self, CancelToken, Request, Sink};
use cvsdesk::entries;
use cvsdesk::remote::{Found, Listing};

use crate::app::{App, RemoteState};

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
    /// When the listing started, so its closing log line can say how long it took.
    started: Instant,
    cancel: CancelToken,
}

impl Loader {
    pub(super) fn start(dir: &Path, repo: &str, seq: u64) -> Loader {
        let (tx, rx) = channel();
        let cancel = CancelToken::default();
        let worker_cancel = cancel.clone();
        let dir = dir.to_path_buf();
        let command_repo = repo.to_string();
        log::debug!(target: "cvsdesk::remote", "phase=start");

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
            started: Instant::now(),
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
    use std::collections::HashSet;
    use std::time::Duration;

    use cvsdesk::entries::{Origin, State};
    use cvsdesk::remote::{Found, Listing};

    use crate::app::tests::{app_with_rows, row};

    #[test]
    fn server_names_arrive_without_waiting_for_the_whole_listing() {
        let mut app = app_with_rows(vec![row("local.txt", false)]);

        app.apply_remote_names(&[
            Found {
                name: "server.txt".to_string(),
                is_dir: false,
            },
            Found {
                name: "LOCAL.txt".to_string(),
                is_dir: false,
            },
            Found {
                name: "more".to_string(),
                is_dir: true,
            },
        ]);

        assert_eq!(app.rows.len(), 3);
        assert_eq!(app.rows[1].name, "server.txt");
        assert_eq!(app.rows[1].state, State::Remote);
        assert_eq!(app.rows[1].origin, Origin::Remote);
        assert!(app.rows[2].is_dir);
        assert!(app.order_dirty);
    }

    #[test]
    fn a_batch_keeps_the_row_at_the_top_of_the_view() {
        let mut app = app_with_rows(vec![row("a.txt", false), row("z.txt", false)]);
        app.list_anchor = Some(("z.txt".to_string(), 2.5));

        app.apply_remote_names(&[Found {
            name: "b.txt".to_string(),
            is_dir: false,
        }]);

        assert_eq!(
            app.pending_scroll,
            Some(("z.txt".to_string(), 2.5)),
            "the next frame must restore the same top row"
        );
    }

    #[test]
    fn a_finished_listing_keeps_a_selected_server_row_by_name() {
        let mut app = app_with_rows(vec![row("local.txt", false)]);
        app.apply_remote_names(&[
            Found {
                name: "old.txt".to_string(),
                is_dir: false,
            },
            Found {
                name: "keep.txt".to_string(),
                is_dir: false,
            },
        ]);
        let keep = app
            .rows
            .iter()
            .position(|row| row.name == "keep.txt")
            .expect("the incremental batch added keep.txt");
        app.selected = HashSet::from([keep]);

        app.apply_remote_listing(Listing {
            files: vec!["keep.txt".to_string(), "new.txt".to_string()],
            dirs: Vec::new(),
        });

        assert!(!app.rows.iter().any(|row| row.name == "old.txt"));
        assert!(app.rows.iter().any(|row| row.name == "new.txt"));
        assert_eq!(app.selected_names(), vec!["keep.txt".to_string()]);
    }

    #[test]
    fn stopping_a_listing_keeps_the_names_found_so_far() {
        let mut app = app_with_rows(vec![row("local.txt", false)]);
        app.remote_state = RemoteState::Loading;
        app.remote_found = 1;
        app.apply_remote_names(&[Found {
            name: "keep.txt".to_string(),
            is_dir: false,
        }]);

        app.stop_remote_listing();

        assert_eq!(app.remote_state, RemoteState::Stopped);
        assert!(app.rows.iter().any(|row| row.name == "keep.txt"));
        assert!(
            app.status.contains("stopped"),
            "the status bar must say the list is partial: {}",
            app.status
        );
    }

    #[test]
    fn a_cached_listing_appears_without_starting_a_scan() {
        let mut app = app_with_rows(vec![row("local.txt", false)]);
        app.root = ":pserver:ann@host:/repo".to_string();
        app.repository = "Develop/Design".to_string();
        app.remote_enabled = true;
        app.remote_cache.put(
            cache_key(&app.root, &app.repository),
            Listing {
                files: vec!["cached.txt".to_string()],
                dirs: vec!["more".to_string()],
            },
        );

        app.start_remote_listing();

        assert!(app.remote.is_none(), "a cached folder must not scan again");
        assert_eq!(app.remote_state, RemoteState::Ready);
        assert!(app.rows.iter().any(|row| row.name == "cached.txt"));
        assert!(app.rows.iter().any(|row| row.name == "more"));
    }

    #[test]
    fn the_status_label_counts_the_names_while_the_listing_runs() {
        let mut app = app_with_rows(Vec::new());
        app.remote_state = RemoteState::Loading;
        app.remote_found = 123;

        assert_eq!(app.remote_status_label(), "Checking server… 123 found");
    }

    #[test]
    fn a_stopped_listing_is_reported_as_partial() {
        let mut app = app_with_rows(vec![row("local.txt", false)]);
        app.apply_remote_names(&[Found {
            name: "keep.txt".to_string(),
            is_dir: false,
        }]);
        app.remote_state = RemoteState::Stopped;

        assert_eq!(app.remote_status_label(), "1 not checked out (partial)");
    }

    #[test]
    fn the_cache_keeps_only_the_newest_listings() {
        let mut cache = Cache::default();
        for index in 0..=Cache::LIMIT {
            cache.put(format!("folder-{index}"), Listing::default());
        }

        assert!(cache.get("folder-0").is_none());
        assert!(cache.get(&format!("folder-{}", Cache::LIMIT)).is_some());
    }

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

/// One complete repository listing per folder, kept for this run of the window. A folder
/// opened again shows its cached names at once; View > Refresh server list asks again.
pub(super) struct Cache {
    entries: HashMap<String, Listing>,
    order: VecDeque<String>,
}

impl Cache {
    const LIMIT: usize = 16;

    pub(super) fn get(&mut self, key: &str) -> Option<Listing> {
        let listing = self.entries.get(key)?.clone();
        if let Some(at) = self.order.iter().position(|old| old == key) {
            if let Some(old) = self.order.remove(at) {
                self.order.push_back(old);
            }
        }
        Some(listing)
    }

    pub(super) fn put(&mut self, key: String, listing: Listing) {
        if self.entries.insert(key.clone(), listing).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > Self::LIMIT {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

/// The cache key: one repository folder under one root.
pub(super) fn cache_key(root: &str, repository: &str) -> String {
    format!("{}\n{}", root.trim(), repository.trim())
}

impl App {
    /// Show the repository names for this folder, from the cache when this window already
    /// read them, otherwise in the background.
    pub(super) fn start_remote_listing(&mut self) {
        self.cancel_remote();
        if !self.remote_enabled || self.shot.is_some() {
            return;
        }
        if !self.has_cvs || self.repository.trim().is_empty() {
            return;
        }
        let key = cache_key(&self.root, &self.repository);
        if let Some(listing) = self.remote_cache.get(&key) {
            self.apply_remote_listing(listing);
            return;
        }
        self.start_remote_scan();
    }

    /// Read the repository names again even when a listing is cached. The rows on screen
    /// stay while the scan runs.
    pub(super) fn refresh_remote_listing(&mut self) {
        if !self.remote_enabled || self.shot.is_some() {
            return;
        }
        if !self.has_cvs || self.repository.trim().is_empty() {
            return;
        }
        self.start_remote_scan();
    }

    fn start_remote_scan(&mut self) {
        // Dropping the old loader cancels a scan that is already running.
        self.remote = None;
        self.remote_seq = self.remote_seq.wrapping_add(1);
        self.remote_lines.clear();
        self.remote_pending.clear();
        self.remote_found = 0;
        self.remote_last_merge = Instant::now();
        self.remote_state = RemoteState::Loading;
        let root_path = entries::parse_cvsroot(&self.root).4;
        self.remote_parser = Some(cvsdesk::remote::Parser::new(&self.repository, &root_path));
        self.remote = Some(Loader::start(&self.dir, &self.repository, self.remote_seq));
    }

    /// Drop the running listing, if any. `Loader::drop` cancels its worker.
    pub(super) fn cancel_remote(&mut self) {
        if self.remote.is_some() {
            log::debug!(target: "cvsdesk::remote", "phase=end result=cancelled");
        }
        self.remote = None;
        self.remote_parser = None;
        self.remote_pending.clear();
        self.remote_lines.clear();
        self.remote_found = 0;
        self.remote_state = RemoteState::Off;
    }

    /// Stop reading the repository names on the user's request. The names found so far
    /// stay on screen; the status bar says the list is partial.
    pub(super) fn stop_remote_listing(&mut self) {
        if self.remote_state != RemoteState::Loading {
            return;
        }
        self.remote = None;
        self.remote_parser = None;
        self.remote_lines.clear();
        self.flush_remote_pending();
        self.remote_state = RemoteState::Stopped;
        log::info!(
            target: "cvsdesk::remote",
            "phase=end result=stopped found={}",
            self.remote_found
        );
        self.status = format!("Server folder list stopped — {} found", self.remote_found);
    }

    /// Take whatever the worker has said so far. Names are merged in small batches while
    /// the scan runs; the complete listing is applied when it finishes.
    pub(super) fn poll_remote(&mut self) {
        let (lines, finished, current) = {
            let Some(loader) = self.remote.as_mut() else {
                return;
            };
            let mut lines = Vec::new();
            let mut finished: Option<Result<i32, String>> = None;
            loop {
                match loader.rx.try_recv() {
                    Ok(Msg::Line(line)) => lines.push(line),
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
            let current = loader.seq == self.remote_seq && loader.repo == self.repository;
            (lines, finished, current)
        };
        if !current {
            if finished.is_some() {
                log::debug!(target: "cvsdesk::remote", "phase=end result=stale");
            }
            return;
        }
        self.absorb_remote_lines(lines);
        let Some(finished) = finished else {
            self.merge_remote_batch_if_due();
            return;
        };

        let loader = self.remote.take().expect("the loader was just borrowed");
        let elapsed = loader.started.elapsed().as_millis();
        let line_count = self.remote_lines.len();
        let root_path = entries::parse_cvsroot(&self.root).4;
        let listing = self
            .remote_parser
            .take()
            .map(|parser| parser.finish())
            .unwrap_or_else(|| {
                cvsdesk::remote::parse(&self.remote_lines, &loader.repo, &root_path)
            });
        match finished {
            Ok(0) => {
                let error = cvsdesk::remote::error_line(&self.remote_lines).map(str::to_string);
                if let Some(error) = error {
                    log::warn!(
                        target: "cvsdesk::remote",
                        "phase=end code=0 result=cvs-error lines={} elapsed_ms={}",
                        line_count,
                        elapsed
                    );
                    self.flush_remote_pending();
                    self.remote_state = RemoteState::Failed;
                    self.status = format!("Server list unavailable: {}", error);
                } else {
                    log::info!(
                        target: "cvsdesk::remote",
                        "phase=end code=0 found={} lines={} elapsed_ms={}",
                        self.remote_found,
                        line_count,
                        elapsed
                    );
                    self.apply_remote_listing(listing);
                }
            }
            Ok(code) => {
                log::warn!(
                    target: "cvsdesk::remote",
                    "phase=end code={} result=error lines={} elapsed_ms={}",
                    code,
                    line_count,
                    elapsed
                );
                self.flush_remote_pending();
                self.remote_state = RemoteState::Failed;
                self.status = format!("Server list unavailable (cvs returned {})", code);
            }
            Err(error) => {
                log::warn!(
                    target: "cvsdesk::remote",
                    "phase=end result=engine-error lines={} elapsed_ms={}",
                    line_count,
                    elapsed
                );
                self.flush_remote_pending();
                self.remote_state = RemoteState::Failed;
                self.status = format!("Server list unavailable: {}", error);
            }
        }
        self.remote_lines.clear();
    }

    fn absorb_remote_lines(&mut self, lines: Vec<String>) {
        for line in lines {
            if let Some(parser) = self.remote_parser.as_mut() {
                if let Some(found) = parser.push(&line) {
                    self.remote_pending.push(found);
                    self.remote_found += 1;
                }
            }
            self.remote_lines.push(line);
        }
    }

    fn merge_remote_batch_if_due(&mut self) {
        if self.remote_pending.is_empty() || self.list_interacting {
            return;
        }
        if self.remote_last_merge.elapsed() < Duration::from_millis(750)
            && self.remote_pending.len() < 300
        {
            return;
        }
        self.flush_remote_pending();
    }

    fn flush_remote_pending(&mut self) {
        if self.remote_pending.is_empty() {
            return;
        }
        let count = self.remote_pending.len().min(2000);
        let batch: Vec<Found> = self.remote_pending.drain(..count).collect();
        self.apply_remote_names(&batch);
        self.remote_last_merge = Instant::now();
    }

    /// Add names that arrived while the scan is running. The row at the top of the view
    /// keeps its place through the next order rebuild.
    pub(super) fn apply_remote_names(&mut self, names: &[Found]) {
        if names.is_empty() {
            return;
        }
        if cvsdesk::remote::add_new_rows(&mut self.rows, names) > 0 {
            self.pending_scroll = self.list_anchor.clone();
            self.order_dirty = true;
        }
    }

    /// Apply the complete listing: add what is missing, drop names the repository no
    /// longer has, and keep the selection on the same names. The listing is cached for
    /// the next visit to this folder.
    pub(super) fn apply_remote_listing(&mut self, listing: Listing) {
        let selected: HashSet<String> = self
            .selected
            .iter()
            .filter_map(|index| self.rows.get(*index))
            .map(|row| row.name.clone())
            .collect();
        let anchor = self
            .anchor
            .and_then(|index| self.rows.get(index))
            .map(|row| row.name.clone());
        if self.list_anchor.is_some() {
            self.pending_scroll = self.list_anchor.clone();
        }

        let local = std::mem::take(&mut self.rows);
        self.rows = cvsdesk::remote::merge_rows(local, &listing);
        self.remote_state = RemoteState::Ready;
        self.selected = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| selected.contains(&row.name))
            .map(|(index, _)| index)
            .collect();
        self.anchor = anchor.and_then(|name| self.rows.iter().position(|row| row.name == name));
        self.order_dirty = true;
        self.state_dirty = true;
        self.counts = self.count_states();
        self.status = format!(
            "{} file(s) in this folder",
            self.rows.iter().filter(|row| !row.is_dir).count()
        );
        self.remote_found = listing.files.len() + listing.dirs.len();
        self.remote_pending.clear();
        self.remote_parser = None;
        self.remote_lines.clear();
        if !self.repository.trim().is_empty() {
            self.remote_cache
                .put(cache_key(&self.root, &self.repository), listing);
        }
    }
}
