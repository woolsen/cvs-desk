//! About: what this tool is, its version and its license.

use super::DialogAction;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const COPYRIGHT: &str = "Copyright (C) 2026 woolsen";
const LICENSE: &str = "GNU AGPL v3.0 only:";
const LICENSE_URL: &str = "https://www.gnu.org/licenses/agpl-3.0.html";

pub(super) fn show(ctx: &egui::Context) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("About")
        .open(&mut open)
        .drag_area(egui::WindowDrag::Anywhere)
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new("CVS Desk").strong());
            ui.label(format!("Version {}", VERSION));
            ui.label("A desktop front end for CVS (version control).");
            ui.label(COPYRIGHT);
            ui.horizontal_wrapped(|ui| {
                ui.label(LICENSE);
                ui.hyperlink_to("View license", LICENSE_URL);
            });
            ui.separator();
            if ui.button("Close").clicked() {
                action = Some(DialogAction::Close);
            }
        });
    if !open {
        action = Some(DialogAction::Close);
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_about_box_carries_the_license_notice() {
        assert!(COPYRIGHT.contains("woolsen"));
        assert!(LICENSE.contains("AGPL"));
        assert!(LICENSE_URL.contains("agpl-3.0"));
    }

    #[test]
    fn the_about_box_carries_a_version() {
        assert!(VERSION.split('.').count() >= 2);
    }
}
