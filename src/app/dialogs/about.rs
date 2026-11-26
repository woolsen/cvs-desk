//! About: the version, the CVSROOT and the folder being shown.

use super::DialogAction;
use crate::app::ui::wrap;

const COPYRIGHT: &str = "Copyright (C) 2026 woolsen";
const LICENSE: &str = "Licensed under the GNU AGPL v3.0 only. No warranty.";
const LICENSE_URL: &str = "https://www.gnu.org/licenses/agpl-3.0.html";

pub(super) fn show(
    ctx: &egui::Context,
    cvsroot: &str,
    kind_label: &str,
    repository: &str,
    dir: &str,
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("About")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new("CVS Desk").strong());
            ui.label("A desktop front end for CVS (version control).");
            ui.label(COPYRIGHT);
            ui.horizontal_wrapped(|ui| {
                ui.label(LICENSE);
                ui.hyperlink_to("View license", LICENSE_URL);
            });
            ui.separator();
            wrap::label(ui, &root_line(cvsroot, kind_label));
            if !repository.is_empty() {
                wrap::label(ui, &format!("Repository: {}", repository));
            }
            wrap::label(ui, &format!("Working folder: {}", dir));
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("File states").clicked() {
                    action = Some(DialogAction::ShowSheet);
                }
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

/// What the window says about the root. Empty means the folder is not a working copy;
/// the root itself is read from that folder's `CVS/Root`, never from a setting.
fn root_line(root: &str, kind_label: &str) -> String {
    if root.trim().is_empty() {
        "CVSROOT: (not a CVS working copy)".to_string()
    } else {
        format!("CVSROOT: {} ({})", root, kind_label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_about_line_names_the_folders_root() {
        assert_eq!(
            root_line(":pserver:ann@host:/repo", "pserver"),
            "CVSROOT: :pserver:ann@host:/repo (pserver)"
        );
    }

    #[test]
    fn a_folder_without_a_root_is_not_a_working_copy() {
        assert_eq!(root_line("", ""), "CVSROOT: (not a CVS working copy)");
    }

    #[test]
    fn the_about_box_carries_the_license_notice() {
        assert!(COPYRIGHT.contains("woolsen"));
        assert!(LICENSE.contains("AGPL"));
        assert!(LICENSE_URL.starts_with("https://"));
    }
}
