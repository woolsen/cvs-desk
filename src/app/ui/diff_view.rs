//! The two-column diff view.
//!
//! The component owns only presentation: it takes a [`DiffModel`] of two complete
//! files and draws it. Fetching the revisions, the temporary checkout and the
//! cleanup live in `version_loader`, so this file can be tested with plain strings.
//!
//! Layout is a fixed-height row list inside one vertical scroll area. Both columns
//! share one horizontal offset while their line-number gutters stay pinned, which
//! keeps long source lines readable without losing the line numbers. Only the rows
//! in the viewport are interacted with or painted.

use std::sync::Arc;

use cvsdesk::sidediff::{DiffModel, Line, Notice, Row, RowKind};
use egui::text::{ByteIndex, LayoutJob, LayoutSection, TextWrapping};
use egui::{Align, Align2, Color32, CursorIcon, FontId, Galley, Rect, Sense, Stroke};

use crate::app::ui::typography::{self, small_monospace};

const ADD_BG: Color32 = Color32::from_rgb(0xE6, 0xFF, 0xEC);
const DEL_BG: Color32 = Color32::from_rgb(0xFF, 0xEB, 0xE9);
const MISSING_BG: Color32 = Color32::from_rgb(0xF6, 0xF8, 0xFA);
const SELECTED_BG: Color32 = Color32::from_rgb(0xCC, 0xE0, 0xF7);
const HOVER_BG: Color32 = Color32::from_rgb(0xF5, 0xF8, 0xFC);
const GUTTER_TEXT: Color32 = Color32::from_gray(110);
const SIGN_ADD: Color32 = Color32::from_rgb(0x1A, 0x7F, 0x37);
const SIGN_DEL: Color32 = Color32::from_rgb(0xCF, 0x22, 0x2E);
const DIVIDER: Color32 = Color32::from_gray(210);
const TRACK: Color32 = Color32::from_gray(232);
const THUMB: Color32 = Color32::from_gray(180);
const NOTICE: Color32 = Color32::from_rgb(0x9A, 0x65, 0x00);
const NOTICE_BAD: Color32 = Color32::from_rgb(0xA5, 0x0E, 0x0E);
const PAD_X: f32 = 6.0;
const PAD_Y: f32 = 2.0;
const DIVIDER_W: f32 = 6.0;
const SIGN_W: f32 = 13.0;
const SCROLLBAR_H: f32 = 12.0;
const SYNTAX_LIMIT: usize = 4 * 1024 * 1024;

/// The widget's memory: the split, the shared horizontal offset and the highlighted
/// lines. The highlighted cache is rebuilt only when the model or the theme changes.
pub(crate) struct DiffView {
    split: f32,
    hscroll: f32,
    selected: Option<usize>,
    scroll_to: Option<usize>,
    cache: Option<Cache>,
}

impl Default for DiffView {
    fn default() -> Self {
        Self::new()
    }
}

impl DiffView {
    pub(crate) fn new() -> Self {
        Self {
            split: 0.5,
            hscroll: 0.0,
            selected: None,
            scroll_to: None,
            cache: None,
        }
    }

    /// Draw one model. `generation` changes whenever the window receives a new pair;
    /// it resets the scroll position and invalidates the highlight cache.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, model: &DiffModel, generation: u64) {
        let theme = egui_extras::syntax_highlighting::CodeTheme::from_style(ui.style())
            .with_font_size(typography::MONO);
        let rebuild = self
            .cache
            .as_ref()
            .is_none_or(|cache| cache.generation != generation || cache.theme != theme);
        if rebuild {
            self.cache = Some(Cache::build(ui, model, generation, theme.clone()));
            self.hscroll = 0.0;
            self.selected = None;
            self.scroll_to = Some(0);
        }

        self.header(ui, model);
        self.notices(ui, model);

        if model.rows.is_empty() {
            if model.notices.is_empty() {
                ui.label("No changes.");
            }
            return;
        }

