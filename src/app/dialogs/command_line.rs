//! Command Line: one cvs command typed as it would be at the console.

use cvsdesk::cvs::{self, Cmd, CommandRequest};

use super::{Dialog, DialogAction};
use crate::app::ui::{typography, wrap};
use crate::app::{App, OutputKind};

pub(super) fn show(ctx: &egui::Context, text: &mut String, cvsroot: &str) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Command Line")
        .open(&mut open)
        .drag_area(egui::WindowDrag::Anywhere)
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .show(ctx, |ui| {
            ui.label("Run a cvs command in this folder:");
            ui.add(
                egui::TextEdit::multiline(text)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            );
            if !cvsroot.is_empty() {
                wrap::label_in(
                    ui,
                    &format!("-d {} and the switches above are added for you", cvsroot),
                    typography::small(),
                    true,
                );
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Run").clicked() {
                    action = Some(DialogAction::RunLine);
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
    let Dialog::CommandLine { text } = d else {
        return;
    };
    let raw = text.trim().to_string();
    let cmd = raw
        .strip_prefix("cvs ")
        .or_else(|| raw.strip_prefix("cvs.exe "))
        .unwrap_or(&raw)
        .to_string();
    let mut req = CommandRequest::new(Cmd::Status);
    // The whole line is passed through as-is after the program name.
    let parts = cvs::split_args(&cmd).unwrap_or_default();
    if let Some(first) = parts.first() {
        // A typed line may name a command this window does not know, which can still
        // write to the working copy, so the list is read again afterwards. The words
        // below are the queries: they only read the repository and the folder.
        req.refresh = !matches!(
            first.as_str(),
            "log"
                | "status"
                | "diff"
                | "annotate"
                | "editors"
                | "watchers"
                | "history"
                | "rdiff"
                | "rtag"
                | "admin"
                | "login"
                | "logout"
        );
        req.cmd = match first.as_str() {
            "update" => Cmd::Update,
            "commit" => Cmd::Commit,
            "add" => Cmd::Add,
            "remove" => Cmd::Remove,
            "diff" => Cmd::Diff,
            "log" => Cmd::Log,
            "tag" => Cmd::Tag,
            "rtag" => Cmd::Rtag,
            "checkout" => Cmd::Checkout,
            "import" => Cmd::Import,
            "init" => Cmd::Init,
            _ => Cmd::Status,
        };
        req.extra = parts.iter().skip(1).cloned().collect();
    }
    app.start(req, OutputKind::Console);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::dialogs::Dialog;
    use crate::app::tests::app_with_rows;

    #[test]
    fn a_typed_rtag_line_stays_rtag() {
        let mut app = app_with_rows(Vec::new());
        let text = "cvs rtag T1 mod".to_string();
        app.dialog = Some(Dialog::CommandLine { text: text.clone() });

        apply(&mut app, Dialog::CommandLine { text });

        assert_eq!(
            app.runner.lines.first().map(String::as_str),
            Some("$ cvs -q -z9 rtag T1 mod")
        );
    }
}
