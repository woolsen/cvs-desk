//! The status bar: the last command, the counts and the last trouble.

use crate::app::{App, Trouble};

use super::state_color;

impl App {
    pub(super) fn ui_status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.loading.is_some() {
                    ui.label(egui::RichText::new("Reading folder…").weak());
                } else {
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
                    .on_hover_text("Unticked in View > Show, so these rows are not listed.");
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
    }
}
