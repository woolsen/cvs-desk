//! The dialogs: one module per window, with the command its button runs.

use cvsdesk::graph::RevNode;
use cvsdesk::prefs::Prefs;

use crate::app::{App, OutputKind};

mod about;
mod checkout;
mod command_line;
mod commit;
mod diff;
mod graph;
mod import;
mod login;
mod preferences;
mod repository;
pub(super) mod tags;

pub(super) enum Dialog {
    Checkout {
        tag: String,
        readonly: bool,
        module: String,
        root: String,
    },
    Import {
        vendor: String,
        release: String,
        message: String,
        module: String,
        root: String,
    },
    Tags(Box<tags::TagsState>),
    Commit {
        text: String,
        file: String,
        force: bool,
    },
    Login {
        password: String,
        root: String,
    },
    Diff {
        unified: bool,
        ignore_case: bool,
        no_whitespace: bool,
        no_blank_lines: bool,
        extra: String,
    },
    Preferences {
        tab: usize,
        draft: Box<Prefs>,
    },
    CommandLine {
        text: String,
    },
    Graph {
        file: String,
        nodes: Vec<RevNode>,
    },
    About,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DialogAction {
    Close,
    Checkout,
    Import,
    TagsCreate,
    TagsDelete,
    TagsRetry,
    Commit,
    Login,
    Diff,
    SavePrefs,
    RunLine,
    ShowSheet,
}

impl App {
    pub(super) fn ui_dialogs(&mut self, ctx: &egui::Context) {
        let sel_names = self.selected_local_names();
        let cvsroot = self.root.clone();
        let repository = self.repository.clone();
        let recent_roots = self.prefs.recent_roots.clone();
        let kind_label = self.folder_kind().unwrap_or("").to_string();
        let dir = self.dir.display().to_string();
        let busy = self.runner.running;
        let has_cvs = self.has_cvs;
        let action = {
            let Some(d) = &mut self.dialog else {
                return;
            };
            match d {
                Dialog::Checkout {
                    tag,
                    readonly,
                    module,
                    root,
                } => checkout::show(ctx, tag, readonly, module, root, &recent_roots),
                Dialog::Import {
                    vendor,
                    release,
                    message,
                    module,
                    root,
                } => import::show(
                    ctx,
                    vendor,
                    release,
                    message,
                    module,
                    root,
                    &recent_roots,
                    &self.prefs,
                ),
                Dialog::Tags(state) => tags::show(ctx, state, busy, has_cvs),
                Dialog::Commit { text, file, force } => {
                    commit::show(ctx, text, file, force, &sel_names)
                }
                Dialog::Login { password, root } => login::show(ctx, password, root, &recent_roots),
                Dialog::Diff {
                    unified,
                    ignore_case,
                    no_whitespace,
                    no_blank_lines,
                    extra,
                } => diff::show(
                    ctx,
                    unified,
                    ignore_case,
                    no_whitespace,
                    no_blank_lines,
                    extra,
                ),
                Dialog::Preferences { tab, draft } => preferences::show(ctx, tab, draft),
                Dialog::CommandLine { text } => command_line::show(ctx, text, &cvsroot),
                Dialog::Graph { file, nodes } => graph::show(ctx, file, nodes),
                Dialog::About => about::show(ctx, &cvsroot, &kind_label, &repository, &dir),
            }
        };
        if let Some(a) = action {
            self.handle_dialog_action(a);
        }
    }

    fn handle_dialog_action(&mut self, a: DialogAction) {
        match a {
            DialogAction::Close => {
                self.dialog = None;
                self.tag_waiting = None;
                return;
            }
            DialogAction::TagsCreate => {
                tags::create(self);
                return;
            }
            DialogAction::TagsDelete => {
                tags::delete(self);
                return;
            }
            DialogAction::TagsRetry => {
                self.request_tags();
                return;
            }
            DialogAction::ShowSheet => {
                self.refresh_state_sheet();
                self.output = OutputKind::State;
                self.dialog = None;
                return;
            }
            _ => {}
        }
        let Some(d) = self.dialog.take() else {
            return;
        };
        match a {
            DialogAction::Checkout => checkout::apply(self, d),
            DialogAction::Import => import::apply(self, d),
            DialogAction::Commit => commit::apply(self, d),
            DialogAction::Login => login::apply(self, d),
            DialogAction::Diff => diff::apply(self, d),
            DialogAction::SavePrefs => preferences::apply(self, d),
            DialogAction::RunLine => command_line::apply(self, d),
            DialogAction::Close
            | DialogAction::ShowSheet
            | DialogAction::TagsCreate
            | DialogAction::TagsDelete
            | DialogAction::TagsRetry => {}
        }
    }
}