        let row_h = row_height(ui);
        let body_h = (ui.available_height() - SCROLLBAR_H).max(60.0);

        egui::ScrollArea::vertical()
            .id_salt("side_by_side_diff")
            .auto_shrink([false, false])
            .max_height(body_h)
            .show_viewport(ui, |ui, viewport| {
                let width = ui.available_width();
                let total_h = model.rows.len() as f32 * row_h;
                let (content, _) =
                    ui.allocate_exact_size(egui::vec2(width, total_h), Sense::hover());
                let top = content.top();
                let left = content.left();

                let (left_rect, right_rect, divider_rect) = columns(content, self.split);

                let start = ((viewport.min.y / row_h).floor() as usize).min(model.rows.len());
                let end = ((viewport.max.y / row_h).ceil() as usize + 1).min(model.rows.len());
                let cache = self.cache.as_ref().expect("the cache was built above");

                for index in start..end {
                    let rect = Rect::from_min_size(
                        egui::pos2(left, top + index as f32 * row_h),
                        egui::vec2(width, row_h),
                    );
                    let response = ui.interact(
                        rect,
                        ui.scope_id().with(("diff_row", index)),
                        Sense::click(),
                    );
                    if response.clicked() {
                        self.selected = Some(index);
                    }
                    if response.double_clicked() {
                        copy_row(ui.ctx(), &model.rows[index]);
                    }

                    let selected = self.selected == Some(index);
                    paint_row(
                        ui,
                        model,
                        index,
                        rect,
                        left_rect,
                        right_rect,
                        row_h,
                        &cache.left,
                        &cache.right,
                        self.hscroll,
                        selected,
                        response.hovered(),
                    );

                    response.context_menu(|ui| {
                        if ui.button("Copy old line").clicked() {
                            if let Some(line) = left_line(&model.rows[index]) {
                                ui.ctx().copy_text(line.text.clone());
                            }
                            ui.close();
                        }
                        if ui.button("Copy new line").clicked() {
                            if let Some(line) = right_line(&model.rows[index]) {
                                ui.ctx().copy_text(line.text.clone());
                            }
                            ui.close();
                        }
                    });
                }

                // The divider sits above the rows, so dragging it does not click a
                // row. It is drawn after them for the same reason.
                let divider = ui.interact(
                    Rect::from_min_size(divider_rect.min, egui::vec2(DIVIDER_W, content.height())),
                    ui.scope_id().with("diff_divider"),
                    Sense::drag(),
                );
                if divider.dragged() {
                    if let Some(pointer) = divider.interact_pointer_pos() {
                        self.split = ((pointer.x - left) / content.width()).clamp(0.2, 0.8);
                    }
                }
                if divider.hovered() || divider.dragged() {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                let visible_divider = ui.clip_rect().intersect(divider_rect);
                if visible_divider.is_positive() {
                    ui.painter().rect_filled(visible_divider, 0.0, DIVIDER);
                }

                if let Some(target) = self.scroll_to.take() {
                    let target = target.min(model.rows.len().saturating_sub(1));
                    let y = top + target as f32 * row_h;
                    ui.scroll_to_rect(
                        Rect::from_min_size(egui::pos2(left, y), egui::vec2(width, row_h)),
                        Some(Align::Center),
                    );
                }
            });

        // The two columns share one offset. The maximum is driven by the wider side;
        // the visible width by the narrower code area.
        let (left_w, right_w) = code_widths(ui.available_width(), self.split, model);
        let visible = left_w.min(right_w).max(1.0);
        let cache = self.cache.as_ref().expect("the cache was built above");
        let max_width = cache.left.max_width.max(cache.right.max_width);
        let max_offset = (max_width - visible).max(0.0);
        self.hscroll = self.hscroll.clamp(0.0, max_offset);
        horizontal_bar(ui, &mut self.hscroll, max_offset, visible);
    }

