//! Preferences: the four tabs of settings, edited on a draft and saved on request.

use cvsdesk::prefs::Prefs;

use super::{Dialog, DialogAction};
use crate::app::ui::typography::small_monospace;
use crate::app::ui::wrap;
use crate::app::{App, Trouble};

pub(super) fn show(
    ctx: &egui::Context,
    tab: &mut usize,
    draft: &mut Prefs,
) -> Option<DialogAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Preferences")
        .open(&mut open)
        .drag_area(egui::WindowDrag::Anywhere)
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (i, name) in ["Repositories", "Programs", "Behavior", "Defaults"]
                    .iter()
                    .enumerate()
                {
                    if ui.selectable_label(*tab == i, *name).clicked() {
                        *tab = i;
                    }
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| match tab {
                    0 => {
                        ui.label(
                            "Every folder carries its own CVSROOT in CVS/Root. Commands \
                             running in a folder need no setting; this list only feeds the \
                             Checkout, Import and Login windows.",
                        );
                        ui.separator();
                        ui.label("Repositories used before:");
                        if draft.recent_roots.is_empty() {
                            ui.label(egui::RichText::new("None used yet.").small().weak());
                        } else {
                            let mut forget = None;
                            for (index, root) in draft.recent_roots.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    wrap::label_in(ui, root, small_monospace(), false);
                                    if ui.small_button("Forget").clicked() {
                                        forget = Some(index);
                                    }
                                });
                            }
                            if let Some(index) = forget {
                                draft.recent_roots.remove(index);
                            }
                        }
                    }
                    1 => {
                        ui.horizontal(|ui| {
                            ui.label("Viewer:");
                            ui.text_edit_singleline(&mut draft.viewer);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Helper:");
                            ui.text_edit_singleline(&mut draft.helper);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Browser:");
                            ui.text_edit_singleline(&mut draft.browser);
                        });
                        ui.horizontal(|ui| {
                            ui.label("External diff:");
                            ui.text_edit_singleline(&mut draft.extdiff);
                        });
                        ui.checkbox(&mut draft.word_wrap_log, "Wrap long log messages");
                    }
                    2 => {
                        ui.checkbox(&mut draft.quiet, "Quiet output (-q)");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut draft.z9, "Compress traffic (-z)");
                            ui.add(egui::DragValue::new(&mut draft.z_level).range(1..=9));
                        });
                        ui.checkbox(&mut draft.prune, "Prune empty folders (-P)");
                        ui.checkbox(&mut draft.iso_convert, "Convert text to ISO-8859-1");
                        ui.checkbox(&mut draft.checkout_ro, "Check out read-only");
                        ui.checkbox(&mut draft.unix_lf, "Store files with Unix line ends");
                    }
                    _ => {
                        ui.checkbox(&mut draft.unified, "Diff in unified format");
                        ui.checkbox(&mut draft.ignore_case, "Diff ignores case");
                        ui.checkbox(&mut draft.no_whitespace, "Diff ignores white space");
                        ui.checkbox(&mut draft.no_blank_lines, "Diff ignores blank lines");
                        ui.horizontal(|ui| {
                            ui.label("Extra diff options:");
                            ui.text_edit_singleline(&mut draft.diff_options);
                        });
                        ui.separator();
                        ui.label("Commit message template:");
                        ui.add(
                            egui::TextEdit::multiline(&mut draft.log_template)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY),
                        );
                        ui.separator();
                        ui.checkbox(
                            &mut draft.diff_ext,
                            "Use an external diff program instead of cvs diff",
                        );
                        ui.add_enabled(
                            draft.diff_ext,
                            egui::TextEdit::singleline(&mut draft.settings_folder)
                                .hint_text("folder the external diff program runs in"),
                        );
                        ui.checkbox(
                            &mut draft.keep_editors,
                            "Keep an editor open on added files (cvs edit)",
                        );
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    action = Some(DialogAction::SavePrefs);
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
    let Dialog::Preferences { draft, .. } = d else {
        return;
    };
    // The draft already holds the last root used; rebuilding it from the old connection
    // fields would overwrite that with a value the user no longer edits.
    let p = (*draft).clone();
    app.prefs = p;
    // The Preferences draft may have been open while the file list's filters changed;
    // keep the list's current view instead of restoring the draft's older one.
    app.sync_view_prefs();
    // Write them out beside the program, so the tag lists and the CVSROOT survive a restart.
    app.status = match app.prefs.save() {
        Ok(_) => {
            app.trouble = None;
            "Preferences saved".into()
        }
        Err(e) => {
            app.trouble = Some(Trouble::Message(format!("settings: {}", e)));
            "Preferences kept for this session only".into()
        }
    };
}
