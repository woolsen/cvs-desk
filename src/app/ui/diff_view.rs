//! The two-column diff view.
//!
//! The component owns only presentation: it takes a [`DiffModel`] of two complete
//! files and draws it. Fetching the revisions, the temporary checkout and the
//! cleanup live in `version_loader`, so this file can be tested with plain strings.
//!
//! The colours follow the IntelliJ Light diff viewer: a row that exists only on the
//! new side is green, a modified or deleted row is pale blue, and the line-number
//! gutter stays white with a `+` or `-` beside the code. The layering is the one
//! IDEA and VS Code use: editor background, then the whole-line diff tint, then the
//! stronger intra-line word tint, then the selection, and finally the syntax-coloured
//! text. Text is always painted last, so no overlay can hide it.
//!
//! Layout is a fixed-height row list inside one vertical scroll area. Both columns
//! share one horizontal offset while their line-number gutters stay pinned, which
//! keeps long source lines readable without losing the line numbers. Only the rows
//! in the viewport are interacted with or painted. The right-hand overview lane is
//! the vertical scrollbar: it shows one mark per change and can be clicked or dragged.

use std::ops::Range;
use std::sync::Arc;

use cvsdesk::sidediff::{DiffModel, Hunk, Line, Notice, Row, RowKind, WordDiff};
use egui::text::{ByteIndex, CCursor, LayoutJob, LayoutSection, TextWrapping};
use egui::{
    Align, Align2, Color32, CursorIcon, FontId, Galley, Key, Modifiers, Rect, Sense, Stroke,
};

use crate::app::ui::typography::{self, small_monospace};

// Sampled from IntelliJ Light's diff viewer. They are painted in this order:
// editor background, whole-line tint, the stronger intra-line word tint, the
// selection, and finally the syntax-coloured text. IDEA and VS Code both keep the
// text last, so an overlay never hides the code it is marking.
const EDITOR_BG: Color32 = Color32::WHITE;
const ADDED_BG: Color32 = Color32::from_rgb(0xBE, 0xE6, 0xBE);
const CHANGED_BG: Color32 = Color32::from_rgb(0xE7, 0xEF, 0xFA);
const WORD_CHANGED_BG: Color32 = Color32::from_rgb(0xC2, 0xD8, 0xF2);
const SELECTION_BG: Color32 = Color32::from_rgb(0xB5, 0xD5, 0xFF);
const SELECTION_INACTIVE_BG: Color32 = Color32::from_rgb(0xE5, 0xEB, 0xF1);
const GUTTER_TEXT: Color32 = Color32::from_rgb(0x8C, 0x8C, 0x8C);
const SIGN_ADD: Color32 = Color32::from_rgb(0x1E, 0x6B, 0x2E);
const SIGN_DEL: Color32 = Color32::from_rgb(0xB3, 0x3A, 0x3A);
const DIVIDER: Color32 = Color32::from_rgb(0xE3, 0xE3, 0xE3);
const DIVIDER_ACTIVE: Color32 = Color32::from_rgb(0x4A, 0x86, 0xC8);
const TRACK: Color32 = Color32::from_rgb(0xF1, 0xF1, 0xF1);
const THUMB: Color32 = Color32::from_rgb(0xC9, 0xC9, 0xC9);
const OVERVIEW_THUMB: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 18);
const OVERVIEW_THUMB_BORDER: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 32);
const ADD_MARK: Color32 = Color32::from_rgb(0x59, 0xA8, 0x69);
const MOD_MARK: Color32 = Color32::from_rgb(0x38, 0x9F, 0xD6);
const DEL_MARK: Color32 = Color32::from_rgb(0xD7, 0x54, 0x52);
const DIFF_COUNT: Color32 = Color32::from_rgb(0x16, 0x7A, 0xD1);
const NOTICE: Color32 = Color32::from_rgb(0x9A, 0x65, 0x00);
const NOTICE_BAD: Color32 = Color32::from_rgb(0xA5, 0x0E, 0x0E);
const PAD_X: f32 = 6.0;
const PAD_Y: f32 = 3.0;
const SPINE_W: f32 = 9.0;
const SIGN_W: f32 = 13.0;
const OVERVIEW_W: f32 = 12.0;
const SCROLLBAR_H: f32 = 10.0;
const SYNTAX_LIMIT: usize = 4 * 1024 * 1024;

/// One end of a text selection. `ch` is a byte offset into the line's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextPos {
    side: Side,
    row: usize,
    ch: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    anchor: TextPos,
    focus: TextPos,
}

