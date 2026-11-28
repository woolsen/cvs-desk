//! Compare with: the revision or tag the next diff runs against.

use super::{Dialog, DialogAction};
use crate::app::App;

pub(super) fn show(ctx: &egui::Context, revision: &mut String) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Compare with Revision or Tag")
        .open(&mut open)
        .drag_area(egui::WindowDrag::Anywhere)
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.label("Compare the selection with:");
            ui.add(
                egui::TextEdit::singleline(revision)
                    .hint_text("1.4 or REL_1_0")
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                egui::RichText::new("cvs reads a revision number and a tag the same way here.")
                    .small()
                    .weak(),
            );
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!revision.trim().is_empty(), egui::Button::new("Compare"))
                    .clicked()
                {
                    action = Some(DialogAction::Compare);
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
    let Dialog::Compare { revision } = d else {
        return;
    };
    app.diff_with(&revision);
}
