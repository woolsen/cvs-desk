//! The window's type scale, and the row heights that follow from it.
//!
//! Every size is set here once, through egui's own text styles. Widgets that let
//! egui lay text out ask for `TextStyle::Body` and friends; the few places that
//! paint text directly resolve the same style, so there is one source for both.
//!
//! The window stays compact: the body is 13, not 14, and the list keeps 22-pixel
//! rows. What it does not do is shrink secondary text below what a Chinese or
//! Latin letter needs: the old 9-pixel `Small` was unreadable on Windows.

use egui::{FontFamily, FontId, TextStyle};

/// Hints and the output pane's line count.
pub const SMALL: f32 = 11.0;
/// Running text: menus, file rows, dialogs and the status bar.
pub const BODY: f32 = 13.0;
/// The console and other fixed-pitch text (CVSROOT, settings paths).
///
/// This must stay even: a Chinese letter is two Latin cells, and at an odd size the
/// layout rounds each Latin cell up, so two cells no longer equal one Chinese letter.
pub const MONO: f32 = 14.0;
/// Window titles. One step above body text: 18 (egui's own heading) shouted, and the
/// compact frame reads better when the title stays close to the text under it.
pub const HEADING: f32 = 14.0;

/// One row of the file browser, and the column headings above it.
pub const LIST_ROW_H: f32 = 22.0;
pub const LIST_HEADER_H: f32 = 22.0;

/// The face paths, roots and settings files are drawn in: small, and fixed-pitch so a
/// path reads as a path rather than as a sentence.
pub fn small_monospace() -> FontId {
    FontId::new(SMALL, FontFamily::Monospace)
}

/// Secondary running text: hints, a window's scope and the pane's line count.
pub fn small() -> FontId {
    FontId::new(SMALL, FontFamily::Proportional)
}

/// Installs the scale into the style egui lays widgets out with.
pub fn apply(style: &mut egui::Style) {
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(SMALL, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(BODY, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(MONO, FontFamily::Monospace),
        ),
        (
            TextStyle::Heading,
            FontId::new(HEADING, FontFamily::Proportional),
        ),
    ]
    .into();
}

/// Installs the scale into both styles egui keeps, dark and light.
///
/// egui picks the active style from the system theme. Setting only the style that is
/// active at startup leaves the other at egui's defaults, and a light Windows then
/// draws its 18-pixel heading again.
pub fn install(ctx: &egui::Context) {
    ctx.all_styles_mut(apply);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::ui::test_run;

    #[test]
    fn a_dialog_title_sits_one_step_above_body_text() {
        assert_eq!(HEADING, 14.0);
        assert_eq!(BODY, 13.0);
        assert!(
            HEADING < 18.0,
            "the title stays below egui's default heading, which looked oversized"
        );
    }

    #[test]
    fn a_window_title_is_drawn_at_the_heading_size() {
        let ctx = egui::Context::default();
        install(&ctx);

        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        ));
        let draw = |ctx: &egui::Context| {
            egui::Window::new("Dialog title").show(ctx, |ui| {
                ui.label("Body text");
                ui.label(egui::RichText::new("Heading text").text_style(egui::TextStyle::Heading));
            });
        };
        let _ = test_run(&ctx, input.clone(), |_| draw(&ctx));
        let output = test_run(&ctx, input, |_| draw(&ctx));

        let heights = text_heights(&output);
        let height = |text: &str| {
            heights
                .iter()
                .find(|(shown, _)| shown == text)
                .unwrap_or_else(|| panic!("{text} is drawn"))
                .1
        };
        assert_eq!(height("Dialog title"), height("Heading text"));
        assert!(
            height("Dialog title") > height("Body text"),
            "a dialog title is one step above body text"
        );
    }

    #[test]
    fn a_path_is_drawn_small_and_fixed_pitch() {
        let path = small_monospace();
        assert_eq!(path.size, SMALL);
        assert_eq!(path.family, FontFamily::Monospace);
    }

    #[test]
    fn a_hint_is_drawn_small_and_in_the_ui_face() {
        let hint = small();
        assert_eq!(hint.size, SMALL);
        assert_eq!(hint.family, FontFamily::Proportional);
    }

    #[test]
    fn the_scale_reaches_both_theme_styles() {
        let ctx = egui::Context::default();
        install(&ctx);

        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let style = ctx.style_of(theme);
            assert_eq!(
                style.text_styles[&TextStyle::Heading].size,
                HEADING,
                "{theme:?} keeps the dialog title size"
            );
            assert_eq!(
                style.text_styles[&TextStyle::Body].size,
                BODY,
                "{theme:?} keeps the body size"
            );
        }

        // The window follows the system theme, so the light style is the one a
        // light Windows actually draws with.
        ctx.set_theme(egui::Theme::Light);
        assert_eq!(
            ctx.style_of(egui::Theme::Light).text_styles[&TextStyle::Heading].size,
            HEADING
        );
    }

    fn text_heights(output: &egui::FullOutput) -> Vec<(String, f32)> {
        fn collect(shape: &egui::Shape, out: &mut Vec<(String, f32)>) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push((text.galley.text().to_owned(), text.galley.size().y))
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn the_compact_scale_is_installed_for_every_text_role() {
        let mut style = egui::Style::default();
        apply(&mut style);
        assert_eq!(style.text_styles[&TextStyle::Small].size, SMALL);
        assert_eq!(style.text_styles[&TextStyle::Body].size, BODY);
        assert_eq!(style.text_styles[&TextStyle::Button].size, BODY);
        assert_eq!(style.text_styles[&TextStyle::Monospace].size, MONO);
        assert_eq!(style.text_styles[&TextStyle::Heading].size, HEADING);
        assert_eq!(
            style.text_styles[&TextStyle::Monospace].family,
            FontFamily::Monospace
        );
    }
}
