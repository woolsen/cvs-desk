//! Labels whose long text fills each row.
//!
//! egui breaks a row at a Chinese letter as soon as it is the last "word boundary" that
//! fits, so a Windows path that runs through a Chinese folder name ends its row right
//! after that name and leaves the rest of the row empty. [`label_in`] breaks at the last
//! separator that fits instead, through [`cvsdesk::util::wrap_path`].

use cvsdesk::util::wrap_path;

/// Draw `text` at `font`, dimmed when `weak`, filling each row to the width the label has.
pub(crate) fn label_in(
    ui: &mut egui::Ui,
    text: &str,
    font: egui::FontId,
    weak: bool,
) -> egui::Response {
    // A hair narrower than the row: a line measured as exactly the width can come out a
    // fraction wider once egui adds the kerning to the letter that follows it.
    let width = (ui.available_width() - 1.0).max(1.0);
    let filled = wrap_path(text, width, |part| width_of(ui, part, &font));
    let mut rich = egui::RichText::new(filled).font(font);
    if weak {
        rich = rich.weak();
    }
    ui.label(rich)
}

/// The room `text` takes in `font`, the same measure egui lays a label out with.
fn width_of(ui: &egui::Ui, text: &str, font: &egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
        .size()
        .x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui::fonts;
    use crate::app::ui::test_run;

    const PATH: &str = r"D:\example\projects\示例项目\repos\cvs\SourceCode\src";

    /// `Working folder: ` plus the path up to and including `示例项目`.
    const UP_TO_THE_CHINESE_NAME: usize = 40;

    fn text() -> String {
        format!("Working folder: {PATH}")
    }

    fn input() -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 400.0),
        ));
        input
    }

    /// The room the whole line takes, so a test can talk about a row half its size.
    fn natural_width(ctx: &egui::Context) -> f32 {
        let font = egui::TextStyle::Body.resolve(&ctx.style_of(egui::Theme::Light));
        ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text(), font, egui::Color32::PLACEHOLDER)
                .size()
                .x
        })
    }

    /// One row of laid-out text: what it says, and how much room it takes.
    struct Row {
        text: String,
        width: f32,
    }

    /// Everything the rows say, with the row breaks taken out.
    fn joined(rows: &[Row]) -> String {
        rows.iter().map(|row| row.text.as_str()).collect()
    }

    /// Draw the label in a fixed-width window; answer each row egui actually drew.
    fn drawn(ctx: &egui::Context, window_width: f32) -> Vec<Row> {
        let draw = |ctx: &egui::Context| {
            egui::Window::new("Wrap")
                .resizable(false)
                .default_width(window_width)
                .show(ctx, |ui| {
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    label_in(ui, &text(), font, false);
                });
        };
        // The window learns its content size on one frame and lays out with it on the next.
        for _ in 0..3 {
            let _ = test_run(ctx, input(), |_| draw(ctx));
        }
        let output = test_run(ctx, input(), |_| draw(ctx));

        let mut found = None;
        fn collect(shape: &egui::Shape, found: &mut Option<Vec<Row>>) {
            match shape {
                egui::Shape::Text(text) if text.galley.text().starts_with("Working folder") => {
                    let rows = text
                        .galley
                        .rows
                        .iter()
                        .map(|row| Row {
                            text: row.glyphs.iter().map(|glyph| glyph.chr).collect(),
                            width: row.rect().width(),
                        })
                        .collect();
                    *found = Some(rows);
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, found);
                    }
                }
                _ => {}
            }
        }
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut found);
        }
        found.expect("the label is drawn")
    }

    /// The window's own fonts, so the Chinese name takes the room it takes in the program.
    fn window() -> egui::Context {
        let ctx = egui::Context::default();
        fonts::install(&ctx);
        // egui only builds its faces on the first frame.
        let _ = test_run(&ctx, input(), |_| {});
        ctx
    }

    #[test]
    fn a_path_through_a_chinese_folder_runs_past_the_chinese_name() {
        let ctx = window();
        let window_width = natural_width(&ctx) * 0.75;
        let rows = drawn(&ctx, window_width);

        assert!(rows.len() >= 2, "the path has to wrap");
        assert_eq!(joined(&rows), text(), "no character may be lost");
        assert!(
            rows[0].text.chars().count() > UP_TO_THE_CHINESE_NAME,
            "the first row must not end right after the Chinese name: {:?}",
            rows[0].text
        );
        for row in &rows {
            assert!(
                row.width <= window_width,
                "{:?} is wider than the window it was given",
                row.text
            );
        }
    }

    #[test]
    fn text_that_fits_stays_on_one_row() {
        let ctx = window();
        let rows = drawn(&ctx, natural_width(&ctx) + 160.0);
        assert_eq!(rows.len(), 1, "{:?}", rows[0].text);
    }
}
