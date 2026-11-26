//! Checkout: the repository, a module, an optional tag, and whether to check it out
//! read-only.

use std::path::{Path, PathBuf};

use cvsdesk::cvs::{Cmd, CommandRequest};

use super::repository;
use super::{Dialog, DialogAction};
use crate::app::{App, OutputKind};

pub(super) fn show(
    ctx: &egui::Context,
    tag: &mut String,
    readonly: &mut bool,
    module: &mut String,
    root: &mut String,
    recent: &[String],
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Checkout")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .show(ctx, |ui| {
            repository::show(ui, root, recent);
            ui.separator();
            ui.label("Checkout a module into this folder.");
            ui.horizontal(|ui| {
                ui.label("Module:");
                ui.text_edit_singleline(module);
            });
            ui.horizontal(|ui| {
                ui.label("Tag / branch:");
                ui.text_edit_singleline(tag);
            });
            ui.checkbox(readonly, "Check out read-only");
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Checkout").clicked() {
                    action = Some(DialogAction::Checkout);
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

/// The command the window's fields describe. Kept apart from `apply` so the root the
/// dialog names can be tested without starting cvs.
fn request(tag: &str, readonly: bool, module: &str, root: &str) -> CommandRequest {
    let mut req = CommandRequest::new(Cmd::Checkout);
    req.tag = if tag.trim().is_empty() {
        String::new()
    } else {
        tag.trim().to_string()
    };
    req.readonly = readonly;
    if !module.trim().is_empty() {
        req.files = vec![module.trim().to_string()];
    }
    req.root = Some(root.trim().to_string());
    req
}

/// The folder `cvs checkout <module>` creates under `dir`. A nested module path is
/// checked out under its last part, which is also the folder to open afterwards.
fn checkout_folder(dir: &Path, module: &str) -> Option<PathBuf> {
    let name = module
        .trim()
        .rsplit(['/', '\\'])
        .find(|part| !part.trim().is_empty())?;
    Some(dir.join(name.trim()))
}

pub(super) fn apply(app: &mut App, d: Dialog) {
    let Dialog::Checkout {
        tag,
        readonly,
        module,
        root,
    } = d
    else {
        return;
    };
    app.prefs.remember_root(&root);
    let created = checkout_folder(&app.dir, &module);
    let started = app.start(request(&tag, readonly, &module, &root), OutputKind::Console);
    if started {
        app.checkout_waiting = created;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_runs_against_the_root_in_the_dialog() {
        let req = request("HEAD", false, "proj", ":pserver:ann@host:/repo");
        assert_eq!(req.root.as_deref(), Some(":pserver:ann@host:/repo"));
        assert_eq!(req.files, vec!["proj"]);
        assert_eq!(req.tag, "HEAD", "the dialog's default tag is kept");
    }

    #[test]
    fn a_named_tag_and_readonly_survive() {
        let req = request("Release_1_0", true, " proj ", ":pserver:ann@host:/repo");
        assert_eq!(req.tag, "Release_1_0");
        assert!(req.readonly);
        assert_eq!(req.files, vec!["proj"], "the module is trimmed");
    }

    #[test]
    fn checkout_opens_the_folder_named_after_the_module() {
        assert_eq!(
            checkout_folder(std::path::Path::new("C:\\work"), "proj"),
            Some(std::path::PathBuf::from("C:\\work\\proj"))
        );
        assert_eq!(
            checkout_folder(std::path::Path::new("C:\\work"), "tools/proj"),
            Some(std::path::PathBuf::from("C:\\work\\proj")),
            "cvs names the folder after the last part of the module path"
        );
        assert_eq!(
            checkout_folder(std::path::Path::new("C:\\work"), "  "),
            None
        );
        assert_eq!(checkout_folder(std::path::Path::new("C:\\work"), ""), None);
    }
}
