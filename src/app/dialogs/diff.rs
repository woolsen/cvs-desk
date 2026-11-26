//! Diff settings: the flags the next diff is sent with.

use cvsdesk::cvs::{Cmd, CommandRequest};

use super::{Dialog, DialogAction};
use crate::app::{App, OutputKind};

pub(super) fn show(
    ctx: &egui::Context,
    unified: &mut bool,
    ignore_case: &mut bool,
    no_whitespace: &mut bool,
    no_blank_lines: &mut bool,
    extra: &mut String,
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Diff settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(400.0)
        .show(ctx, |ui| {
            ui.checkbox(unified, "Unified format (-u)");
            ui.checkbox(ignore_case, "Ignore case (-i)");
            ui.checkbox(no_whitespace, "Ignore white space (-w)");
            ui.checkbox(no_blank_lines, "Ignore blank lines (-B)");
            ui.horizontal(|ui| {
                ui.label("More options:");
                ui.text_edit_singleline(extra);
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Diff").clicked() {
                    action = Some(DialogAction::Diff);
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
    let Dialog::Diff {
        unified,
        ignore_case,
        no_whitespace,
        no_blank_lines,
        extra,
    } = d
    else {
        return;
    };
    app.prefs.unified = unified;
    app.prefs.ignore_case = ignore_case;
    app.prefs.no_whitespace = no_whitespace;
    app.prefs.no_blank_lines = no_blank_lines;
    app.prefs.diff_options = extra;
    let mut req = CommandRequest::new(Cmd::Diff);
    req.files = app.selected_local_names();
    req.extra = app.prefs.diff_flags();
    app.start(req, OutputKind::Diff);
}
