//! The status bar: the last command, the counts and the last trouble.

use crate::app::{App, RemoteState, Trouble};

use super::state_color;

impl App {
    pub(super) fn ui_status_bar(&mut self, ui: &mut egui::Ui) {
        // The status bar is drawn before the central panel, so it must make the view
        // order current itself; otherwise the first frame after a read shows the old
        // count.
        self.ensure_order();
        let mut stop_remote = false;
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.loading.is_some() {
                    ui.label(egui::RichText::new("Reading folder…").weak());
                } else {
                    ui.label(
                        egui::RichText::new(format!(
                            "Showing {} of {}",
                            self.order.len(),
                            self.rows.len()
                        ))
                        .weak(),
                    );
                    ui.separator();
                    ui.label(
                        egui::RichText::new(self.remote_status_label())
                            .small()
                            .weak(),
                    )
                    .on_hover_text("Repository names are read with rlog -R for this folder only.");
                    if self.remote_state == RemoteState::Loading
                        && ui
                            .small_button("Stop")
                            .on_hover_text("Stop reading the server folder list")
                            .clicked()
                    {
                        stop_remote = true;
                    }
                    ui.separator();
                    let counts = &self.counts;
                    if counts.is_empty() {
                        ui.label(egui::RichText::new("No local changes").weak());
                    } else {
                        for (s, n) in counts {
                            ui.label(
                                egui::RichText::new(format!("{}: {}", s.label(), n))
                                    .color(state_color(*s)),
                            );
                        }
                    }
                }
                let hidden = self.filters.filtering();
                if !hidden.is_empty() {
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!("hidden: {}", hidden.join(", ")))
                            .small()
                            .weak(),
                    )
                    .on_hover_text("Hidden by the current view, the Show menu or the scope bar.");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(t) = &self.trouble {
                        let colour = match t {
                            Trouble::Busy => egui::Color32::from_rgb(190, 120, 0),
                            Trouble::Exit(_) | Trouble::Message(_) => {
                                egui::Color32::from_rgb(190, 60, 60)
                            }
                        };
                        let text = t.text();
                        let label = ui.label(egui::RichText::new(&text).color(colour));
                        if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                            self.trouble = None;
                        }
                        label.on_hover_text("This is also written into the console.");
                    }
                    ui.label(egui::RichText::new(&self.status).weak());
                    if !self.root.is_empty() {
                        ui.add(egui::Label::new(egui::RichText::new(&self.root).weak()).truncate())
                            .on_hover_text(&self.root);
                    } else if self.loading.is_none() {
                        ui.label(egui::RichText::new("Not a CVS working copy").weak());
                    }
                });
            });
        });
        if stop_remote {
            self.stop_remote_listing();
        }
    }
}