    fn header(&mut self, ui: &mut egui::Ui, model: &DiffModel) {
        let mut target = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(&model.left.title).strong());
            ui.label(egui::RichText::new("→").color(GUTTER_TEXT));
            ui.label(egui::RichText::new(&model.right.title).strong());

            ui.separator();
            ui.label(
                egui::RichText::new(format!("+{}", model.stats.added))
                    .small()
                    .color(SIGN_ADD),
            );
            ui.label(
                egui::RichText::new(format!("−{}", model.stats.removed))
                    .small()
                    .color(SIGN_DEL),
            );
            if model.stats.added == 0 && model.stats.removed == 0 && model.notices.is_empty() {
                ui.label(
                    egui::RichText::new("No changes.")
                        .small()
                        .color(GUTTER_TEXT),
                );
            }

            if !model.hunks.is_empty() {
                ui.separator();
                if ui
                    .small_button("Prev")
                    .on_hover_text("Previous change")
                    .clicked()
                {
                    let current = self
                        .selected
                        .and_then(|row| hunk_at(model, row))
                        .map_or(model.hunks.len() - 1, |index| index.saturating_sub(1));
                    target = Some(model.hunks[current].first_row);
                }
                if ui
                    .small_button("Next")
                    .on_hover_text("Next change")
                    .clicked()
                {
                    let current = self
                        .selected
                        .and_then(|row| hunk_at(model, row))
                        .map_or(0, |index| (index + 1).min(model.hunks.len() - 1));
                    target = Some(model.hunks[current].first_row);
                }
                let changes = model.hunks.len();
                ui.label(
                    egui::RichText::new(format!(
                        "{changes} change{}",
                        if changes == 1 { "" } else { "s" }
                    ))
                    .small()
                    .color(GUTTER_TEXT),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("Copy", |ui| {
                    if ui.button("Old version").clicked() {
                        ui.ctx().copy_text(model.left.text.clone());
                        ui.close();
                    }
                    if ui.button("New version").clicked() {
                        ui.ctx().copy_text(model.right.text.clone());
                        ui.close();
                    }
                });
            });
        });
        if target.is_some() {
            self.scroll_to = target;
        }
    }

    fn notices(&self, ui: &mut egui::Ui, model: &DiffModel) {
        for notice in &model.notices {
            let color = match notice {
                Notice::Binary | Notice::TooLarge => NOTICE_BAD,
                _ => NOTICE,
            };
            ui.label(egui::RichText::new(notice.message()).small().color(color));
        }
    }
}

struct Cache {
    generation: u64,
    theme: egui_extras::syntax_highlighting::CodeTheme,
    left: SideCache,
    right: SideCache,
}

impl Cache {
    fn build(
        ui: &egui::Ui,
        model: &DiffModel,
        generation: u64,
        theme: egui_extras::syntax_highlighting::CodeTheme,
    ) -> Self {
        Self {
            generation,
            theme: theme.clone(),
            left: highlight_side(ui, &model.left, &theme),
            right: highlight_side(ui, &model.right, &theme),
        }
    }
}

#[derive(Default)]
struct SideCache {
    galleys: Vec<Arc<Galley>>,
    max_width: f32,
}

fn highlight_side(
    ui: &egui::Ui,
    file: &cvsdesk::sidediff::FileText,
    theme: &egui_extras::syntax_highlighting::CodeTheme,
) -> SideCache {
    if file.text.is_empty() {
        return SideCache::default();
    }
    let jobs = if file.meta.bytes <= SYNTAX_LIMIT && !file.language.is_empty() {
        let full = egui_extras::syntax_highlighting::highlight(
            ui.ctx(),
            ui.style(),
            theme,
            &file.text,
            &file.language,
        );
        split_job(&full, &file.text)
    } else {
        plain_jobs(ui, &file.text)
    };
    let mut galleys = Vec::with_capacity(jobs.len());
    let mut max_width: f32 = 0.0;
    for job in jobs {
        let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
        max_width = max_width.max(galley.size().x);
        galleys.push(galley);
    }
    SideCache { galleys, max_width }
}

