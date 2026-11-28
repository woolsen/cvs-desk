//! Login: the CVSROOT the console will log into, and the password to save for it.

use cvsdesk::cvs::{Cmd, CommandRequest};

use super::connection;
use super::repository;
use super::{Dialog, DialogAction};
use crate::app::{App, OutputKind};

pub(super) fn show(
    ctx: &egui::Context,
    password: &mut String,
    cvsroot: &mut String,
    recent: &[String],
    busy: bool,
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    let pserver = connection::is_pserver(cvsroot);
    let has_embedded = connection::split_root(cvsroot).1.is_some();
    let ready = !busy
        && pserver
        && !cvsroot.trim().is_empty()
        && (!password.trim().is_empty() || has_embedded);
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
                    .hint_text("CVS password, or a password in the repository string")
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
            if !cvsroot.trim().is_empty() && !pserver {
                ui.label(
                    egui::RichText::new("Login is only for :pserver: roots.")
                        .small()
                        .weak(),
                );
            }
            if busy {
                ui.label(
                    egui::RichText::new("cvs is already running; wait for it to finish.")
                        .small()
                        .weak(),
                );
            }
            ui.separator();
            ui.horizontal(|ui| {
                let button = ui.add_enabled(ready, egui::Button::new("Login"));
                let button = if busy {
                    button.on_disabled_hover_text("cvs is already running")
                } else if cvsroot.trim().is_empty() {
                    button.on_disabled_hover_text("Enter the repository")
                } else if !pserver {
                    button.on_disabled_hover_text("Login is only for :pserver: roots")
                } else if password.trim().is_empty() && !has_embedded {
                    button.on_disabled_hover_text("Enter the password")
                } else {
                    button
                };
                if button.clicked() {
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
    let (root, password) = connection::password_for(root, password);
    let mut req = CommandRequest::new(Cmd::Login);
    req.password = password;
    // Named explicitly: a login has to work before a working copy exists, when there is
    // no `CVS/Root` to read a root from.
    req.root = Some(root);
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
    fn a_pasted_root_password_is_used_and_kept_out_of_the_request() {
        let req = request("", ":pserver:ann:secret@host:/repo");
        assert_eq!(req.root.as_deref(), Some(":pserver:ann@host:/repo"));
        assert_eq!(req.password.as_deref(), Some("secret"));
    }

    #[test]
    fn login_runs_against_the_root_in_the_dialog() {
        let req = request("secret", ":pserver:ann@host:/repo");
        assert_eq!(req.root.as_deref(), Some(":pserver:ann@host:/repo"));
        assert_eq!(req.password.as_deref(), Some("secret"));
    }
}
