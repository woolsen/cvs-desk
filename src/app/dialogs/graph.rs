//! The revision graph window: one file's trunk and branches, drawn from its `cvs log`.

use cvsdesk::graph::{self, RevNode};

use super::DialogAction;
use crate::app::ui::typography;

pub(super) fn show(ctx: &egui::Context, file: &str, nodes: &[RevNode]) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new(format!("Graph — {}", file))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(680.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(graph::summary(nodes)));
                ui.separator();
                for (name, colour) in graph::legend() {
                    ui.label(egui::RichText::new(format!("● {}", name)).color(colour));
                }
            });
            ui.separator();
            graph::draw(ui, nodes, 360.0, typography::LIST_ROW_H);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Close").clicked() {
                    action = Some(DialogAction::Close);
                }
            });
        });
    if !open {
        action = Some(DialogAction::Close);
    }
    action
}