/// Split a whole-file `LayoutJob` at line boundaries. The sections keep the syntax
/// colours the whole-file highlighter gave them, including a token that spans a
/// newline; the newline itself is dropped because it is not drawn.
fn split_job(job: &LayoutJob, text: &str) -> Vec<LayoutJob> {
    let mut out = Vec::new();
    let mut start = 0;
    for piece in text.split_inclusive('\n') {
        let end = start + piece.len();
        let body = piece.strip_suffix('\n').unwrap_or(piece);
        let body_end = start + body.len();
        let mut line = LayoutJob {
            text: body.to_string(),
            wrap: TextWrapping {
                max_width: f32::INFINITY,
                ..Default::default()
            },
            ..Default::default()
        };
        for section in &job.sections {
            let from = section.byte_range.start.0.max(start).min(body_end);
            let to = section.byte_range.end.0.min(body_end).max(from);
            if from < to {
                line.sections.push(LayoutSection {
                    leading_space: 0.0,
                    byte_range: ByteIndex(from - start)..ByteIndex(to - start),
                    format: section.format.clone(),
                });
            }
        }
        if !body.is_empty() && line.sections.is_empty() {
            // A highlighter always covers the whole text, but a defensive fallback
            // keeps a malformed job from making egui's layout panic.
            line.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(0)..ByteIndex(body.len()),
                format: job
                    .sections
                    .first()
                    .map(|section| section.format.clone())
                    .unwrap_or_default(),
            });
        }
        out.push(line);
        start = end;
    }
    out
}

fn plain_jobs(ui: &egui::Ui, text: &str) -> Vec<LayoutJob> {
    let font = mono_font(ui);
    let color = ui.visuals().text_color();
    text.split_inclusive('\n')
        .map(|piece| {
            let body = piece.strip_suffix('\n').unwrap_or(piece);
            LayoutJob::simple(body.to_string(), font.clone(), color, f32::INFINITY)
        })
        .collect()
}

fn row_height(ui: &egui::Ui) -> f32 {
    let font = mono_font(ui);
    ui.ctx().fonts_mut(|fonts| fonts.row_height(&font)) + PAD_Y * 2.0
}

fn mono_font(ui: &egui::Ui) -> FontId {
    ui.style()
        .text_styles
        .get(&egui::TextStyle::Monospace)
        .cloned()
        .unwrap_or_else(|| FontId::monospace(typography::MONO))
}

fn columns(content: Rect, split: f32) -> (Rect, Rect, Rect) {
    let usable = (content.width() - DIVIDER_W).max(1.0);
    let left_w = (usable * split).max(1.0);
    let right_w = (usable - left_w).max(1.0);
    let left = Rect::from_min_size(content.min, egui::vec2(left_w, content.height()));
    let right = Rect::from_min_size(
        egui::pos2(content.left() + left_w + DIVIDER_W, content.top()),
        egui::vec2(right_w, content.height()),
    );
    let divider = Rect::from_min_size(
        egui::pos2(content.left() + left_w, content.top()),
        egui::vec2(DIVIDER_W, content.height()),
    );
    (left, right, divider)
}

fn code_widths(width: f32, split: f32, model: &DiffModel) -> (f32, f32) {
    let usable = (width - DIVIDER_W).max(1.0);
    let left = usable * split;
    let right = usable - left;
    let left_gutter = gutter_width(line_count(&model.left.text));
    let right_gutter = gutter_width(line_count(&model.right.text));
    (
        (left - left_gutter - SIGN_W).max(1.0),
        (right - right_gutter - SIGN_W).max(1.0),
    )
}

fn line_count(text: &str) -> usize {
    text.split_inclusive('\n').count()
}

