//! The frame and the shared drawing helpers: the menu, the panes, the output pane,
//! and the status bar.

use cvsdesk::entries::State;

use crate::app::App;

pub(super) mod diff_view;
mod files;
mod menu;
mod output;
mod status;

pub(super) mod fonts;
mod icons;
pub(super) mod typography;
pub(super) mod wrap;

impl App {
    /// Draw the whole window: menu, file list, output pane and status bar.
    pub(super) fn ui_frame(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        self.ui_menu(ui, frame);
        self.ui_status_bar(ui);
        self.ui_output(ui);
        egui::CentralPanel::default().show(ui, |ui| {
            self.ui_file_list(ui);
        });
        self.ui_dialogs(ui.ctx());
    }
}

/// Run one headless pass in a test, dropping the font-atlas delta on purpose.
///
/// egui 0.37 panics when a `TexturesDelta` is dropped unapplied. A real integration
/// uploads it; these tests only read shapes, so there is nothing to upload.
#[cfg(test)]
pub(super) fn test_run(
    ctx: &egui::Context,
    input: egui::RawInput,
    mut add_contents: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut output = ctx.run_ui(input, |ui| add_contents(ui));
    output.textures_delta.clear();
    output
}

fn state_color(s: State) -> egui::Color32 {
    match s {
        State::Unchanged => egui::Color32::from_rgb(40, 40, 40),
        State::Modified => egui::Color32::from_rgb(160, 40, 40),
        State::Added => egui::Color32::from_rgb(20, 90, 170),
        State::Removed => egui::Color32::from_rgb(130, 130, 130),
        State::Conflict => egui::Color32::from_rgb(190, 20, 20),
        State::NeedsPatch => egui::Color32::from_rgb(170, 110, 0),
        State::Missing => egui::Color32::from_rgb(150, 90, 160),
        State::Unknown => egui::Color32::from_rgb(90, 90, 90),
        State::Folder => egui::Color32::from_rgb(30, 30, 90),
        State::Remote => egui::Color32::from_rgb(70, 100, 135),
    }
}
