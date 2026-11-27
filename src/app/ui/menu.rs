//! The menu bar.

use cvsdesk::cvs::{Cmd, CommandRequest};

use crate::app::dialogs::Dialog;
use crate::app::files::SortKey;
use crate::app::{App, OutputKind};

impl App {
    // -- ui -----------------------------------------------------------------

    pub(super) fn ui_menu(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        // A folder without a `CVS/Root` has nothing for these commands to act on. They
        // stay visible but greyed, so the reason is on screen instead of in an error.
        let in_project = !self.root.is_empty();
        let history_ready = self.selected.len() == 1
            && self
                .selected
                .iter()
                .next()
                .and_then(|index| self.rows.get(*index))
                .is_some_and(|row| !row.is_dir && row.origin != cvsdesk::entries::Origin::Remote);
        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open folder…").clicked() {
                        self.open_folder(frame);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Checkout…").clicked() {
                        self.open_dialog(Dialog::Checkout {
                            tag: "HEAD".into(),
                            readonly: self.prefs.checkout_ro,
                            module: String::new(),
                            root: self.default_connection(),
                        });
                        ui.close();
                    }
                    if ui.button("Import…").clicked() {
                        self.open_dialog(Dialog::Import {
                            vendor: String::new(),
                            release: String::new(),
                            message: String::new(),
                            module: String::new(),
                            root: self.default_connection(),
                        });
                        ui.close();
                    }
                    if ui.button("Init").clicked() {
                        let mut req = CommandRequest::new(Cmd::Init);
                        req.files.clear();
                        self.start(req, OutputKind::Console);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                ui.menu_button("View", |ui| {
                    if ui
                        .add_enabled(self.history.can_back(), egui::Button::new("Back"))
                        .clicked()
                    {
                        self.go_back();
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.history.can_forward(), egui::Button::new("Forward"))
                        .clicked()
                    {
                        self.go_forward();
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.can_go_up(), egui::Button::new("Up"))
                        .clicked()
                    {
                        self.go_up();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Reload view").clicked() {
                        self.reload();
                        self.output = OutputKind::Console;
                        ui.close();
                    }
                    if ui
                        .add_enabled(in_project, egui::Button::new("Refresh server list"))
                        .on_hover_text("Read the repository names again with rlog -R")
                        .clicked()
                    {
                        self.refresh_remote_listing();
                        ui.close();
                    }
                    if ui.button("File states…").clicked() {
                        self.refresh_state_sheet();
                        self.output = OutputKind::State;
                        ui.close();
                    }
                    if ui.button("Explore").clicked() {
                        self.explore();
                        ui.close();
                    }
                    if ui.button("Search this folder").clicked() {
                        self.open_search();
                        ui.close();
                    }
                    if ui
                        .button("Select all shown")
                        .on_hover_text("Select every row the current view shows (Ctrl+A).")
                        .clicked()
                    {
                        self.select_all_shown();
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("Show", |ui| {
                        let f = &mut self.filters;
                        let mut changed = false;
                        changed |= ui.checkbox(&mut f.unchanged, "Up-to-date files").changed();
                        changed |= ui.checkbox(&mut f.changed, "Locally modified").changed();
                        changed |= ui.checkbox(&mut f.added, "Locally added").changed();
                        changed |= ui.checkbox(&mut f.removed, "Locally removed").changed();
                        changed |= ui.checkbox(&mut f.conflicts, "Conflicts").changed();
                        changed |= ui.checkbox(&mut f.unknown, "Untracked").changed();
                        changed |= ui
                            .checkbox(&mut f.hide_unknown, "Hide untracked files")
                            .changed();
                        changed |= ui.checkbox(&mut f.missing, "Missing locally").changed();
                        changed |= ui
                            .checkbox(&mut f.hide_missing, "Hide missing files")
                            .changed();
                        changed |= ui.checkbox(&mut f.folders, "Sub-folders").changed();
                        if changed {
                            self.order_dirty = true;
                        }
                    });
                    ui.menu_button("Sort by", |ui| {
                        let active = self.effective_sort();
                        let mut keys = vec![SortKey::Name, SortKey::State, SortKey::Revision];
                        if self.has_row_tags() {
                            keys.push(SortKey::Tag);
                        }
                        keys.push(SortKey::Date);
                        for k in keys {
                            let label = if active == k {
                                format!("{} {}", k.label(), if self.sort_asc { "↑" } else { "↓" })
                            } else {
                                k.label().to_owned()
                            };
                            if ui.radio(active == k, label).clicked() {
                                self.flip_sort(k);
                            }
                        }
                    });
                });

                ui.menu_button("Modify", |ui| {
                    if !in_project {
                        project_hint(ui);
                    }
                    ui.add_enabled_ui(in_project, |ui| {
                        if ui.button("Update").clicked() {
                            self.cvs_command(Cmd::Update);
                            ui.close();
                        }
                        if ui
                            .button("Update All (This Folder)")
                            .on_hover_text(
                                "Update this folder and its subfolders, whatever is selected",
                            )
                            .clicked()
                        {
                            self.update_whole_folder();
                            ui.close();
                        }
                        if ui.button("Commit…").clicked() {
                            let template = self.prefs.log_template.clone();
                            self.open_commit_dialog(template);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Add").clicked() {
                            self.cvs_command(Cmd::Add);
                            ui.close();
                        }
                        if ui.button("Add Binary").clicked() {
                            self.cvs_command(Cmd::AddBinary);
                            ui.close();
                        }
                        if ui.button("Add Unicode").clicked() {
                            self.cvs_command(Cmd::AddUnicode);
                            ui.close();
                        }
                        if ui.button("Remove").clicked() {
                            self.cvs_command(Cmd::Remove);
                            ui.close();
                        }
                        if ui.button("Move To Trash").clicked() {
                            self.move_to_trash();
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Edit").clicked() {
                            self.edit_selected();
                            ui.close();
                        }
                        if ui.button("Reserved Edit").clicked() {
                            self.checked_out_edit();
                            ui.close();
                        }
                        if ui.button("Unedit").clicked() {
                            self.cvs_command(Cmd::Unedit);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Watch On").clicked() {
                            self.cvs_command(Cmd::WatchOn);
                            ui.close();
                        }
                        if ui.button("Watch Off").clicked() {
                            self.cvs_command(Cmd::WatchOff);
                            ui.close();
                        }
                    });
                });

                ui.menu_button("Query", |ui| {
                    if !in_project {
                        project_hint(ui);
                    }
                    ui.add_enabled_ui(in_project, |ui| {
                        if ui.button("Query Update").clicked() {
                            self.cvs_command(Cmd::QueryUpdate);
                            ui.close();
                        }
                        if ui.button("Diff").clicked() {
                            self.open_diff_dialog();
                            ui.close();
                        }
                        if ui
                            .add_enabled(history_ready, egui::Button::new("File History…"))
                            .on_hover_text("Select one local file to see its history")
                            .clicked()
                        {
                            self.history_selected();
                            ui.close();
                        }
                        if cmd_item(ui, &Cmd::Status) {
                            self.cvs_command(Cmd::Status);
                            ui.close();
                        }
                        if cmd_item(ui, &Cmd::Annotate) {
                            self.cvs_command(Cmd::Annotate);
                            ui.close();
                        }
                        if ui.button("Editors").clicked() {
                            self.cvs_command(Cmd::Editors);
                            ui.close();
                        }
                        if ui.button("Watchers").clicked() {
                            self.cvs_command(Cmd::Watchers);
                            ui.close();
                        }
                        if ui.button("Repository History…").clicked() {
                            self.cvs_command(Cmd::History);
                            ui.close();
                        }
                    });
                });

                ui.menu_button("Tag", |ui| {
                    if !in_project {
                        project_hint(ui);
                    }
                    ui.add_enabled_ui(in_project, |ui| {
                        if ui.button("Tags…").clicked() {
                            self.open_tags();
                            ui.close();
                        }
                    });
                });

                ui.menu_button("Advanced", |ui| {
                    if !in_project {
                        project_hint(ui);
                    }
                    ui.add_enabled_ui(in_project, |ui| {
                        if cmd_item(ui, &Cmd::Export) {
                            self.cvs_command(Cmd::Export);
                            ui.close();
                        }
                        if cmd_item(ui, &Cmd::Rdiff) {
                            self.cvs_command(Cmd::Rdiff);
                            ui.close();
                        }
                        if cmd_item(ui, &Cmd::Admin) {
                            self.cvs_command(Cmd::Admin);
                            ui.close();
                        }
                    });
                });

                ui.menu_button("Tools", |ui| {
                    if ui.button("Login…").clicked() {
                        self.open_dialog(Dialog::Login {
                            password: String::new(),
                            root: self.default_connection(),
                        });
                        ui.close();
                    }
                    if ui.button("Logout").clicked() {
                        self.cvs_command(Cmd::Logout);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Command Line…").clicked() {
                        self.open_dialog(Dialog::CommandLine {
                            text: "cvs ".to_string(),
                        });
                        ui.close();
                    }
                    if ui.button("Preferences…").clicked() {
                        self.open_dialog(Dialog::Preferences {
                            tab: 0,
                            draft: Box::new(self.prefs.clone()),
                        });
                        ui.close();
                    }
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("Open Log Folder").clicked() {
                        self.open_log_folder();
                        ui.close();
                    }
                    if ui.button("Copy Diagnostics").clicked() {
                        ui.ctx().copy_text(cvsdesk::diag::diagnostics_tail(120));
                        self.status = "Diagnostics copied to the clipboard".into();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("About").clicked() {
                        self.open_dialog(Dialog::About);
                        ui.close();
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.runner.running {
                        ui.label(
                            egui::RichText::new("cvs running")
                                .color(egui::Color32::from_rgb(180, 80, 0)),
                        );
                    }
                });
            });
        });
    }
}

/// A short line at the top of a menu whose commands need a working copy.
fn project_hint(ui: &mut egui::Ui) {
    ui.label(
        egui::RichText::new("This folder is not a CVS working copy.")
            .small()
            .weak(),
    );
    ui.separator();
}

/// A menu row for one CVS command, using the label `Cmd::menu` gives it.
/// Returns true when clicked.
fn cmd_item(ui: &mut egui::Ui, cmd: &Cmd) -> bool {
    ui.button(cmd.menu()).clicked()
}