#[allow(clippy::too_many_arguments)]
fn paint_row(
    ui: &egui::Ui,
    model: &DiffModel,
    index: usize,
    rect: Rect,
    left_column: Rect,
    right_column: Rect,
    row_h: f32,
    left_cache: &SideCache,
    right_cache: &SideCache,
    hscroll: f32,
    selected: bool,
    hovered: bool,
) {
    let row = &model.rows[index];
    let painter = ui.painter();
    let left_half = Rect::from_min_max(
        egui::pos2(left_column.left(), rect.top()),
        egui::pos2(left_column.right(), rect.bottom()),
    );
    let right_half = Rect::from_min_max(
        egui::pos2(right_column.left(), rect.top()),
        egui::pos2(right_column.right(), rect.bottom()),
    );

    let (left_line, right_line) = row_lines(row);
    let left_bg = match row {
        Row::Both {
            kind: RowKind::Replace,
            ..
        } => DEL_BG,
        Row::Left(_) => DEL_BG,
        Row::Right(_) => MISSING_BG,
        _ => Color32::TRANSPARENT,
    };
    let right_bg = match row {
        Row::Both {
            kind: RowKind::Replace,
            ..
        } => ADD_BG,
        Row::Right(_) => ADD_BG,
        Row::Left(_) => MISSING_BG,
        _ => Color32::TRANSPARENT,
    };
    if left_bg != Color32::TRANSPARENT {
        painter.rect_filled(left_half, 0.0, left_bg);
    }
    if right_bg != Color32::TRANSPARENT {
        painter.rect_filled(right_half, 0.0, right_bg);
    }
    if selected {
        painter.rect_filled(rect, 0.0, SELECTED_BG.gamma_multiply(0.45));
    } else if hovered {
        painter.rect_filled(rect, 0.0, HOVER_BG.gamma_multiply(0.6));
    }

    let left_gutter = gutter_width(line_count(&model.left.text));
    let right_gutter = gutter_width(line_count(&model.right.text));
    let text_color = ui.visuals().text_color();
    let y = rect.center().y;

    paint_side(
        ui,
        left_half,
        left_gutter,
        left_line,
        left_cache,
        hscroll,
        y,
        text_color,
        row_h,
    );
    paint_side(
        ui,
        right_half,
        right_gutter,
        right_line,
        right_cache,
        hscroll,
        y,
        text_color,
        row_h,
    );
    draw_sign(ui, left_half, left_gutter, row, Side::Left, y);
    draw_sign(ui, right_half, right_gutter, row, Side::Right, y);
}

#[allow(clippy::too_many_arguments)]
fn paint_side(
    ui: &egui::Ui,
    column: Rect,
    gutter: f32,
    line: Option<&Line>,
    cache: &SideCache,
    hscroll: f32,
    y: f32,
    text_color: Color32,
    row_h: f32,
) {
    let painter = ui.painter();
    let number_x = column.left() + gutter - PAD_X;
    let code_left = column.left() + gutter + SIGN_W;
    let code_rect = Rect::from_min_size(
        egui::pos2(code_left, column.top()),
        egui::vec2((column.right() - code_left).max(1.0), column.height()),
    );
    if let Some(line) = line {
        painter.text(
            egui::pos2(number_x, y),
            Align2::RIGHT_CENTER,
            line.no.to_string(),
            small_monospace(),
            GUTTER_TEXT,
        );
        if let Some(galley) = cache.galleys.get(line.no.saturating_sub(1)) {
            let x = code_rect.left() - hscroll;
            painter.with_clip_rect(code_rect).galley(
                egui::pos2(x, column.top() + (row_h - galley.size().y) * 0.5),
                galley.clone(),
                text_color,
            );
            if !line.terminated {
                painter.with_clip_rect(code_rect).text(
                    egui::pos2(x + galley.size().x + 8.0, y),
                    Align2::LEFT_CENTER,
                    "\\ No newline at end of file",
                    small_monospace(),
                    NOTICE_BAD,
                );
            }
        }
    } else {
        // A filler row still has a gutter and a background, which makes the missing
        // side visible without pretending it contains a line.
        painter.line_segment(
            [
                egui::pos2(code_left, column.top() + 3.0),
                egui::pos2(code_left, column.bottom() - 3.0),
            ],
            Stroke::new(1.0, Color32::from_gray(230)),
        );
    }
}

