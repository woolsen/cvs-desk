//! The Tags window: the tags of the selected files or the current folder, with one place
//! to create one and one place to delete one. The CVS switches stay behind the buttons.

use cvsdesk::cvs::{Cmd, CommandRequest};
use cvsdesk::tags::{self, TagInfo};

use super::{Dialog, DialogAction};
use crate::app::ui::{typography, wrap};
use crate::app::{App, OutputKind, TagWait};

const ERROR: egui::Color32 = egui::Color32::from_rgb(180, 60, 60);

/// What the window is showing and what the user has typed or selected.
pub(crate) struct TagsState {
    /// The files or folders the window acts on; empty means the current folder.
    pub(crate) scope: Vec<String>,
    pub(crate) scope_label: String,
    pub(crate) tags: Vec<TagInfo>,
    pub(crate) name: String,
    pub(crate) branch: bool,
    pub(crate) selected: Option<String>,
    /// The tag waiting for the inline "Delete?" confirmation.
    pub(crate) confirm: Option<String>,
    pub(crate) loading: bool,
    /// True until the name field has been given the keyboard on the first frame.
    pub(crate) focus_name: bool,
    pub(crate) error: String,
    pub(crate) status: String,
}

impl TagsState {
    pub(crate) fn loading(scope: Vec<String>, scope_label: String) -> Self {
        Self {
            scope,
            scope_label,
            tags: Vec::new(),
            name: String::new(),
            branch: false,
            selected: None,
            confirm: None,
            loading: true,
            focus_name: true,
            error: String::new(),
            status: String::new(),
        }
    }
}

pub(super) fn show(
    ctx: &egui::Context,
    state: &mut TagsState,
    busy: bool,
    has_cvs: bool,
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Tags")
        .open(&mut open)
        .drag_area(egui::WindowDrag::Anywhere)
        .collapsible(false)
        .resizable(true)
        .default_width(580.0)
        .show(ctx, |ui| {
            wrap::label_in(
                ui,
                &format!("Scope: {}", state.scope_label),
                typography::small(),
                true,
            );
            ui.add_space(4.0);

            // The name can be typed while the list is still being read; only the button
            // that starts a second cvs command waits for the query to finish.
            let form_enabled = has_cvs;
            ui.add_enabled_ui(form_enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Tag name:");
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut state.name)
                            .hint_text("Release_1_0")
                            .desired_width(260.0),
                    );
                    if form_enabled && state.focus_name {
                        state.focus_name = false;
                        ui.memory_mut(|m| m.request_focus(response.id));
                    }
                    let name = state.name.trim();
                    let problem = if name.is_empty() {
                        None
                    } else {
                        tags::name_problem(name)
                    };
                    let exists = !name.is_empty() && state.tags.iter().any(|tag| tag.name == name);
                    let can_create = !busy && !name.is_empty() && problem.is_none() && !exists;
                    if ui
                        .add_enabled(can_create, egui::Button::new("Create tag"))
                        .clicked()
                    {
                        action = Some(DialogAction::TagsCreate);
                    }
                });
                ui.checkbox(&mut state.branch, "Create as branch");
                if let Some(problem) =
                    tags::name_problem(&state.name).filter(|_| !state.name.trim().is_empty())
                {
                    ui.colored_label(ERROR, problem);
                } else if !state.name.trim().is_empty()
                    && state.tags.iter().any(|tag| tag.name == state.name.trim())
                {
                    ui.colored_label(ERROR, "A tag with this name already exists");
                }
            });
            if !has_cvs {
                ui.colored_label(ERROR, "This folder is not a CVS working copy.");
            }

            ui.separator();
            if state.loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading tags…");
                });
            } else if !state.error.is_empty() {
                ui.colored_label(ERROR, &state.error);
                if ui.add_enabled(!busy, egui::Button::new("Retry")).clicked() {
                    action = Some(DialogAction::TagsRetry);
                }
            }

            let mut picked = None;
            if !state.tags.is_empty() {
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        egui::Grid::new("tags_grid")
                            .num_columns(3)
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new("Name").strong());
                                ui.label(egui::RichText::new("Type").strong());
                                ui.label(egui::RichText::new("Revision").strong());
                                ui.end_row();
                                for tag in &state.tags {
                                    let selected =
                                        state.selected.as_deref() == Some(tag.name.as_str());
                                    // Every cell selects the row, so the user does not have
                                    // to aim at the name column before pressing Delete.
                                    if ui.selectable_label(selected, &tag.name).clicked()
                                        || ui.selectable_label(selected, tag.type_label()).clicked()
                                        || ui
                                            .selectable_label(selected, tag.revision_label())
                                            .clicked()
                                    {
                                        picked = Some(tag.name.clone());
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            } else if !state.loading && state.error.is_empty() {
                ui.label(
                    egui::RichText::new("No tags in this scope yet.")
                        .italics()
                        .weak(),
                );
            }
            if let Some(name) = picked {
                state.selected = Some(name);
                state.confirm = None;
            }

            ui.separator();
            ui.horizontal(|ui| {
                if let Some(name) = state.confirm.clone() {
                    ui.label(format!("Delete tag {}?", name));
                    if ui
                        .add_enabled(!busy && has_cvs, egui::Button::new("Delete"))
                        .clicked()
                    {
                        action = Some(DialogAction::TagsDelete);
                    }
                    if ui.button("Cancel").clicked() {
                        state.confirm = None;
                    }
                } else {
                    let can_delete = state.selected.is_some() && !busy && has_cvs;
                    if ui
                        .add_enabled(can_delete, egui::Button::new("Delete tag…"))
                        .clicked()
                    {
                        state.confirm = state.selected.clone();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            action = Some(DialogAction::Close);
                        }
                    });
                }
            });
            if !state.status.is_empty() {
                ui.label(egui::RichText::new(&state.status).small().weak());
            }
        });
    if !open {
        action = Some(DialogAction::Close);
    }
    action
}

