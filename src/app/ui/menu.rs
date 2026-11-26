//! The menu bar.

use cvsdesk::cvs::{Cmd, CommandRequest};

use crate::app::dialogs::Dialog;
use crate::app::files::SortKey;
use crate::app::{App, OutputKind};

impl App {
    // -- ui -----------------------------------------------------------------

    pub(super) fn ui_menu(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // A folder without a `CVS/Root` has nothing for these commands to act on. They
        // stay visible but greyed, so the reason is on screen instead of in an error.
        let in_project = !self.root.is_empty();
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open folder…").clicked() {
                        self.open_folder(frame);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Checkout…").clicked() {
                        self.open_dialog(Dialog::Checkout {
                            tag: "HEAD".into(),
                            readonly: self.prefs.checkout_ro,
                            module: String::new(),
                            root: self.default_connection(),
                        });
                        ui.close_menu();
                    }
                    if ui.button("Import…").clicked() {
                        self.open_dialog(Dialog::Import {
                            vendor: String::new(),
                            release: String::new(),
                            message: String::new(),
                            module: String::new(),
                            root: self.default_connection(),
                        });
                        ui.close_menu();
                    }
                    if ui.button("Init").clicked() {
                        let mut req = CommandRequest::new(Cmd::Init);
                        req.files.clear();
                        self.start(req, OutputKind::Console);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                ui.menu_button("View", |ui| {
                    if ui
                        .add_enabled(self.history.can_back(), egui::Button::new("Back"))
                        .clicked()
                    {
                        self.go_back();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(self.history.can_forward(), egui::Button::new("Forward"))
                        .clicked()
                    {
                        self.go_forward();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(self.can_go_up(), egui::Button::new("Up"))
                        .clicked()
                    {
                        self.go_up();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Reload view").clicked() {
                        self.reload();
                        self.output = OutputKind::Console;
                        ui.close_menu();
                    }
                    if ui.button("File states…").clicked() {
                        self.refresh_state_sheet();
                        self.output = OutputKind::State;
                        ui.close_menu();
                    }
                    if ui.button("Explore").clicked() {
                        self.explore();
                        ui.close_menu();
                    }
                    if ui.button("Search this folder").clicked() {
                        self.open_search();
                        ui.close_menu();
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
                        changed |= ui.checkbox(&mut f.unknown, "Not in project").changed();
                        changed |= ui
                            .checkbox(&mut f.hide_unknown, "Hide files not in project")
                            .changed();
                        changed |= ui.checkbox(&mut f.missing, "Missing").changed();
                        changed |= ui
                            .checkbox(&mut f.hide_missing, "Hide missing files")
                            .changed();
                        changed |= ui.checkbox(&mut f.folders, "Sub-folders").changed();
                        if changed {
                            self.order_dirty = true;
                        }
                    });
                    ui.menu_button("Sort by", |ui| {
                        for k in [
                            SortKey::Name,
                            SortKey::State,
                            SortKey::Revision,
                            SortKey::Tag,
                            SortKey::Date,
                        ] {
                            let label = if self.sort == k {
                                format!("{} {}", k.label(), if self.sort_asc { "↑" } else { "↓" })
                            } else {
                                k.label().to_owned()
                            };
                            if ui.radio(self.sort == k, label).clicked() {
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
                            ui.close_menu();
                        }
                        if ui
                            .button("Update All (This Folder)")
                            .on_hover_text(
                                "Update this folder and its subfolders, whatever is selected",
                            )
                            .clicked()
                        {
                            self.update_whole_folder();
                            ui.close_menu();
                        }
                        if ui.button("Commit…").clicked() {
                            let template = self.prefs.log_template.clone();
                            self.open_commit_dialog(template);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Add").clicked() {
                            self.cvs_command(Cmd::Add);
                            ui.close_menu();
                        }
                        if ui.button("Add Binary").clicked() {
                            self.cvs_command(Cmd::AddBinary);
                            ui.close_menu();
                        }
                        if ui.button("Add Unicode").clicked() {
                            self.cvs_command(Cmd::AddUnicode);
                            ui.close_menu();
                        }
                        if ui.button("Remove").clicked() {
                            self.cvs_command(Cmd::Remove);
                            ui.close_menu();
                        }
                        if ui.button("Move To Trash").clicked() {
                            self.move_to_trash();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Edit").clicked() {
                            self.edit_selected();
                            ui.close_menu();
                        }
                        if ui.button("Reserved Edit").clicked() {
                            self.checked_out_edit();
                            ui.close_menu();
                        }
                        if ui.button("Unedit").clicked() {
                            self.cvs_command(Cmd::Unedit);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Watch On").clicked() {
                            self.cvs_command(Cmd::WatchOn);
                            ui.close_menu();
                        }
                        if ui.button("Watch Off").clicked() {
                            self.cvs_command(Cmd::WatchOff);
                            ui.close_menu();
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
                            ui.close_menu();
                        }
                        if ui.button("Diff").clicked() {
                            self.open_diff_dialog();
                            ui.close_menu();
                        }
                        if cmd_item(ui, &Cmd::Log) {
                            self.cvs_command(Cmd::Log);
                            ui.close_menu();
                        }
                        if cmd_item(ui, &Cmd::Status) {
                            self.cvs_command(Cmd::Status);
                            ui.close_menu();
                        }
                        if cmd_item(ui, &Cmd::Annotate) {
                            self.cvs_command(Cmd::Annotate);
                            ui.close_menu();
                        }
                        if ui.button("Editors").clicked() {
                            self.cvs_command(Cmd::Editors);
                            ui.close_menu();
                        }
                        if ui.button("Watchers").clicked() {
                            self.cvs_command(Cmd::Watchers);
                            ui.close_menu();
                        }
                        if ui.button("History…").clicked() {
                            self.cvs_command(Cmd::History);
                            ui.close_menu();
                        }
                        if ui.button("Graph…").clicked() {
                            self.graph_selected();
                            ui.close_menu();
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
                            ui.close_menu();
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
                            ui.close_menu();
                        }
                        if cmd_item(ui, &Cmd::Rdiff) {
                            self.cvs_command(Cmd::Rdiff);
                            ui.close_menu();
                        }
                        if cmd_item(ui, &Cmd::Admin) {
                            self.cvs_command(Cmd::Admin);
                            ui.close_menu();
                        }
                    });
                });

                ui.menu_button("Tools", |ui| {
                    if ui.button("Login…").clicked() {
                        self.open_dialog(Dialog::Login {
                            password: String::new(),
                            root: self.default_connection(),
                        });
                        ui.close_menu();
                    }
                    if ui.button("Logout").clicked() {
                        self.cvs_command(Cmd::Logout);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Command Line…").clicked() {
                        self.open_dialog(Dialog::CommandLine {
                            text: "cvs ".to_string(),
                        });
                        ui.close_menu();
                    }
                    if ui.button("Preferences…").clicked() {
                        self.open_dialog(Dialog::Preferences {
                            tab: 0,
                            draft: Box::new(self.prefs.clone()),
                        });
                        ui.close_menu();
                    }
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        self.open_dialog(Dialog::About);
                        ui.close_menu();
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.runner.running {
                        ui.label(
                            egui::RichText::new("cvs running")
                                .color(egui::Color32::from_rgb(180, 80, 0)),
                        );
                    }
                    if let Some(kind) = self.folder_kind() {
                        ui.label(egui::RichText::new(kind).weak());
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