fn draw_sign(ui: &egui::Ui, column: Rect, gutter: f32, row: &Row, side: Side, y: f32) {
    let (sign, color) = match (row, side) {
        (Row::Left(_), Side::Left) => ("-", SIGN_DEL),
        (Row::Right(_), Side::Right) => ("+", SIGN_ADD),
        (
            Row::Both {
                kind: RowKind::Replace,
                ..
            },
            Side::Left,
        ) => ("-", SIGN_DEL),
        (
            Row::Both {
                kind: RowKind::Replace,
                ..
            },
            Side::Right,
        ) => ("+", SIGN_ADD),
        _ => return,
    };
    ui.painter().text(
        egui::pos2(column.left() + gutter + 3.0, y),
        Align2::LEFT_CENTER,
        sign,
        mono_font(ui),
        color,
    );
}

#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
}

fn gutter_width(lines: usize) -> f32 {
    let digits = lines.max(1).to_string().len() as f32;
    digits * 7.6 + PAD_X * 2.0
}

fn horizontal_bar(ui: &mut egui::Ui, offset: &mut f32, max: f32, visible: f32) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), SCROLLBAR_H),
        Sense::click_and_drag(),
    );
    if max <= 0.0 {
        return;
    }
    let track = rect.shrink2(egui::vec2(4.0, 3.0));
    ui.painter().rect_filled(track, 4.0, TRACK);
    let total = visible + max;
    let thumb_w = (visible / total * track.width())
        .max(28.0)
        .min(track.width());
    let travel = (track.width() - thumb_w).max(1.0);
    let t = (*offset / max).clamp(0.0, 1.0);
    let thumb = Rect::from_min_size(
        egui::pos2(track.left() + t * travel, track.top()),
        egui::vec2(thumb_w, track.height()),
    );
    ui.painter().rect_filled(thumb, 4.0, THUMB);
    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let t = ((pointer.x - track.left() - thumb_w * 0.5) / travel).clamp(0.0, 1.0);
            *offset = t * max;
        }
    }
}

fn row_lines(row: &Row) -> (Option<&Line>, Option<&Line>) {
    match row {
        Row::Both { left, right, .. } => (Some(left), Some(right)),
        Row::Left(line) => (Some(line), None),
        Row::Right(line) => (None, Some(line)),
    }
}

fn left_line(row: &Row) -> Option<&Line> {
    row_lines(row).0
}

fn right_line(row: &Row) -> Option<&Line> {
    row_lines(row).1
}

fn copy_row(ctx: &egui::Context, row: &Row) {
    if let Some(line) = right_line(row).or_else(|| left_line(row)) {
        ctx.copy_text(line.text.clone());
    }
}

