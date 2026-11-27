//! Login: the CVSROOT the console will log into, and the password to save for it.

use cvsdesk::cvs::{Cmd, CommandRequest};

use super::repository;
use super::{Dialog, DialogAction};
use crate::app::{App, OutputKind};

pub(super) fn show(
    ctx: &egui::Context,
    password: &mut String,
    cvsroot: &mut String,
    recent: &[String],
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Login")
        .open(&mut open)
        .drag_area(egui::WindowDrag::Anywhere)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .show(ctx, |ui| {
            ui.label("Log in to the CVS server with this CVSROOT:");
            repository::show(ui, cvsroot, recent);
            ui.separator();
            ui.label("Password:");
            ui.add(
                egui::TextEdit::singleline(password)
                    .password(true)
                    .hint_text("CVS password")
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                egui::RichText::new(
                    "The password is handed to cvs on its input, and saved by cvs in \
                     .cvspass, not by this window.",
                )
                .small()
                .weak(),
            );
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Login").clicked() {
                    action = Some(DialogAction::Login);
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
fn request(password: &str, root: &str) -> CommandRequest {
    let mut req = CommandRequest::new(Cmd::Login);
    req.password = Some(password.to_string());
    // Named explicitly: a login has to work before a working copy exists, when there is
    // no `CVS/Root` to read a root from.
    req.root = Some(root.trim().to_string());
    req
}

pub(super) fn apply(app: &mut App, d: Dialog) {
    let Dialog::Login { password, root } = d else {
        return;
    };
    app.prefs.remember_root(&root);
    app.start(request(&password, &root), OutputKind::Console);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_runs_against_the_root_in_the_dialog() {
        let req = request("secret", ":pserver:ann@host:/repo");
        assert_eq!(req.root.as_deref(), Some(":pserver:ann@host:/repo"));
        assert_eq!(req.password.as_deref(), Some("secret"));
    }
}