/// Create the tag named in the window. The request carries the scope the window was
/// opened on, so it does not drift when the file list's selection changes behind it.
pub(super) fn create(app: &mut App) {
    if app.runner.running {
        return;
    }
    let Some((name, branch, scope, exists)) = tag_request_parts(app) else {
        return;
    };
    if let Some(problem) = tags::name_problem(&name) {
        set_status(app, problem);
        return;
    }
    if exists {
        set_status(app, "A tag with this name already exists".to_string());
        return;
    }

    let mut req = CommandRequest::new(Cmd::Tag);
    req.tag = name.clone();
    req.branch = branch;
    req.files = scope;
    app.start(req, OutputKind::Console);
    if app.runner.running {
        app.tag_waiting = Some(TagWait::AfterWrite);
    }
    if let Some(Dialog::Tags(state)) = app.dialog.as_mut() {
        state.confirm = None;
        state.error.clear();
        state.status = format!("Creating {}…", name);
    }
}

/// Delete the tag the user confirmed. The name comes from the window, not from a text
/// field, so a typo cannot remove the wrong tag.
pub(super) fn delete(app: &mut App) {
    if app.runner.running {
        return;
    }
    let Some((name, scope)) = confirmed_tag(app) else {
        return;
    };

    let mut req = CommandRequest::new(Cmd::Tag);
    req.tag = name.clone();
    req.delete = true;
    req.files = scope;
    app.start(req, OutputKind::Console);
    if app.runner.running {
        app.tag_waiting = Some(TagWait::AfterWrite);
    }
    if let Some(Dialog::Tags(state)) = app.dialog.as_mut() {
        state.confirm = None;
        state.error.clear();
        state.status = format!("Deleting {}…", name);
    }
}

fn tag_request_parts(app: &App) -> Option<(String, bool, Vec<String>, bool)> {
    let Some(Dialog::Tags(state)) = app.dialog.as_ref() else {
        return None;
    };
    let name = state.name.trim().to_string();
    let exists = state.tags.iter().any(|tag| tag.name == name);
    Some((name, state.branch, state.scope.clone(), exists))
}

fn confirmed_tag(app: &App) -> Option<(String, Vec<String>)> {
    let Some(Dialog::Tags(state)) = app.dialog.as_ref() else {
        return None;
    };
    state
        .confirm
        .clone()
        .map(|name| (name, state.scope.clone()))
}

fn set_status(app: &mut App, status: String) {
    if let Some(Dialog::Tags(state)) = app.dialog.as_mut() {
        state.status = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::app_with_rows;

    fn state(scope: &[&str], name: &str) -> TagsState {
        let scope: Vec<String> = scope.iter().map(|s| s.to_string()).collect();
        let mut state = TagsState::loading(scope, "test scope".to_string());
        state.name = name.to_string();
        state.loading = false;
        state
    }

    fn line(app: &App) -> String {
        app.runner.lines.first().cloned().unwrap_or_default()
    }

    #[test]
    fn creating_a_tag_uses_the_name_the_branch_flag_and_the_scope() {
        let mut app = app_with_rows(Vec::new());
        let mut state = state(&["a.txt", "b.txt"], "Release_2_0");
        state.branch = true;
        app.dialog = Some(Dialog::Tags(Box::new(state)));

        create(&mut app);

        assert!(
            line(&app).contains("tag -b Release_2_0 a.txt b.txt"),
            "{}",
            line(&app)
        );
        assert_eq!(app.tag_waiting, Some(TagWait::AfterWrite));
    }

    #[test]
    fn deleting_a_tag_uses_the_confirmed_name_and_the_scope() {
        let mut app = app_with_rows(Vec::new());
        let mut state = state(&["a.txt"], "");
        state.selected = Some("Release_1_0".to_string());
        state.confirm = Some("Release_1_0".to_string());
        app.dialog = Some(Dialog::Tags(Box::new(state)));

        delete(&mut app);

        assert!(
            line(&app).contains("tag -d Release_1_0 a.txt"),
            "{}",
            line(&app)
        );
        assert_eq!(app.tag_waiting, Some(TagWait::AfterWrite));
    }

    #[test]
    fn a_name_that_does_not_start_with_a_letter_never_reaches_cvs() {
        let mut app = app_with_rows(Vec::new());
        app.dialog = Some(Dialog::Tags(Box::new(state(&[], "-d"))));

        create(&mut app);

        assert!(app.runner.lines.is_empty(), "{:?}", app.runner.lines);
        let Some(Dialog::Tags(state)) = &app.dialog else {
            panic!("the window is still open");
        };
        assert_eq!(state.status, "A tag name must start with a letter");
    }

    #[test]
    fn a_name_already_in_the_list_is_not_created_again() {
        let mut app = app_with_rows(Vec::new());
        let mut state = state(&[], "Release_1_0");
        state.tags = vec![TagInfo {
            name: "Release_1_0".to_string(),
            branch: false,
            revision: Some("1.2".to_string()),
        }];
        app.dialog = Some(Dialog::Tags(Box::new(state)));

        create(&mut app);

        assert!(app.runner.lines.is_empty(), "{:?}", app.runner.lines);
        let Some(Dialog::Tags(state)) = &app.dialog else {
            panic!("the window is still open");
        };
        assert_eq!(state.status, "A tag with this name already exists");
    }
}