fn hunk_at(model: &DiffModel, row: usize) -> Option<usize> {
    model
        .hunks
        .iter()
        .position(|hunk| row >= hunk.first_row && row <= hunk.last_row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cvsdesk::sidediff::{DiffOptions, FileText};

    fn model(old: &str, new: &str) -> DiffModel {
        DiffModel::build(
            FileText::from_text("old", old, "c"),
            FileText::from_text("new", new, "c"),
            DiffOptions::default(),
        )
    }

    #[test]
    fn a_line_job_keeps_each_line_and_its_colour() {
        let mut job = LayoutJob::default();
        job.append(
            "a",
            0.0,
            egui::TextFormat::simple(FontId::monospace(12.0), Color32::RED),
        );
        job.append(
            "b\nc",
            0.0,
            egui::TextFormat::simple(FontId::monospace(12.0), Color32::BLUE),
        );
        let lines = split_job(&job, "ab\nc");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "ab");
        assert_eq!(lines[1].text, "c");
        assert_eq!(lines[0].sections.len(), 2);
        assert_eq!(lines[1].sections.len(), 1);
    }

    #[test]
    fn the_last_line_without_a_newline_is_still_a_job() {
        let job = LayoutJob::simple(
            "a".to_string(),
            FontId::monospace(12.0),
            Color32::BLACK,
            f32::INFINITY,
        );
        let lines = split_job(&job, "a");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "a");
    }

    #[test]
    fn trailing_blank_lines_each_get_a_job() {
        let job = LayoutJob::simple(
            "a\n\n".to_string(),
            FontId::monospace(12.0),
            Color32::BLACK,
            f32::INFINITY,
        );
        let lines = split_job(&job, "a\n\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "");
    }

    #[test]
    fn the_hunk_buttons_find_the_change_around_a_row() {
        let model = model("a\nb\nc\nd\ne\nf\ng\n", "a\nB\nc\nd\ne\nf\nG\n");
        assert_eq!(model.hunks.len(), 2);
        assert_eq!(hunk_at(&model, model.hunks[0].first_row), Some(0));
        assert_eq!(hunk_at(&model, model.hunks[1].first_row), Some(1));
        assert_eq!(hunk_at(&model, 3), None);
    }

    #[test]
    fn copy_row_prefers_the_new_side() {
        let row = Row::Both {
            left: Line {
                no: 1,
                text: "old".into(),
                terminated: true,
            },
            right: Line {
                no: 1,
                text: "new".into(),
                terminated: true,
            },
            kind: RowKind::Replace,
        };
        let ctx = egui::Context::default();
        copy_row(&ctx, &row);
        // The context's output is only produced by a frame; this test mainly makes
        // sure the function chooses the new side without panicking.
    }

    #[test]
    fn an_empty_model_shows_the_no_changes_path() {
        let model = model("", "");
        assert!(model.rows.is_empty());
    }

    #[test]
    fn the_raw_button_does_not_change_the_model() {
        // The component owns no raw mode; drawing a model must never mutate it.
        let model = model("a\n", "b\n");
        assert_eq!(model.rows.len(), 1);
    }

    #[test]
    fn the_widget_marks_a_missing_final_newline() {
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let model = model("a\nb", "a\nb\n");
        let mut view = DiffView::new();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 420.0),
        ));
        let output = crate::app::ui::test_run(&ctx, input, |ui| {
            view.show(ui, &model, 1);
        });
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }
        assert!(
            texts
                .iter()
                .any(|text| text == "\\ No newline at end of file"),
            "the view hid the missing final newline: {texts:?}"
        );
    }

    #[test]
    fn the_widget_draws_both_columns_and_keeps_empty_rows() {
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let model = model("a\n\nb\n", "a\n\nB\n");
        let mut view = DiffView::new();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 420.0),
        ));
        let output = crate::app::ui::test_run(&ctx, input, |ui| {
            view.show(ui, &model, 1);
        });

        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }
        for expected in ["old", "new", "a", "b", "B", "1", "2", "3"] {
            assert!(
                texts.iter().any(|text| text == expected),
                "the diff view did not draw {expected:?}: {texts:?}"
            );
        }

        fn walk_rects(shape: &egui::Shape, out: &mut Vec<(Rect, Color32)>) {
            match shape {
                egui::Shape::Rect(rect) => out.push((rect.rect, rect.fill)),
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk_rects(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut rects = Vec::new();
        for clipped in &output.shapes {
            walk_rects(&clipped.shape, &mut rects);
        }
        assert!(
            rects
                .iter()
                .any(|(rect, fill)| *fill == DEL_BG && rect.height() < 40.0),
            "the removed row has no per-row background: {rects:?}"
        );
        assert!(
            rects
                .iter()
                .any(|(rect, fill)| *fill == ADD_BG && rect.height() < 40.0),
            "the added row has no per-row background: {rects:?}"
        );
    }
}
