//! The little type icon drawn before each name in the browser.
//!
//! The glyphs are Remix Icon, installed as a font by [`fonts::install`], so the program
//! carries one font file and no image assets. They are tinted by type: a folder is amber,
//! code blue, pictures violet, and so on, without competing with the state-coloured text.

use cvsdesk::filekind::FileKind;
use egui_remixicon::icons;

use super::fonts;

/// How much room one icon takes in front of a name.
pub const GAP: f32 = 18.0;
/// The icon's nominal size; the glyphs are square and stay sharp at any scale.
pub const SIZE: f32 = 16.0;

pub fn glyph(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Folder => icons::FOLDER_FILL,
        FileKind::Text => icons::FILE_TEXT_FILL,
        FileKind::Code => icons::FILE_CODE_FILL,
        FileKind::Config => icons::FILE_LIST_FILL,
        FileKind::Pdf => icons::FILE_PDF_FILL,
        FileKind::Word => icons::FILE_WORD_FILL,
        FileKind::Excel => icons::FILE_EXCEL_FILL,
        FileKind::Ppt => icons::FILE_PPT_FILL,
        FileKind::Image => icons::FILE_IMAGE_FILL,
        FileKind::Audio => icons::FILE_MUSIC_FILL,
        FileKind::Video => icons::FILE_VIDEO_FILL,
        FileKind::Archive => icons::FILE_ZIP_FILL,
        FileKind::Executable => icons::FILE_SETTINGS_FILL,
        FileKind::Database => icons::DATABASE_2_FILL,
        FileKind::Key => icons::FILE_LOCK_FILL,
        FileKind::Disc => icons::DISC_FILL,
        FileKind::Font => icons::FONT_FAMILY,
        FileKind::Generic => icons::FILE_FILL,
    }
}

pub fn color(kind: FileKind) -> egui::Color32 {
    match kind {
        FileKind::Folder => egui::Color32::from_rgb(217, 164, 65),
        FileKind::Text => egui::Color32::from_rgb(107, 114, 128),
        FileKind::Code => egui::Color32::from_rgb(47, 111, 228),
        FileKind::Config => egui::Color32::from_rgb(139, 115, 85),
        FileKind::Pdf => egui::Color32::from_rgb(217, 48, 37),
        FileKind::Word => egui::Color32::from_rgb(43, 87, 154),
        FileKind::Excel => egui::Color32::from_rgb(33, 115, 70),
        FileKind::Ppt => egui::Color32::from_rgb(210, 71, 38),
        FileKind::Image => egui::Color32::from_rgb(124, 58, 237),
        FileKind::Audio => egui::Color32::from_rgb(15, 118, 110),
        FileKind::Video => egui::Color32::from_rgb(190, 24, 93),
        FileKind::Archive => egui::Color32::from_rgb(161, 98, 7),
        FileKind::Executable => egui::Color32::from_rgb(71, 85, 105),
        FileKind::Database => egui::Color32::from_rgb(79, 70, 229),
        FileKind::Key => egui::Color32::from_rgb(180, 83, 9),
        FileKind::Disc => egui::Color32::from_rgb(100, 116, 139),
        FileKind::Font => egui::Color32::from_rgb(14, 116, 144),
        FileKind::Generic => egui::Color32::from_rgb(156, 163, 175),
    }
}

/// The icon of a row whose file is gone; the state column says "Missing".
pub const MISSING: egui::Color32 = egui::Color32::from_gray(175);

/// Draws the icon with its left edge at `left`, centred on `center_y`.
pub fn paint(
    painter: &egui::Painter,
    left: f32,
    center_y: f32,
    kind: FileKind,
    tint: egui::Color32,
) {
    painter.text(
        egui::pos2(left, center_y - SIZE / 2.0),
        egui::Align2::LEFT_TOP,
        glyph(kind),
        fonts::icon_font(SIZE),
        tint,
    );
}