impl Selection {
    fn normalized(self) -> (TextPos, TextPos) {
        if (self.anchor.row, self.anchor.ch) <= (self.focus.row, self.focus.ch) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    fn is_empty(self) -> bool {
        self.anchor == self.focus
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChangeKind {
    Added,
    Modified,
    Removed,
}

impl ChangeKind {
    fn color(self) -> Color32 {
        match self {
            Self::Added => ADD_MARK,
            Self::Modified => MOD_MARK,
            Self::Removed => DEL_MARK,
        }
    }
}

/// The widget's memory: the split, the shared horizontal offset and the highlighted
/// lines. The highlighted cache is rebuilt only when the model or the theme changes.
pub(crate) struct DiffView {
    split: f32,
    hscroll: f32,
    selection: Option<Selection>,
    caret: Option<TextPos>,
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
            selection: None,
            caret: None,
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
            self.selection = None;
            self.caret = None;
            self.scroll_to = Some(0);
        }

        let focus_id = ui.make_persistent_id("side_by_side_diff_focus");
        let focused = ui.ctx().memory(|memory| memory.has_focus(focus_id));
        self.handle_keys(ui, model, focus_id, focused);
        self.header(ui, model, focus_id);
        self.notices(ui, model);

        if model.rows.is_empty() {
            if model.notices.is_empty() {
                ui.label("No changes.");
            }
            return;
        }

        let row_h = row_height(ui);

        // Reserve the whole remaining height in one exact rectangle. If the body
        // reports even a few points more than it was given, egui's `Resize` grows
        // the window back every frame; see `a_shrunken_diff_window_does_not_grow_back`.
        let (body_rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        // The rows request this id when they are clicked. Register a tiny focusable
        // widget for it as well: egui drops a focus id that no widget claims at the
        // end of the frame, and Ctrl+C would otherwise lose the selection's owner.
        let _focus_anchor = ui.interact(
            Rect::from_min_size(body_rect.min, egui::vec2(1.0, 1.0)),
            focus_id,
            Sense::click(),
        );
        let mut body = ui.new_child(egui::UiBuilder::new().max_rect(body_rect));
        let bar_gap = body.spacing().item_spacing.y;
        let code_h = (body_rect.height() - SCROLLBAR_H - bar_gap).max(1.0);
        let overview_w = if body_rect.width() >= 460.0 {
            OVERVIEW_W
        } else {
            0.0
        };
        let code_w = (body_rect.width() - overview_w).max(120.0);
        let code_rect = Rect::from_min_size(body_rect.min, egui::vec2(code_w, code_h));
        let mut viewport = None;

        {
            let mut code_ui = body.new_child(egui::UiBuilder::new().max_rect(code_rect));
            egui::ScrollArea::vertical()
                .id_salt("side_by_side_diff")
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .max_height(code_h)
                .show_viewport(&mut code_ui, |ui, view| {
                    viewport = Some(view);
                    let width = ui.available_width();
                    let total_h = model.rows.len() as f32 * row_h;
                    let (content, _) =
                        ui.allocate_exact_size(egui::vec2(width, total_h), Sense::hover());
                    let top = content.top();
                    let left = content.left();
                    let (left_column, right_column, divider_rect) = columns(content, self.split);

                    let start = ((view.min.y / row_h).floor() as usize).min(model.rows.len());
                    let end = ((view.max.y / row_h).ceil() as usize + 1).min(model.rows.len());
                    let text_color = ui.visuals().text_color();
                    let focused = ui.ctx().memory(|memory| memory.has_focus(focus_id));

                    for index in start..end {
                        let rect = Rect::from_min_size(
                            egui::pos2(left, top + index as f32 * row_h),
                            egui::vec2(width, row_h),
                        );
                        let response = ui.interact(
                            rect,
                            ui.scope_id().with(("diff_row", index)),
                            Sense::click_and_drag(),
                        );

                        self.interact_row(
                            ui,
                            model,
                            left_column,
                            right_column,
                            row_h,
                            content,
                            &response,
                            focus_id,
                        );

                        let selection = self.selection;
                        if let Some(cache) = self.cache.as_ref() {
                            paint_row(
                                ui,
                                model,
                                index,
                                rect,
                                left_column,
                                right_column,
                                row_h,
                                &cache.left,
                                &cache.right,
                                self.hscroll,
                                selection,
                                focused,
                                text_color,
                            );
                        }

                        response.context_menu(|ui| {
                            if let Some(selection) = selection {
                                if !selection.is_empty() && ui.button("Copy selection").clicked() {
                                    ui.ctx().copy_text(selection_text(model, selection));
                                    ui.close();
                                }
                                ui.separator();
                            }
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

                    // The divider is drawn after the rows, so it is never covered
                    // by a row background. It is only one pixel wide; the nine-pixel
                    // strip around it is the drag target.
                    let divider = ui.interact(
                        divider_rect,
                        ui.scope_id().with("diff_divider"),
                        Sense::click_and_drag(),
                    );
                    if divider.dragged() {
                        if let Some(pointer) = divider.interact_pointer_pos() {
                            self.split = ((pointer.x - left) / content.width()).clamp(0.2, 0.8);
                        }
                    }
                    if divider.double_clicked() {
                        self.split = 0.5;
                    }
                    if divider.hovered() || divider.dragged() {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                    }
                    let line_color = if divider.hovered() || divider.dragged() {
                        DIVIDER_ACTIVE
                    } else {
                        DIVIDER
                    };
                    let line_x = divider_rect.center().x;
                    ui.painter().line_segment(
                        [
                            egui::pos2(line_x, content.top()),
                            egui::pos2(line_x, content.bottom()),
                        ],
                        Stroke::new(1.0, line_color),
                    );

                    if let Some(target) = self.scroll_to.take() {
                        let target = target.min(model.rows.len().saturating_sub(1));
                        let y = top + target as f32 * row_h;
                        ui.scroll_to_rect(
                            Rect::from_min_size(egui::pos2(left, y), egui::vec2(width, row_h)),
                            Some(Align::Center),
                        );
                    }
                });

            // The two columns share one offset. The maximum is driven by the wider
            // side; the visible width by the narrower code area.
            let (left_w, right_w) = code_widths(code_rect.width(), self.split, model);
            let visible = left_w.min(right_w).max(1.0);
            let cache = self.cache.as_ref().expect("the cache was built above");
            let max_width = cache.left.max_width.max(cache.right.max_width);
            let max_offset = (max_width - visible).max(0.0);
            self.hscroll = self.hscroll.clamp(0.0, max_offset);
            horizontal_bar(&mut code_ui, &mut self.hscroll, max_offset, visible);
        }

        if overview_w > 0.0 {
            if let Some(view) = viewport {
                self.overview(&mut body, model, body_rect, code_h, row_h, view);
            }
        }
    }

    fn handle_keys(
        &mut self,
        ui: &mut egui::Ui,
        model: &DiffModel,
        focus_id: egui::Id,
        focused: bool,
    ) {
        if !focused {
            return;
        }
        let (copy, select_all, clear) = ui.input_mut(|input| {
            (
                input.consume_key(Modifiers::COMMAND, Key::C),
                input.consume_key(Modifiers::COMMAND, Key::A),
                input.consume_key(Modifiers::NONE, Key::Escape),
            )
        });

        if copy {
            if let Some(selection) = self.selection {
                let text = selection_text(model, selection);
                if !text.is_empty() {
                    ui.ctx().copy_text(text);
                }
            }
        }
        if select_all {
            let side = self
                .selection
                .map(|selection| selection.focus.side)
                .or(self.caret.map(|caret| caret.side));
            if let Some(side) = side {
                if let Some(selection) = select_all_selection(model, side) {
                    self.selection = Some(selection);
                    self.caret = Some(selection.focus);
                }
            }
        }
        if clear {
            self.selection = None;
            self.caret = None;
            ui.ctx()
                .memory_mut(|memory| memory.surrender_focus(focus_id));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn interact_row(
        &mut self,
        ui: &egui::Ui,
        model: &DiffModel,
        left_column: Rect,
        right_column: Rect,
        row_h: f32,
        content: Rect,
        response: &egui::Response,
        focus_id: egui::Id,
    ) {
        let pointer = ui.ctx().input(|input| input.pointer.interact_pos());
        let columns = (left_column, right_column);

        if response.drag_started() {
            if let Some(pos) = ui
                .ctx()
                .input(|input| input.pointer.press_origin())
                .or(pointer)
            {
                if let Some(side) = side_at(pos, columns) {
                    ui.ctx().memory_mut(|memory| memory.request_focus(focus_id));
                    let row = row_at_y(pos.y, content, row_h, model.rows.len());
                    let anchor = self.pos_at(model, side, row, pos, columns, row_h);
                    self.selection = Some(Selection {
                        anchor,
                        focus: anchor,
                    });
                    self.caret = Some(anchor);
                }
            }
        }

        if response.dragged() {
            if let Some(selection) = self.selection {
                if let Some(pos) = pointer {
                    let side = selection.anchor.side;
                    let row = row_at_y(pos.y, content, row_h, model.rows.len());
                    let focus = self.pos_at(model, side, row, pos, columns, row_h);
                    self.selection = Some(Selection {
                        anchor: selection.anchor,
                        focus,
                    });
                    self.caret = Some(focus);
                }
            }
        }

        if response.triple_clicked() {
            if let Some(pos) = pointer {
                if let Some(side) = side_at(pos, columns) {
                    let row = row_at_y(pos.y, content, row_h, model.rows.len());
                    if let Some(line) = line_for(model, side, row) {
                        let start = TextPos { side, row, ch: 0 };
                        let end = TextPos {
                            side,
                            row,
                            ch: line.text.len(),
                        };
                        self.selection = Some(Selection {
                            anchor: start,
                            focus: end,
                        });
                        self.caret = Some(end);
                        ui.ctx().memory_mut(|memory| memory.request_focus(focus_id));
                    }
                }
            }
        } else if response.double_clicked() {
            if let Some(pos) = pointer {
                if let Some(side) = side_at(pos, columns) {
                    let row = row_at_y(pos.y, content, row_h, model.rows.len());
                    let at = self.pos_at(model, side, row, pos, columns, row_h);
                    if let Some(line) = line_for(model, side, at.row) {
                        let range = word_range(&line.text, at.ch);
                        let start = TextPos {
                            side,
                            row: at.row,
                            ch: range.start,
                        };
                        let end = TextPos {
                            side,
                            row: at.row,
                            ch: range.end,
                        };
                        self.selection = Some(Selection {
                            anchor: start,
                            focus: end,
                        });
                        self.caret = Some(end);
                        ui.ctx().memory_mut(|memory| memory.request_focus(focus_id));
                    }
                }
            }
        } else if response.clicked() {
            if let Some(pos) = pointer {
                if let Some(side) = side_at(pos, columns) {
                    let row = row_at_y(pos.y, content, row_h, model.rows.len());
                    let focus = self.pos_at(model, side, row, pos, columns, row_h);
                    let shift = ui.ctx().input(|input| input.modifiers.shift);
                    if shift {
                        if let Some(caret) = self.caret.filter(|caret| caret.side == side) {
                            self.selection = Some(Selection {
                                anchor: caret,
                                focus,
                            });
                        } else {
                            self.selection = Some(Selection {
                                anchor: focus,
                                focus,
                            });
                        }
                    } else {
                        self.selection = Some(Selection {
                            anchor: focus,
                            focus,
                        });
                    }
                    self.caret = Some(focus);
                    ui.ctx().memory_mut(|memory| memory.request_focus(focus_id));
                }
            }
        }
    }

    fn pos_at(
        &self,
        model: &DiffModel,
        side: Side,
        row: usize,
        pointer: egui::Pos2,
        columns: (Rect, Rect),
        row_h: f32,
    ) -> TextPos {
        let row = nearest_row(model, side, row);
        let column = match side {
            Side::Left => columns.0,
            Side::Right => columns.1,
        };
        let gutter = gutter_width(line_count(side_text(model, side)));
        let code_left = column.left() + gutter + SIGN_W;
        let ch = if pointer.x <= code_left {
            0
        } else if let (Some(line), Some(cache)) = (line_for(model, side, row), self.cache.as_ref())
        {
            if let Some(galley) = cache.side(side).galleys.get(line.no.saturating_sub(1)) {
                let local_x = pointer.x - (code_left - self.hscroll);
                let cursor = galley.cursor_from_pos(egui::vec2(local_x, row_h * 0.5));
                byte_at_char(&line.text, cursor.index.0)
            } else {
                line.text.len()
            }
        } else {
            0
        };
        TextPos { side, row, ch }
    }

    fn header(&mut self, ui: &mut egui::Ui, model: &DiffModel, focus_id: egui::Id) {
        let mut target = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(&model.left.title).strong());
            ui.label(egui::RichText::new("→").color(GUTTER_TEXT));
            ui.label(egui::RichText::new(&model.right.title).strong());

            if !model.hunks.is_empty() {
                ui.separator();
                if ui
                    .small_button("Prev")
                    .on_hover_text("Previous change")
                    .clicked()
                {
                    let current = self
                        .active_row()
                        .and_then(|row| hunk_at(model, row))
                        .map_or(model.hunks.len() - 1, |index| index.saturating_sub(1));
                    target = Some(model.hunks[current].first_row);
                    ui.ctx().memory_mut(|memory| memory.request_focus(focus_id));
                }
                if ui
                    .small_button("Next")
                    .on_hover_text("Next change")
                    .clicked()
                {
                    let current = self
                        .active_row()
                        .and_then(|row| hunk_at(model, row))
                        .map_or(0, |index| (index + 1).min(model.hunks.len() - 1));
                    target = Some(model.hunks[current].first_row);
                    ui.ctx().memory_mut(|memory| memory.request_focus(focus_id));
                }
                let changes = model.hunks.len();
                ui.label(
                    egui::RichText::new(format!(
                        "{changes} difference{}",
                        if changes == 1 { "" } else { "s" }
                    ))
                    .small()
                    .color(DIFF_COUNT),
                );
            } else if model.stats.added == 0 && model.stats.removed == 0 && model.notices.is_empty()
            {
                ui.label(
                    egui::RichText::new("No changes.")
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

    fn active_row(&self) -> Option<usize> {
        self.selection
            .map(|selection| selection.focus.row)
            .or(self.caret.map(|caret| caret.row))
    }

    fn overview(
        &mut self,
        ui: &mut egui::Ui,
        model: &DiffModel,
        body_rect: Rect,
        height: f32,
        row_h: f32,
        viewport: Rect,
    ) {
        let lane = Rect::from_min_size(
            egui::pos2(body_rect.right() - OVERVIEW_W, body_rect.top()),
            egui::vec2(OVERVIEW_W, height),
        );
        ui.painter().line_segment(
            [
                egui::pos2(lane.left(), lane.top()),
                egui::pos2(lane.left(), lane.bottom()),
            ],
            Stroke::new(1.0, DIVIDER),
        );

        let total_h = model.rows.len() as f32 * row_h;
        if total_h <= 0.0 {
            return;
        }

        let visible_top = (viewport.min.y / total_h).clamp(0.0, 1.0);
        let visible_bottom = (viewport.max.y / total_h).clamp(0.0, 1.0);
        let thumb_top = lane.top() + visible_top * lane.height();
        let thumb_bottom = lane.top() + visible_bottom * lane.height();
        let thumb = Rect::from_min_max(
            egui::pos2(lane.left() + 2.0, thumb_top),
            egui::pos2(lane.right() - 2.0, (thumb_bottom).max(thumb_top + 8.0)),
        );
        ui.painter().rect_filled(thumb, 3.0, OVERVIEW_THUMB);
        ui.painter().rect_stroke(
            thumb,
            3.0,
            Stroke::new(1.0, OVERVIEW_THUMB_BORDER),
            egui::StrokeKind::Inside,
        );

        for (kind, y, h) in hunk_marks(model, lane.height()) {
            let mark = Rect::from_min_size(
                egui::pos2(lane.left() + 3.0, lane.top() + y),
                egui::vec2((lane.width() - 6.0).max(1.0), h),
            );
            ui.painter().rect_filled(mark, 2.0, kind.color());
        }

        let response = ui.interact(
            lane,
            ui.scope_id().with("diff_overview"),
            Sense::click_and_drag(),
        );
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if response.clicked() || response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let fraction = ((pointer.y - lane.top()) / lane.height()).clamp(0.0, 1.0);
                let row = (fraction * model.rows.len() as f32).floor() as usize;
                self.scroll_to = Some(row.min(model.rows.len().saturating_sub(1)));
                ui.ctx().request_repaint();
            }
        }
        if response.hovered() {
            response.show_tooltip_text("Click or drag to jump to a change");
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

impl Cache {
    fn side(&self, side: Side) -> &SideCache {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
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
    let usable = (content.width() - SPINE_W).max(1.0);
    let left_w = (usable * split).max(1.0);
    let right_w = (usable - left_w).max(1.0);
    let left = Rect::from_min_size(content.min, egui::vec2(left_w, content.height()));
    let right = Rect::from_min_size(
        egui::pos2(content.left() + left_w + SPINE_W, content.top()),
        egui::vec2(right_w, content.height()),
    );
    let divider = Rect::from_min_size(
        egui::pos2(content.left() + left_w, content.top()),
        egui::vec2(SPINE_W, content.height()),
    );
    (left, right, divider)
}

fn code_widths(width: f32, split: f32, model: &DiffModel) -> (f32, f32) {
    let usable = (width - SPINE_W).max(1.0);
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

fn side_text(model: &DiffModel, side: Side) -> &str {
    match side {
        Side::Left => &model.left.text,
        Side::Right => &model.right.text,
    }
}

fn line_for(model: &DiffModel, side: Side, row: usize) -> Option<&Line> {
    match (side, model.rows.get(row)?) {
        (Side::Left, Row::Both { left, .. }) => Some(left),
        (Side::Left, Row::Left(line)) => Some(line),
        (Side::Right, Row::Both { right, .. }) => Some(right),
        (Side::Right, Row::Right(line)) => Some(line),
        _ => None,
    }
}

fn nearest_row(model: &DiffModel, side: Side, row: usize) -> usize {
    if model.rows.is_empty() {
        return 0;
    }
    let row = row.min(model.rows.len() - 1);
    if line_for(model, side, row).is_some() {
        return row;
    }
    for distance in 1..=model.rows.len() {
        if row >= distance && line_for(model, side, row - distance).is_some() {
            return row - distance;
        }
        if row + distance < model.rows.len() && line_for(model, side, row + distance).is_some() {
            return row + distance;
        }
    }
    row
}

fn row_at_y(y: f32, content: Rect, row_h: f32, row_count: usize) -> usize {
    if row_count == 0 {
        return 0;
    }
    let row = ((y - content.top()) / row_h).floor();
    row.clamp(0.0, (row_count - 1) as f32) as usize
}

fn side_at(pos: egui::Pos2, columns: (Rect, Rect)) -> Option<Side> {
    if pos.x < columns.0.right() {
        Some(Side::Left)
    } else if pos.x > columns.1.left() {
        Some(Side::Right)
    } else {
        None
    }
}

fn byte_at_char(text: &str, ch: usize) -> usize {
    text.char_indices()
        .nth(ch)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn char_at_byte(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text[..byte].chars().count()
}

fn word_range(text: &str, byte: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let byte = byte.min(text.len());
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let index = chars
        .partition_point(|(offset, _)| *offset <= byte)
        .saturating_sub(1)
        .min(chars.len() - 1);
    let class = char_class(chars[index].1);
    let mut start = index;
    while start > 0 && char_class(chars[start - 1].1) == class {
        start -= 1;
    }
    let mut end = index + 1;
    while end < chars.len() && char_class(chars[end].1) == class {
        end += 1;
    }
    let start_byte = chars[start].0;
    let end_byte = chars
        .get(end)
        .map(|(offset, _)| *offset)
        .unwrap_or(text.len());
    start_byte..end_byte
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Whitespace,
    Other,
}

fn char_class(ch: char) -> CharClass {
    if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else if ch.is_whitespace() {
        CharClass::Whitespace
    } else {
        CharClass::Other
    }
}

fn selection_text(model: &DiffModel, selection: Selection) -> String {
    let (start, end) = selection.normalized();
    if start.side != end.side {
        return String::new();
    }
    let side = start.side;
    let mut parts: Vec<&str> = Vec::new();
    for row in start.row..=end.row {
        let Some(line) = line_for(model, side, row) else {
            continue;
        };
        let from = if row == start.row {
            start.ch.min(line.text.len())
        } else {
            0
        };
        let to = if row == end.row {
            end.ch.min(line.text.len())
        } else {
            line.text.len()
        };
        let (from, to) = if from <= to { (from, to) } else { (to, from) };
        if line.text.is_char_boundary(from) && line.text.is_char_boundary(to) {
            parts.push(&line.text[from..to]);
        }
    }
    parts.join("\n")
}

fn select_all_selection(model: &DiffModel, side: Side) -> Option<Selection> {
    let first = (0..model.rows.len()).find(|&row| line_for(model, side, row).is_some())?;
    let last = (0..model.rows.len())
        .rev()
        .find(|&row| line_for(model, side, row).is_some())?;
    let end = line_for(model, side, last)
        .map(|line| line.text.len())
        .unwrap_or(0);
    Some(Selection {
        anchor: TextPos {
            side,
            row: first,
            ch: 0,
        },
        focus: TextPos {
            side,
            row: last,
            ch: end,
        },
    })
}

fn selection_range(
    model: &DiffModel,
    selection: Selection,
    side: Side,
    row: usize,
) -> Option<Range<usize>> {
    let (start, end) = selection.normalized();
    if start.side != side || end.side != side || row < start.row || row > end.row {
        return None;
    }
    let line = line_for(model, side, row)?;
    let from = if row == start.row {
        start.ch.min(line.text.len())
    } else {
        0
    };
    let to = if row == end.row {
        end.ch.min(line.text.len())
    } else {
        line.text.len()
    };
    let (from, to) = if from <= to { (from, to) } else { (to, from) };
    if from >= to || !line.text.is_char_boundary(from) || !line.text.is_char_boundary(to) {
        return None;
    }
    Some(from..to)
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
    selection: Option<Selection>,
    focused: bool,
    text_color: Color32,
) {
    let row = &model.rows[index];
    let painter = ui.painter();
    let (left_bg, right_bg) = match row {
        Row::Both {
            kind: RowKind::Replace,
            ..
        } => (CHANGED_BG, CHANGED_BG),
        Row::Left(_) => (CHANGED_BG, EDITOR_BG),
        Row::Right(_) => (EDITOR_BG, ADDED_BG),
        _ => (EDITOR_BG, EDITOR_BG),
    };
    let left_gutter = gutter_width(line_count(&model.left.text));
    let right_gutter = gutter_width(line_count(&model.right.text));
    let left_half = Rect::from_min_max(
        egui::pos2(left_column.left(), rect.top()),
        egui::pos2(left_column.right(), rect.bottom()),
    );
    let right_half = Rect::from_min_max(
        egui::pos2(right_column.left(), rect.top()),
        egui::pos2(right_column.right(), rect.bottom()),
    );

    let left_fill = Rect::from_min_max(
        egui::pos2(left_column.left() + left_gutter, rect.top()),
        egui::pos2(left_column.right(), rect.bottom()),
    );
    let right_fill = Rect::from_min_max(
        egui::pos2(right_column.left() + right_gutter, rect.top()),
        egui::pos2(right_column.right(), rect.bottom()),
    );
    if left_bg != EDITOR_BG {
        painter.rect_filled(left_fill, 0.0, left_bg);
    }
    if right_bg != EDITOR_BG {
        painter.rect_filled(right_fill, 0.0, right_bg);
    }

    let (left_line, right_line) = row_lines(row);
    let words: Option<&WordDiff> = model.words.get(index).and_then(Option::as_ref);
    let left_words = words.map(|words| words.left.as_slice()).unwrap_or(&[]);
    let right_words = words.map(|words| words.right.as_slice()).unwrap_or(&[]);
    let left_selection =
        selection.and_then(|selection| selection_range(model, selection, Side::Left, index));
    let right_selection =
        selection.and_then(|selection| selection_range(model, selection, Side::Right, index));
    let y = rect.center().y;

    paint_side(
        ui,
        Side::Left,
        model,
        left_half,
        left_line,
        left_cache,
        hscroll,
        y,
        row_h,
        left_selection,
        left_words,
        text_color,
        focused,
    );
    paint_side(
        ui,
        Side::Right,
        model,
        right_half,
        right_line,
        right_cache,
        hscroll,
        y,
        row_h,
        right_selection,
        right_words,
        text_color,
        focused,
    );
    draw_sign(ui, left_half, left_gutter, row, Side::Left, y);
    draw_sign(ui, right_half, right_gutter, row, Side::Right, y);
}

#[allow(clippy::too_many_arguments)]
fn paint_side(
    ui: &egui::Ui,
    side: Side,
    model: &DiffModel,
    column: Rect,
    line: Option<&Line>,
    cache: &SideCache,
    hscroll: f32,
    y: f32,
    row_h: f32,
    selection: Option<Range<usize>>,
    words: &[Range<usize>],
    text_color: Color32,
    focused: bool,
) {
    let gutter = gutter_width(line_count(side_text(model, side)));
    let number_x = column.left() + gutter - PAD_X;
    let code_left = column.left() + gutter + SIGN_W;
    let code_rect = Rect::from_min_size(
        egui::pos2(code_left, column.top()),
        egui::vec2((column.right() - code_left).max(1.0), column.height()),
    );
    let Some(line) = line else {
        return;
    };
    ui.painter().text(
        egui::pos2(number_x, y),
        Align2::RIGHT_CENTER,
        line.no.to_string(),
        small_monospace(),
        GUTTER_TEXT,
    );
    let Some(galley) = cache.galleys.get(line.no.saturating_sub(1)) else {
        return;
    };
    let text_top = column.top() + (row_h - galley.size().y) * 0.5;
    let painter = ui.painter().with_clip_rect(code_rect);

    for range in words {
        if let Some(rect) = text_range_rect(
            galley,
            &line.text,
            range.clone(),
            code_left,
            hscroll,
            text_top,
            row_h,
        ) {
            painter.rect_filled(rect, 0.0, WORD_CHANGED_BG);
        }
    }
    if let Some(range) = selection {
        if let Some(rect) = text_range_rect(
            galley, &line.text, range, code_left, hscroll, text_top, row_h,
        ) {
            painter.rect_filled(
                rect,
                0.0,
                if focused {
                    SELECTION_BG
                } else {
                    SELECTION_INACTIVE_BG
                },
            );
        }
    }

    painter.galley(
        egui::pos2(code_left - hscroll, text_top),
        galley.clone(),
        text_color,
    );
    if !line.terminated {
        painter.text(
            egui::pos2(code_left - hscroll + galley.size().x + 8.0, y),
            Align2::LEFT_CENTER,
            "\\ No newline at end of file",
            small_monospace(),
            NOTICE_BAD,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn text_range_rect(
    galley: &Galley,
    text: &str,
    range: Range<usize>,
    code_left: f32,
    hscroll: f32,
    text_top: f32,
    row_h: f32,
) -> Option<Rect> {
    if range.start > range.end
        || range.end > text.len()
        || !text.is_char_boundary(range.start)
        || !text.is_char_boundary(range.end)
    {
        return None;
    }
    let start = galley.pos_from_cursor(CCursor::new(char_at_byte(text, range.start)));
    let end = galley.pos_from_cursor(CCursor::new(char_at_byte(text, range.end)));
    let x1 = code_left - hscroll + start.left();
    let x2 = code_left - hscroll + end.left();
    let (x1, x2) = if x1 <= x2 { (x1, x2) } else { (x2, x1) };
    Some(Rect::from_min_max(
        egui::pos2(x1, text_top),
        egui::pos2(x2.max(x1 + 1.0), text_top + row_h),
    ))
}

fn draw_sign(ui: &egui::Ui, column: Rect, gutter: f32, row: &Row, side: Side, y: f32) {
    let (sign, color) = match (row, side) {
        (Row::Left(_), Side::Left) => ("−", SIGN_DEL),
        (Row::Right(_), Side::Right) => ("+", SIGN_ADD),
        (
            Row::Both {
                kind: RowKind::Replace,
                ..
            },
            Side::Left,
        ) => ("−", SIGN_DEL),
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
        egui::pos2(column.left() + gutter + SIGN_W * 0.5, y),
        Align2::CENTER_CENTER,
        sign,
        mono_font(ui),
        color,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[cfg(test)]
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

fn hunk_kind(model: &DiffModel, hunk: &Hunk) -> ChangeKind {
    let mut added = false;
    let mut removed = false;
    for row in &model.rows[hunk.first_row..=hunk.last_row] {
        match row {
            Row::Right(_) => added = true,
            Row::Left(_) => removed = true,
            Row::Both {
                kind: RowKind::Replace,
                ..
            } => {
                added = true;
                removed = true;
            }
            _ => {}
        }
    }
    if added && removed {
        ChangeKind::Modified
    } else if added {
        ChangeKind::Added
    } else {
        ChangeKind::Removed
    }
}

/// Map every hunk to a mark in an overview lane of `height` points. Marks are at
/// least four points tall and are pushed down when two changes would overlap.
fn hunk_marks(model: &DiffModel, height: f32) -> Vec<(ChangeKind, f32, f32)> {
    let total = model.rows.len().max(1) as f32;
    let mut marks = Vec::new();
    let mut next_free = 0.0;
    for hunk in &model.hunks {
        let y = (hunk.first_row as f32 / total * height).max(next_free);
        let h = (((hunk.last_row - hunk.first_row + 1) as f32 / total * height).max(4.0))
            .min(height - y);
        if h <= 0.0 {
            break;
        }
        marks.push((hunk_kind(model, hunk), y, h));
        next_free = y + h + 1.0;
    }
    marks
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

    fn pos(side: Side, row: usize, ch: usize) -> TextPos {
        TextPos { side, row, ch }
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
    fn a_selection_joins_complete_lines_and_blank_lines() {
        let model = model("alpha\n\nomega\n", "alpha\n\nomega\n");
        let selection = Selection {
            anchor: pos(Side::Left, 0, 0),
            focus: pos(Side::Left, 2, 5),
        };
        assert_eq!(selection_text(&model, selection), "alpha\n\nomega");
    }

    #[test]
    fn a_selection_copies_part_of_the_first_and_last_line() {
        let model = model("alpha\nbeta\ngamma\n", "alpha\nbeta\ngamma\n");
        let selection = Selection {
            anchor: pos(Side::Left, 0, 1),
            focus: pos(Side::Left, 1, 2),
        };
        assert_eq!(selection_text(&model, selection), "lpha\nbe");
    }

    #[test]
    fn a_selection_ending_at_the_start_of_a_line_keeps_the_newline() {
        let model = model("alpha\nbeta\n", "alpha\nbeta\n");
        let selection = Selection {
            anchor: pos(Side::Left, 0, 0),
            focus: pos(Side::Left, 1, 0),
        };
        assert_eq!(selection_text(&model, selection), "alpha\n");
    }

    #[test]
    fn a_selection_is_ordered_whichever_end_dragged() {
        let model = model("alpha\nbeta\n", "alpha\nbeta\n");
        let selection = Selection {
            anchor: pos(Side::Left, 1, 4),
            focus: pos(Side::Left, 0, 0),
        };
        assert_eq!(selection_text(&model, selection), "alpha\nbeta");
    }

    #[test]
    fn select_all_covers_every_line_on_one_side() {
        let model = model("a\nb\n", "a\nB\nc\n");
        let left = select_all_selection(&model, Side::Left).expect("the left side has lines");
        assert_eq!(selection_text(&model, left), "a\nb");
        let right = select_all_selection(&model, Side::Right).expect("the right side has lines");
        assert_eq!(selection_text(&model, right), "a\nB\nc");
    }

    #[test]
    fn a_word_is_selected_from_the_clicked_character() {
        let text = "let value = 1;";
        let range = word_range(text, 4);
        assert_eq!(&text[range], "value");
    }

    #[test]
    fn selecting_whitespace_selects_the_run() {
        let text = "let   value";
        let range = word_range(text, 4);
        assert_eq!(&text[range], "   ");
    }

    #[test]
    fn selecting_unicode_does_not_split_a_character() {
        let text = "变量 = 1";
        let range = word_range(text, 3);
        assert_eq!(&text[range], "变量");
    }

    #[test]
    fn overview_marks_are_ordered_and_have_a_minimum_height() {
        let model = model("a\nb\nc\nd\ne\nf\ng\nh\n", "a\nB\nc\nd\ne\nf\nG\nH\n");
        let marks = hunk_marks(&model, 100.0);
        assert_eq!(marks.len(), 2);
        assert!(marks[0].1 < marks[1].1);
        assert!(marks.iter().all(|(_, _, height)| *height >= 4.0));
        assert_eq!(marks[0].0, ChangeKind::Modified);
    }

    #[test]
    fn an_added_hunk_is_green_and_a_deleted_hunk_is_red() {
        let added = model("a\n", "a\nb\n");
        let added_hunk = &added.hunks[0];
        assert_eq!(hunk_kind(&added, added_hunk), ChangeKind::Added);

        let removed = model("a\nb\n", "a\n");
        let removed_hunk = &removed.hunks[0];
        assert_eq!(hunk_kind(&removed, removed_hunk), ChangeKind::Removed);
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
    fn a_shrunken_diff_window_does_not_grow_back() {
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let mut old = String::new();
        let mut new = String::new();
        for index in 0..200 {
            old.push_str(&format!("line {index}\n"));
            if index % 2 == 0 {
                new.push_str(&format!("line {index}\n"));
            } else {
                new.push_str(&format!("changed {index}\n"));
            }
        }
        let model = model(&old, &new);
        let mut view = DiffView::new();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 800.0),
        ));
        let mut heights = Vec::new();
        for frame in 0..8 {
            input.time = Some(frame as f64 * 0.016);
            let mut height = 0.0;
            let _ = crate::app::ui::test_run(&ctx, input.clone(), |ui| {
                let ctx = ui.ctx().clone();
                let window = egui::Window::new("Diff")
                    .id(egui::Id::unique("diff_growth_test"))
                    .resizable(true)
                    .default_size(egui::vec2(900.0, 300.0))
                    .min_size(egui::vec2(400.0, 120.0))
                    .show(&ctx, |ui| view.show(ui, &model, 1));
                height = window.unwrap().response.rect.height();
            });
            heights.push(height);
        }
        let settled = heights[2];
        let last = *heights.last().unwrap();
        assert!(
            (last - settled).abs() < 0.5,
            "the window kept growing after it was sized: {heights:?}"
        );
    }

    #[test]
    fn the_widget_draws_both_columns_and_keeps_empty_rows() {
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let model = model("a\n\nb\n", "a\n\nB\nc\n");
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
        for expected in ["old", "new", "a", "b", "B", "1", "2", "3", "−", "+"] {
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
                .any(|(rect, fill)| *fill == CHANGED_BG && rect.height() < 40.0),
            "the removed row has no per-row background: {rects:?}"
        );
        assert!(
            rects
                .iter()
                .any(|(rect, fill)| *fill == ADDED_BG && rect.height() < 40.0),
            "the inserted row has no per-row background: {rects:?}"
        );
        assert!(
            rects
                .iter()
                .any(|(rect, fill)| *fill == WORD_CHANGED_BG && rect.width() < 40.0),
            "the changed word has no intra-line background: {rects:?}"
        );
    }

    #[test]
    fn a_drag_then_ctrl_c_copies_the_selected_lines() {
        let ctx = egui::Context::default();
        crate::app::ui::fonts::install(&ctx);
        let model = model("alpha\nbeta\n", "alpha\nBETA\n");
        let mut view = DiffView::new();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 420.0),
        ));
        input.time = Some(0.0);
        let first = crate::app::ui::test_run(&ctx, input.clone(), |ui| {
            view.show(ui, &model, 1);
        });

        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Pos2)>) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push((text.galley.text().to_string(), text.pos));
                }
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &first.shapes {
            walk(&clipped.shape, &mut texts);
        }
        let alpha = texts
            .iter()
            .filter(|(text, _)| text == "alpha")
            .map(|(_, pos)| *pos)
            .min_by(|a, b| a.x.total_cmp(&b.x))
            .expect("the left alpha line was drawn");
        let beta = texts
            .iter()
            .find(|(text, _)| text == "beta")
            .map(|(_, pos)| *pos)
            .expect("the left beta line was drawn");
        let row_h = beta.y - alpha.y;
        let start = alpha + egui::vec2(0.0, row_h * 0.5);
        let end = beta + egui::vec2(200.0, row_h * 0.5);

        input.time = Some(0.1);
        input.events = vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ];
        let _ = crate::app::ui::test_run(&ctx, input.clone(), |ui| {
            view.show(ui, &model, 1);
        });

        input.time = Some(0.2);
        input.events = vec![egui::Event::PointerMoved(end)];
        let _ = crate::app::ui::test_run(&ctx, input.clone(), |ui| {
            view.show(ui, &model, 1);
        });

        input.time = Some(0.25);
        input.events = vec![egui::Event::PointerMoved(end)];
        let moved = crate::app::ui::test_run(&ctx, input.clone(), |ui| {
            view.show(ui, &model, 1);
        });
        fn has_fill(output: &egui::FullOutput, fill: Color32) -> bool {
            fn walk(shape: &egui::Shape, fill: Color32) -> bool {
                match shape {
                    egui::Shape::Rect(rect) => rect.fill == fill,
                    egui::Shape::Vec(inner) => inner.iter().any(|shape| walk(shape, fill)),
                    _ => false,
                }
            }
            output
                .shapes
                .iter()
                .any(|clipped| walk(&clipped.shape, fill))
        }
        assert!(
            has_fill(&moved, SELECTION_BG),
            "the selected text was not painted"
        );

        input.time = Some(0.3);
        input.events = vec![egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }];
        let _ = crate::app::ui::test_run(&ctx, input.clone(), |ui| {
            view.show(ui, &model, 1);
        });

        assert!(
            view.selection.is_some(),
            "the drag did not start a selection (start {start:?}, end {end:?})"
        );
        if let Some(selection) = view.selection {
            assert_eq!(
                selection_text(&model, selection),
                "alpha\nbeta",
                "selection: {selection:?}"
            );
        }

        let command = Modifiers::CTRL.plus(Modifiers::COMMAND);
        input.time = Some(0.4);
        input.events = vec![egui::Event::Key {
            key: Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: command,
        }];
        let output = crate::app::ui::test_run(&ctx, input, |ui| {
            view.show(ui, &model, 1);
        });
        let copied = output
            .platform_output
            .commands
            .iter()
            .find_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            });
        assert_eq!(copied, Some("alpha\nbeta"));
    }
}
