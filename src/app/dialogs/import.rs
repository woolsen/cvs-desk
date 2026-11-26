//! Import: the repository, a vendor branch, a release tag and the message that goes
//! with them.

use cvsdesk::cvs::{Cmd, CommandRequest};
use cvsdesk::prefs::Prefs;

use super::repository;
use super::{Dialog, DialogAction};
use crate::app::{App, OutputKind};

pub(super) fn show(
    ctx: &egui::Context,
    vendor: &mut String,
    release: &mut String,
    message: &mut String,
    module: &mut String,
    root: &mut String,
    recent: &[String],
    prefs: &Prefs,
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Import")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .show(ctx, |ui| {
            repository::show(ui, root, recent);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Repository (module):");
                ui.text_edit_singleline(module);
                // Modules imported before are offered again (P_ImportModuleNames).
                if !prefs.import_modules.is_empty() {
                    egui::ComboBox::from_id_salt("module_recent")
                        .selected_text("recent…")
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            for name in prefs.import_modules.clone() {
                                if ui.button(&name).clicked() {
                                    *module = name;
                                }
                            }
                        });
                }
            });
            ui.horizontal(|ui| {
                ui.label("Vendor tag:");
                ui.text_edit_singleline(vendor);
                // Tags used before are offered for reuse (P_VendorNames).
                if !prefs.vendor_names.is_empty() {
                    egui::ComboBox::from_id_salt("vendor_recent")
                        .selected_text("recent…")
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            for name in prefs.vendor_names.clone() {
                                if ui.button(&name).clicked() {
                                    *vendor = name;
                                }
                            }
                        });
                }
            });
            ui.horizontal(|ui| {
                ui.label("Release tag:");
                ui.text_edit_singleline(release);
                // The same for release tags (P_ReleaseNames).
                if !prefs.release_names.is_empty() {
                    egui::ComboBox::from_id_salt("release_recent")
                        .selected_text("recent…")
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            for name in prefs.release_names.clone() {
                                if ui.button(&name).clicked() {
                                    *release = name;
                                }
                            }
                        });
                }
            });
            // Keep the tags last used here and offer them again
            // (P_VendorNames / P_ReleaseNames).
            let same = vendor.trim() == release.trim() && !vendor.trim().is_empty();
            if same {
                ui.colored_label(
                    egui::Color32::from_rgb(180, 60, 60),
                    "Vendor Tag and Release Tag can't have the same values.",
                );
            }
            ui.label("Message:");
            ui.add(
                egui::TextEdit::multiline(message)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.separator();
            ui.horizontal(|ui| {
                let ok = !same && !vendor.trim().is_empty() && !release.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new("Import")).clicked() {
                    action = Some(DialogAction::Import);
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

/// The command the window's fields describe, without starting cvs.
fn request(vendor: &str, release: &str, message: &str, module: &str, root: &str) -> CommandRequest {
    let mut req = CommandRequest::new(Cmd::Import);
    let mut extra = Vec::new();
    if !message.trim().is_empty() {
        extra.push("-m".to_string());
        extra.push(message.trim().to_string());
    }
    req.extra = extra;
    let mut files = Vec::new();
    if !module.trim().is_empty() {
        files.push(module.trim().to_string());
    }
    if !vendor.trim().is_empty() {
        files.push(vendor.trim().to_string());
    }
    if !release.trim().is_empty() {
        files.push(release.trim().to_string());
    }
    req.files = files;
    req.root = Some(root.trim().to_string());
    req
}

pub(super) fn apply(app: &mut App, d: Dialog) {
    let Dialog::Import {
        vendor,
        release,
        message,
        module,
        root,
    } = d
    else {
        return;
    };
    app.prefs.remember_root(&root);
    // Remember what was used, so the Import window can offer it again.
    Prefs::remember(&mut app.prefs.import_modules, &module);
    Prefs::remember(&mut app.prefs.vendor_names, &vendor);
    Prefs::remember(&mut app.prefs.release_names, &release);
    app.start(
        request(&vendor, &release, &message, &module, &root),
        OutputKind::Console,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_runs_against_the_root_in_the_dialog() {
        let req = request("acme", "R1", "", "proj", ":pserver:ann@host:/repo");
        assert_eq!(req.root.as_deref(), Some(":pserver:ann@host:/repo"));
        assert_eq!(req.files, vec!["proj", "acme", "R1"]);
    }

    #[test]
    fn the_message_is_one_argument() {
        let req = request(
            "acme",
            "R1",
            " first drop ",
            "proj",
            ":pserver:ann@host:/repo",
        );
        assert_eq!(req.extra, vec!["-m".to_string(), "first drop".to_string()]);
        // The console still shows it quoted, as the old command line did.
        assert_eq!(
            req.to_line("cvs.exe", "", &[]),
            "cvs.exe import -m \"first drop\" proj acme R1"
        );
    }
}
