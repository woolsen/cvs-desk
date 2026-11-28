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
    folder: &mut String,
    root: &mut String,
    recent: &[String],
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Checkout")
        .open(&mut open)
        .drag_area(egui::WindowDrag::Anywhere)
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .show(ctx, |ui| {
            repository::show(ui, root, recent);
            ui.separator();
            ui.label("Checkout a module into a folder.");
            ui.horizontal(|ui| {
                ui.label("Module:");
                ui.text_edit_singleline(module);
            });
            ui.horizontal(|ui| {
                ui.label("Tag / branch:");
                ui.text_edit_singleline(tag);
            });
            ui.horizontal(|ui| {
                ui.label("Folder:");
                ui.add(
                    egui::TextEdit::singleline(folder)
                        .hint_text("Empty: the module name under this folder")
                        .desired_width(300.0),
                );
                if ui
                    .button("Choose parent…")
                    .on_hover_text("Pick the parent folder; the module name is added to it")
                    .clicked()
                {
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("Parent folder for the checkout")
                        .pick_folder()
                    {
                        *folder = match module_leaf(module.as_str()) {
                            Some(name) => dir.join(name).display().to_string(),
                            None => dir.display().to_string(),
                        };
                    }
                }
            });
            ui.label(
                egui::RichText::new(
                    "The folder may be absolute or relative to this one, and is created \
                     if it is not there yet.",
                )
                .small()
                .weak(),
            );
            ui.checkbox(readonly, "Check out read-only");
            ui.separator();
            ui.horizontal(|ui| {
                let ready = module_leaf(module.as_str()).is_some();
                if ui
                    .add_enabled(ready, egui::Button::new("Checkout"))
                    .clicked()
                {
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
fn request(
    tag: &str,
    readonly: bool,
    module: &str,
    destination: &str,
    root: &str,
) -> CommandRequest {
    let mut req = CommandRequest::new(Cmd::Checkout);
    req.tag = if tag.trim().is_empty() {
        String::new()
    } else {
        tag.trim().to_string()
    };
    req.readonly = readonly;
    req.destination = destination.trim().to_string();
    if !module.trim().is_empty() {
        req.files = vec![module.trim().to_string()];
    }
    req.root = Some(root.trim().to_string());
    req
}

/// Where a checkout writes: the folder to run in, the folder cvs creates, and the name
/// handed to `checkout -d`.
#[derive(Debug, PartialEq, Eq)]
struct CheckoutTarget {
    run_dir: PathBuf,
    dir: PathBuf,
    name: String,
}

/// Resolve the dialog's module and folder fields. An empty folder means the module's own
/// name under the folder the window shows; a folder may be absolute or relative to it.
/// `checkout -d` takes a name beside the process, so the command runs in the target's
/// parent and names only its last part.
fn checkout_target(base: &Path, module: &str, folder: &str) -> Option<CheckoutTarget> {
    let module = module_leaf(module)?;
    let folder = folder.trim();
    let target = if folder.is_empty() {
        base.join(module)
    } else {
        let path = PathBuf::from(folder);
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    };
    let target = crate::app::nav::normalize(&target);
    if target == base {
        return None;
    }
    let name = target.file_name()?.to_string_lossy().into_owned();
    let run_dir = target.parent()?.to_path_buf();
    Some(CheckoutTarget {
        run_dir,
        dir: target,
        name,
    })
}

/// The last part of a module path: `tools/proj` is checked out as `proj`.
fn module_leaf(module: &str) -> Option<&str> {
    module
        .trim()
        .rsplit(['/', '\\'])
        .find(|part| !part.trim().is_empty())
        .map(str::trim)
}

pub(super) fn apply(app: &mut App, d: Dialog) {
    let Dialog::Checkout {
        tag,
        readonly,
        module,
        folder,
        root,
    } = d
    else {
        return;
    };
    app.prefs.remember_root(&root);
    let Some(target) = checkout_target(&app.dir, &module, &folder) else {
        app.status = "Enter a module, and a folder other than the one the window shows".into();
        return;
    };
    if target.run_dir != app.dir {
        if let Err(error) = std::fs::create_dir_all(&target.run_dir) {
            app.status = format!("Could not create {}: {error}", target.run_dir.display());
            return;
        }
    }
    let started = app.start_in(
        target.run_dir,
        request(&tag, readonly, &module, &target.name, &root),
        OutputKind::Console,
    );
    if started {
        app.checkout_waiting = Some(target.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_runs_against_the_root_in_the_dialog() {
        let req = request("HEAD", false, "proj", "", ":pserver:ann@host:/repo");
        assert_eq!(req.root.as_deref(), Some(":pserver:ann@host:/repo"));
        assert_eq!(req.files, vec!["proj"]);
        assert_eq!(req.tag, "HEAD", "the dialog's default tag is kept");
    }

    #[test]
    fn a_named_tag_and_readonly_survive() {
        let req = request("Release_1_0", true, " proj ", "", ":pserver:ann@host:/repo");
        assert_eq!(req.tag, "Release_1_0");
        assert!(req.readonly);
        assert_eq!(req.files, vec!["proj"], "the module is trimmed");
    }

    #[test]
    fn the_folder_is_the_destination_checkout_is_given() {
        let req = request("HEAD", false, "proj", " work ", ":pserver:ann@host:/repo");
        assert_eq!(req.destination, "work");
        assert_eq!(
            req.to_args("", &[]),
            vec!["checkout", "-r", "HEAD", "-d", "work", "proj"]
        );
    }

    #[test]
    fn an_empty_folder_checks_out_the_module_under_this_folder() {
        let base = Path::new("C:\\work");
        let target = checkout_target(base, "proj", "").unwrap();
        assert_eq!(target.run_dir, base);
        assert_eq!(target.dir, base.join("proj"));
        assert_eq!(target.name, "proj");

        let target = checkout_target(base, "tools/proj", "  ").unwrap();
        assert_eq!(target.run_dir, base);
        assert_eq!(
            target.dir,
            base.join("proj"),
            "cvs names the folder after the module's last part"
        );
    }

    #[test]
    fn a_folder_name_under_this_folder_is_the_destination() {
        let base = Path::new("C:\\work");
        let target = checkout_target(base, "proj", "custom").unwrap();
        assert_eq!(target.run_dir, base);
        assert_eq!(target.dir, base.join("custom"));
        assert_eq!(target.name, "custom");

        let target = checkout_target(base, "proj", "sub/custom").unwrap();
        assert_eq!(target.run_dir, base.join("sub"));
        assert_eq!(target.dir, base.join("sub").join("custom"));
        assert_eq!(target.name, "custom");
    }

    #[cfg(windows)]
    #[test]
    fn an_absolute_folder_runs_in_its_parent() {
        let base = Path::new("C:\\work");
        let target = checkout_target(base, "proj", "D:\\out\\custom").unwrap();
        assert_eq!(target.run_dir, Path::new("D:\\out"));
        assert_eq!(target.dir, Path::new("D:\\out\\custom"));
        assert_eq!(target.name, "custom");
    }

    #[test]
    fn a_missing_module_or_the_current_folder_is_no_target() {
        let base = Path::new("C:\\work");
        assert!(checkout_target(base, "  ", "").is_none());
        assert!(
            checkout_target(base, "proj", ".").is_none(),
            "the working copy cannot be written into the folder already on screen"
        );
    }
}
