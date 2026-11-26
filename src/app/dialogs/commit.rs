//! Commit: the log message, or a file to read it from.

use cvsdesk::cvs::{Cmd, CommandRequest};

use super::{Dialog, DialogAction};
use crate::app::{App, OutputKind};

pub(super) fn show(
    ctx: &egui::Context,
    text: &mut String,
    file: &mut String,
    force: &mut bool,
    sel_names: &[String],
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Commit")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .show(ctx, |ui| {
            let names = sel_names;
            if names.is_empty() {
                ui.label(
                    egui::RichText::new("No files selected: the whole folder will be committed.")
                        .weak(),
                );
            } else {
                ui.label(format!("{} file(s) selected:", names.len()));
                ui.label(egui::RichText::new(names.join(", ")).small().weak());
            }
            ui.separator();
            ui.label("Log message:");
            ui.add(
                egui::TextEdit::multiline(text)
                    .desired_rows(6)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                ui.label("Or read the message from the file:");
                ui.text_edit_singleline(file);
            });
            ui.checkbox(force, "Commit even if the file has not changed");
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Commit").clicked() {
                    action = Some(DialogAction::Commit);
                }
                if ui.button("Cancel").clicked() {
                    action = Some(DialogAction::Close);
                }
            });
        });
    if !open {
        action = Some(DialogAction::Close);
    }
    action
}

pub(super) fn apply(app: &mut App, d: Dialog) {
    let Dialog::Commit { text, file, force } = d else {
        return;
    };
    let mut req = CommandRequest::new(Cmd::Commit);
    req.files = app.selected_local_names();
    req.message = text;
    req.message_file = file;
    req.force = force;
    app.start(req, OutputKind::Console);
}
