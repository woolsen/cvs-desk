//! The frame and the shared drawing helpers: the menu, the panes, the output pane,
//! and the status bar.

use cvsdesk::entries::State;

use crate::app::App;

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
    pub(super) fn ui_frame(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        self.ui_menu(ctx, frame);
        self.ui_status_bar(ctx);
        self.ui_output(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_file_list(ui);
        });
        self.ui_dialogs(ctx);
    }
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
