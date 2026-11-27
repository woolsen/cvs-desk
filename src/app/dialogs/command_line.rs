//! Command Line: one cvs command typed as it would be at the console.

use cvsdesk::cvs::{self, CommandRequest};

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
    let parts = words(&text);
    if parts.is_empty() {
        app.status = "Enter a cvs command".into();
        return;
    }
    // The whole line goes to the project's own client as typed. The window does not
    // need to know every command: the engine reports an unknown one itself.
    let refresh = !parts.first().is_some_and(|first| reads_only(first));
    let mut req = CommandRequest::raw(parts);
    req.refresh = refresh;
    app.start(req, OutputKind::Console);
}

/// Commands that only read the repository or the folder, so the file list does not need
/// to be read again after them.
fn reads_only(command: &str) -> bool {
    matches!(
        command,
        "log"
            | "rlog"
            | "status"
            | "diff"
            | "rdiff"
            | "annotate"
            | "rannotate"
            | "editors"
            | "watchers"
            | "history"
            | "rtag"
            | "admin"
            | "login"
            | "logout"
            | "version"
            | "noop"
    )
}

/// The words after a typed `cvs` or `cvs.exe` program name. A quoted argument stays
/// one word, the same way the client's own parser reads it.
fn words(text: &str) -> Vec<String> {
    let text = text.trim();
    let command = ["cvs.exe", "cvs"]
        .iter()
        .find_map(|program| {
            let rest = text.strip_prefix(program)?;
            let separated = rest.chars().next().is_none_or(|c| c.is_whitespace());
            separated.then(|| rest.trim_start())
        })
        .unwrap_or(text);
    cvs::split_args(command).unwrap_or_default()
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

    /// The dialog hands the words to the project's own client as typed. Commands the
    /// window has no menu item for (`admin`, `rlog`, ...) must reach the engine too,
    /// instead of being rewritten into `status`.
    #[test]
    fn a_typed_line_reaches_the_engine_as_typed() {
        for (text, expected) in [
            ("cvs admin -kb a.txt", "$ cvs -q -z9 admin -kb a.txt"),
            ("cvs watch on a.txt", "$ cvs -q -z9 watch on a.txt"),
            ("cvs rlog mod", "$ cvs -q -z9 rlog mod"),
            ("cvs frobnicate", "$ cvs -q -z9 frobnicate"),
        ] {
            let mut app = app_with_rows(Vec::new());
            app.dialog = Some(Dialog::CommandLine {
                text: text.to_string(),
            });

            apply(
                &mut app,
                Dialog::CommandLine {
                    text: text.to_string(),
                },
            );

            assert_eq!(
                app.runner.lines.first().map(String::as_str),
                Some(expected),
                "{text}"
            );
        }
    }

    #[test]
    fn the_program_name_and_quoted_words_are_read_like_the_client_reads_them() {
        for (text, expected) in [
            ("cvs.exe log a.txt", "$ cvs -q -z9 log a.txt"),
            ("cvs\tstatus", "$ cvs -q -z9 status"),
            (
                "cvs commit -m \"first drop\"",
                "$ cvs -q -z9 commit -m \"first drop\"",
            ),
        ] {
            let mut app = app_with_rows(Vec::new());
            app.dialog = Some(Dialog::CommandLine {
                text: text.to_string(),
            });

            apply(
                &mut app,
                Dialog::CommandLine {
                    text: text.to_string(),
                },
            );

            assert_eq!(
                app.runner.lines.first().map(String::as_str),
                Some(expected),
                "{text}"
            );
        }
    }

    #[test]
    fn an_empty_line_does_not_start_a_command() {
        let mut app = app_with_rows(Vec::new());
        let text = "   ".to_string();
        app.dialog = Some(Dialog::CommandLine { text: text.clone() });

        apply(&mut app, Dialog::CommandLine { text });

        assert!(app.runner.lines.is_empty());
        assert!(!app.runner.running);
    }
}
