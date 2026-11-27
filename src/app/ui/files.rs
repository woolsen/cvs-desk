//! The address bar and the file list.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cvsdesk::cvs::Cmd;
use cvsdesk::entries::{self, Origin, Row, State};
use cvsdesk::filekind::FileKind;
use cvsdesk::prefs::ViewScope;
use cvsdesk::util::truncate_columns;

use super::{fonts, icons, state_color, typography};
use crate::app::files::{display_name, search_id, FilterPreset, Filters, SortKey};
use crate::app::{App, RemoteState};

/// What a nav-bar button asked for, handled after the row has been drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NavAction {
    Back,
    Forward,
    Up,
    Reload,
}

/// What the file list's context menu asked for. The menu only records the choice;
/// the list applies it after the rows have been drawn, where it can borrow the window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FileMenuAction {
    Open,
    Update,
    Commit,
    Diff,
    History,
    Tags,
    Status,
    CopyName,
    CopyPath,
    ShowInExplorer,
    MoveToTrash,
}

/// The glyphs on the nav buttons, from the icon font installed by [`fonts::install`].
const BACK: &str = egui_remixicon::icons::ARROW_LEFT_LINE;
const FORWARD: &str = egui_remixicon::icons::ARROW_RIGHT_LINE;
const UP: &str = egui_remixicon::icons::ARROW_UP_LINE;
const RELOAD: &str = egui_remixicon::icons::REFRESH_LINE;
const SEARCH: &str = egui_remixicon::icons::SEARCH_LINE;
const CLOSE: &str = egui_remixicon::icons::CLOSE_LINE;
const FILTER: &str = egui_remixicon::icons::FILTER_3_LINE;

/// How much of the nav bar the search field takes when it is open.
const SEARCH_FIELD_W: f32 = 200.0;
/// Room for the icon button that stays at the far right (search when closed, close
/// when open). The button's real width comes from its padding, so leave a little air.
const SEARCH_BUTTON_W: f32 = 30.0;
/// Room for the funnel button at the far right that folds the filter bar away.
const FILTER_BUTTON_W: f32 = 30.0;

/// The icon's inset in the name column, and the fixed widths after the name. The
/// headings and the rows both take their x positions from [`ListGeometry`], so a heading
/// always sits over the column it names.
const ICON_INSET: f32 = 4.0;
const STATE_W: f32 = 130.0;
/// A little wider than "Revision" needs, so its sort arrow fits.
const REVISION_W: f32 = 72.0;
const OPTS_W: f32 = 48.0;
const DATE_W: f32 = 134.0;
/// The sticky-tag glyph sits in the right end of the revision cell, so the column
/// keeps its width whether or not the folder is pinned.
const STICKY_ICON_SIZE: f32 = 12.0;
const STICKY_ICON_W: f32 = 14.0;
const STICKY_ICON_GAP: f32 = 3.0;
/// The name keeps at least this much even in a narrow window.
const MIN_NAME_W: f32 = 80.0;

/// The heading band, its rule, the faint column rules, and the full-cell hover that
/// marks a heading as clickable.
const HEADER_BG: egui::Color32 = egui::Color32::from_gray(244);
const HEADER_LINE: egui::Color32 = egui::Color32::from_gray(206);
const HEADER_RULE: egui::Color32 = egui::Color32::from_gray(228);
const HEADER_TEXT: egui::Color32 = egui::Color32::from_gray(74);
const HEADER_TEXT_ACTIVE: egui::Color32 = egui::Color32::from_gray(24);
const HEADER_HOVER: egui::Color32 = egui::Color32::from_gray(236);
const HEADER_ARROW_SIZE: f32 = 12.0;
const HEADER_ARROW_GAP: f32 = 4.0;

/// A selected row's band and a hovered row's band. The list paints the full band, so
/// a run of selected rows reads as one block instead of a stack of stripes.
const ROW_SELECTED_BG: egui::Color32 = egui::Color32::from_rgb(204, 224, 247);
const ROW_HOVER_BG: egui::Color32 = egui::Color32::from_rgb(238, 244, 251);
/// The sticky-tag glyph and its folder line; the same muted indigo the tag column used.
const STICKY_COLOR: egui::Color32 = egui::Color32::from_rgb(90, 90, 120);

/// Column positions inside one row, measured from the row's left edge. The fixed
/// columns sum to the window width, so nothing drifts or overflows at the right edge.
struct ListGeometry {
    name_w: f32,
    state_x: f32,
    revision_x: f32,
    opts_x: f32,
    date_x: f32,
}

impl ListGeometry {
    fn for_width(width: f32) -> Self {
        let name_text_x = ICON_INSET + icons::GAP;
        let fixed = STATE_W + REVISION_W + OPTS_W + DATE_W;
        let name_w = (width - name_text_x - fixed).max(MIN_NAME_W);
        let state_x = name_text_x + name_w;
        let revision_x = state_x + STATE_W;
        let opts_x = revision_x + REVISION_W;
        let date_x = opts_x + OPTS_W;
        Self {
            name_w,
            state_x,
            revision_x,
            opts_x,
            date_x,
        }
    }
}

/// One heading: the full cell a click can land in, the smaller inset its label starts
/// at, and the sort key it changes (none for the options column).
struct Heading {
    label: &'static str,
    cell_x: f32,
    w: f32,
    text_x: f32,
    key: Option<SortKey>,
}

/// The address bar's stable id, so clicking the path can hand it the keyboard.
fn address_id() -> egui::Id {
    egui::Id::unique("folder-address")
}

/// The padding inside the address and search fields. The painted path is measured
/// against the same padding, so the two share one height.
const FIELD_MARGIN: egui::Vec2 = egui::vec2(6.0, 4.0);

/// The height egui gives a single-line field in this style: the text's row plus the
/// field's own margin. The painted path uses the same number, so opening the field
/// cannot change the bar's height and move the list below it.
fn field_height(ui: &egui::Ui) -> f32 {
    let font = egui::FontSelection::Default.resolve(ui.style());
    let row = ui
        .painter()
        .layout_no_wrap("0".to_owned(), font, egui::Color32::WHITE)
        .size()
        .y;
    row + 2.0 * FIELD_MARGIN.y
}

/// The scroll offset that puts `name` back at the viewport top after the order was rebuilt.
fn anchored_offset(
    order: &[usize],
    rows: &[Row],
    name: &str,
    within: f32,
    row_h: f32,
) -> Option<f32> {
    let pos = order
        .iter()
        .position(|&index| rows.get(index).is_some_and(|row| row.name == name))?;
    Some(pos as f32 * row_h + within)
}

impl App {
    /// The address bar above the list: back, forward, up, reload, the path, and the
    /// two selection shortcuts that used to sit in the list's header.
    fn ui_nav_bar(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<NavAction> = None;
        let mut submit = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let button = |ui: &mut egui::Ui, enabled: bool, glyph: &str, tip: &str| {
                ui.add_enabled(
                    enabled,
                    egui::Button::new(
                        egui::RichText::new(glyph).font(fonts::icon_font(icons::SIZE)),
                    )
                    .min_size(egui::vec2(24.0, 22.0)),
                )
                .on_hover_text(tip)
                .clicked()
            };

            if button(ui, self.history.can_back(), BACK, "Back") {
                action = Some(NavAction::Back);
            }
            if button(ui, self.history.can_forward(), FORWARD, "Forward") {
                action = Some(NavAction::Forward);
            }
            if button(ui, self.can_go_up(), UP, "Up") {
                action = Some(NavAction::Up);
            }
            if button(ui, true, RELOAD, "Reload this folder") {
                action = Some(NavAction::Reload);
            }
            ui.separator();

            // The search control always sits at the far right, and the funnel that
            // folds the filter bar away sits just right of it. The address bar takes
            // everything else, and is measured against the controls' real width so no
            // empty gutter is left at the edge.
            let search_width = if self.search_open {
                SEARCH_FIELD_W + 3.0 + SEARCH_BUTTON_W
            } else {
                SEARCH_BUTTON_W
            };
            let controls_width = search_width + 3.0 + FILTER_BUTTON_W;
            let field_width = (ui.available_width() - controls_width - 9.0).max(140.0);
            if self.address_editing {
                let id = address_id();
                if self.focus_address {
                    self.focus_address = false;
                    // The field below is drawn on this frame, so the id handed to
                    // AccessKit at the end of the frame is in the tree it describes.
                    ui.memory_mut(|m| m.request_focus(id));
                }
                let response = ui.add_sized(
                    [field_width, field_height(ui)],
                    egui::TextEdit::singleline(&mut self.address)
                        .id(id)
                        .hint_text("Folder path")
                        .margin(FIELD_MARGIN),
                );
                let entered =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let escaped =
                    response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
                if entered {
                    submit = true;
                } else if response.lost_focus() || escaped {
                    if escaped {
                        ui.memory_mut(|m| m.surrender_focus(id));
                    }
                    // Clicking away abandons an edit; the bar always names what is shown.
                    self.address = self.dir.display().to_string();
                    self.address_editing = false;
                }
                response.on_hover_text("Type a folder path and press Enter. Esc cancels.");
            } else {
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(field_width, field_height(ui)),
                    egui::Sense::click(),
                );
                let visuals = *ui.style().interact(&response);
                // The same frame egui paints around a text field: fill, rounding and the
                // widget's own expansion. A click changes the stroke's color, not the
                // frame's size, exactly as it does for the field.
                let frame = rect.expand(visuals.expansion);
                ui.painter().rect(
                    frame,
                    visuals.corner_radius,
                    ui.visuals().extreme_bg_color,
                    visuals.bg_stroke,
                    egui::StrokeKind::Inside,
                );
                let font = egui::TextStyle::Body.resolve(ui.style());
                // A text field keeps its text color on hover, so the path does too.
                let text_color = ui.visuals().widgets.inactive.text_color();
                let shown = address_label(ui, &self.dir, field_width - 2.0 * FIELD_MARGIN.x, &font);
                let galley = ui.painter().layout_no_wrap(shown, font, text_color);
                let pos = egui::pos2(
                    rect.left() + FIELD_MARGIN.x,
                    rect.center().y - galley.size().y / 2.0,
                );
                ui.painter()
                    .with_clip_rect(rect.shrink(2.0))
                    .galley(pos, galley, text_color);
                if response.clicked() {
                    self.address = self.dir.display().to_string();
                    self.address_editing = true;
                    self.focus_address = true;
                }
                response
                    .on_hover_cursor(egui::CursorIcon::Text)
                    .on_hover_text(format!("{}\nClick to type a path.", self.dir.display()));
            }

            ui.add_space(6.0);
            // A right-to-left row pins the controls to the right edge; the field
            // inside the search control keeps its own left-to-right text.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.ui_filter_toggle(ui);
                self.ui_search_box(ui);
            });
        });
        match action {
            Some(NavAction::Back) => self.go_back(),
            Some(NavAction::Forward) => self.go_forward(),
            Some(NavAction::Up) => self.go_up(),
            Some(NavAction::Reload) => self.reload(),
            None => {}
        }
        if submit && !self.submit_address() {
            // An invalid path leaves the field open with the typed text, so a typo can
            // be corrected instead of silently snapping back to the old folder.
            ui.memory_mut(|m| m.request_focus(address_id()));
        }
    }

    /// The filter button at the far right of the nav bar. The filter row above the
    /// list is only shown while this is open, so the list keeps its height by default.
    /// A filter that is not the default keeps the button lit while the row is folded,
    /// so a hidden row never hides the fact that the list is filtered.
    fn ui_filter_toggle(&mut self, ui: &mut egui::Ui) {
        let preset = self.filters.preset();
        let filtered = !self.filters.is_default();
        let response = ui
            .add(
                egui::Button::new(egui::RichText::new(FILTER).font(fonts::icon_font(icons::SIZE)))
                    .min_size(egui::vec2(24.0, 22.0))
                    .selected(self.filters_open || filtered),
            )
            .on_hover_text(if self.filters_open {
                format!("Hide the filter bar. Filter: {}.", preset.label())
            } else if filtered {
                format!(
                    "Show the filter bar. Filter: {} — the list is filtered.",
                    preset.label()
                )
            } else {
                "Show the filter bar: task presets, scope and CVS states.".to_owned()
            });
        if response.clicked() {
            self.filters_open = !self.filters_open;
        }
    }

    /// The search control at the far right of the nav bar. Closed it is one magnifier
    /// button; open it grows a field to the button's left and the button becomes the
    /// close control. Closing always clears the filter, so a closed box and a filtered
    /// list can never disagree.
    fn ui_search_box(&mut self, ui: &mut egui::Ui) {
        let id = search_id();
        let icon_button = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
            ui.add(
                egui::Button::new(egui::RichText::new(glyph).font(fonts::icon_font(icons::SIZE)))
                    .min_size(egui::vec2(24.0, 22.0)),
            )
            .on_hover_text(tip)
            .clicked()
        };

        if !self.search_open {
            if icon_button(ui, SEARCH, "Search this folder") {
                self.open_search();
            }
            return;
        }

        // Right-to-left order: the close button is drawn first, so it stays at the
        // right edge and the field grows to its left.
        if icon_button(ui, CLOSE, "Close the search and clear the filter (Esc)") {
            self.close_search(ui.ctx());
            return;
        }

        if self.focus_search {
            self.focus_search = false;
            // Select what is already there, so typing replaces it.
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(self.search.chars().count()),
                );
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), id);
            }
            ui.memory_mut(|m| m.request_focus(id));
        }

        let response = ui.add_sized(
            [SEARCH_FIELD_W, field_height(ui)],
            egui::TextEdit::singleline(&mut self.search)
                .id(id)
                .hint_text("Search this folder")
                .align(egui::Align2::LEFT_CENTER)
                .margin(FIELD_MARGIN),
        );
        if response.changed() {
            self.order_dirty = true;
        }
        // Clicking into the list while the box is still empty folds it back into the
        // button; with text in it, the box stays open so the filter remains visible.
        if response.lost_focus() && self.search.is_empty() {
            self.search_open = false;
        }

        let shown = self.order.len();
        let total = self.rows.len();
        response.on_hover_text(format!(
            "{shown} of {total} rows match. Case-insensitive; * and ? are wildcards. Esc closes."
        ));
    }

    /// The heading band. One flat strip, not a row of buttons: each label starts over
    /// its data column, the sortable cells take a click and a hover tint, and the active
    /// cell carries a small up or down arrow after its name.
    fn ui_list_header(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(width, typography::LIST_HEADER_H),
            egui::Sense::hover(),
        );
        let g = ListGeometry::for_width(width);
        let headings = [
            Heading {
                label: "File",
                cell_x: 0.0,
                w: g.state_x,
                text_x: ICON_INSET,
                key: Some(SortKey::Name),
            },
            Heading {
                label: "State",
                cell_x: g.state_x,
                w: STATE_W,
                text_x: g.state_x,
                key: Some(SortKey::State),
            },
            Heading {
                label: "Revision",
                cell_x: g.revision_x,
                w: REVISION_W,
                text_x: g.revision_x,
                key: Some(SortKey::Revision),
            },
            Heading {
                label: "Opts",
                cell_x: g.opts_x,
                w: OPTS_W,
                text_x: g.opts_x,
                key: None,
            },
            Heading {
                label: "Recorded",
                cell_x: g.date_x,
                w: DATE_W,
                text_x: g.date_x,
                key: Some(SortKey::Date),
            },
        ];

        ui.painter().rect_filled(rect, 0.0, HEADER_BG);
        // A click must land in the cell under the pointer, not in the neighbour egui's
        // 5 px search radius offers, and a window drawn over the band must take the click
        // itself. The exact cell test keeps the first rule; `Response::clicked` keeps the
        // second, because egui only gives it to the widget in the topmost layer.
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let mut clicked = None;
        for h in &headings {
            let cell = egui::Rect::from_min_size(
                egui::pos2(rect.left() + h.cell_x, rect.top()),
                egui::vec2(h.w, rect.height()),
            );
            let exact_hover = pointer.is_some_and(|p| cell.contains(p));
            let sense = if h.key.is_some() {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let response = ui.interact(cell, ui.scope_id().with(("list-heading", h.label)), sense);
            match h.key {
                Some(key) => {
                    let sorted = self.effective_sort() == key;
                    let tip = if sorted {
                        format!(
                            "Sorted by {}, {}. Click to reverse.",
                            h.label,
                            if self.sort_asc {
                                "ascending"
                            } else {
                                "descending"
                            }
                        )
                    } else {
                        format!("Sort by {}", h.label)
                    };
                    if exact_hover {
                        ui.painter().rect_filled(cell, 0.0, HEADER_HOVER);
                    }
                    let response = if exact_hover {
                        response.on_hover_text(tip)
                    } else {
                        response
                    };
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Button, true, sorted, h.label)
                    });
                    if response.clicked() && exact_hover {
                        clicked = Some(key);
                    }
                }
                None => {
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::Label, true, h.label)
                    });
                    if exact_hover {
                        response.on_hover_text(
                            "The entry's keyword options, e.g. -kb (binary) or -ku (unicode).",
                        );
                    }
                }
            }
        }

        // Rules sit above the labels: a bottom line closes the strip, and faint column
        // rules keep five columns legible without turning the band into a grid.
        let painter = ui.painter();
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0_f32, HEADER_LINE),
        );
        for h in &headings[1..] {
            painter.vline(
                rect.left() + h.cell_x,
                rect.y_range().shrink(3.0),
                egui::Stroke::new(1.0_f32, HEADER_RULE),
            );
        }

        let body = egui::TextStyle::Body.resolve(ui.style());
        for h in &headings {
            let sorted = h.key.is_some_and(|key| self.effective_sort() == key);
            let color = if sorted {
                HEADER_TEXT_ACTIVE
            } else {
                HEADER_TEXT
            };
            let galley = painter.layout_no_wrap(h.label.to_owned(), body.clone(), color);
            let label_w = galley.size().x;
            let pos = egui::pos2(
                rect.left() + h.text_x,
                rect.center().y - galley.size().y / 2.0,
            );
            painter.galley(pos, galley, color);
            if sorted {
                let glyph = if self.sort_asc {
                    egui_remixicon::icons::ARROW_UP_S_FILL
                } else {
                    egui_remixicon::icons::ARROW_DOWN_S_FILL
                };
                painter.text(
                    egui::pos2(pos.x + label_w + HEADER_ARROW_GAP, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    glyph,
                    fonts::icon_font(HEADER_ARROW_SIZE),
                    color,
                );
            }
        }

        if let Some(key) = clicked {
            self.flip_sort(key);
        }
    }

    /// The task presets and the state switches. One menu, so a preset is always a
    /// complete view and a manual change immediately reads as `Custom`.
    fn ui_filter_menu(&mut self, ui: &mut egui::Ui) {
        let preset = self.filters.preset();
        let active = self.filters.filtering().len();
        let label = if active == 0 {
            format!("Filter: {}", preset.label())
        } else {
            format!("Filter: {} ({active})", preset.label())
        };
        ui.menu_button(label, |ui| {
            ui.set_min_width(300.0);
            ui.label(egui::RichText::new("View").small().weak());
            for choice in [
                FilterPreset::WorkingCopy,
                FilterPreset::MyChanges,
                FilterPreset::NeedsAttention,
                FilterPreset::NotCheckedOut,
                FilterPreset::Untracked,
                FilterPreset::AllNames,
            ] {
                if ui
                    .selectable_label(preset == choice, choice.label())
                    .clicked()
                {
                    self.filters.apply_preset(choice);
                    self.order_dirty = true;
                }
            }

            ui.separator();
            ui.label(egui::RichText::new("Scope").small().weak());
            let (local, remote) = self.scope_counts();
            for (scope, label, count) in [
                (ViewScope::WorkingCopy, "Local", local),
                (ViewScope::NotCheckedOut, "Not checked out", remote),
                (ViewScope::All, "All names", local + remote),
            ] {
                if ui
                    .selectable_label(self.filters.scope == scope, format!("{label} ({count})"))
                    .clicked()
                {
                    self.filters.scope = scope;
                    self.order_dirty = true;
                }
            }

            ui.separator();
            ui.label(egui::RichText::new("CVS state").small().weak());
            let local_scope = self.filters.scope != ViewScope::NotCheckedOut;
            for (state, count) in self.filter_state_counts() {
                let label = match state {
                    State::Unchanged => "Up-to-date",
                    State::Modified => "Modified",
                    State::Added => "Added",
                    State::Removed => "Removed",
                    State::Conflict => "Conflicts",
                    State::Unknown => "Untracked",
                    State::Missing => "Missing locally",
                    _ => state.label(),
                };
                let mut on = self.filters.enabled(state);
                let response = ui.add_enabled(
                    local_scope,
                    egui::Checkbox::new(&mut on, format!("{label} ({count})")),
                );
                if response.changed() {
                    self.filters.set_state(state, on);
                    self.order_dirty = true;
                }
            }
            if !local_scope {
                ui.label(
                    egui::RichText::new("Repository names have no local CVS state.")
                        .small()
                        .weak(),
                );
            }

            ui.separator();
            let folder_count = self.rows.iter().filter(|row| row.is_dir).count();
            let mut folders = self.filters.folders;
            if ui
                .checkbox(&mut folders, format!("Sub-folders ({folder_count})"))
                .changed()
            {
                self.filters.folders = folders;
                self.order_dirty = true;
            }
            let mut hide_unknown = self.filters.hide_unknown;
            if ui.checkbox(&mut hide_unknown, "Hide untracked").changed() {
                self.filters.hide_unknown = hide_unknown;
                self.order_dirty = true;
            }
            let mut hide_missing = self.filters.hide_missing;
            if ui.checkbox(&mut hide_missing, "Hide missing").changed() {
                self.filters.hide_missing = hide_missing;
                self.order_dirty = true;
            }

            ui.separator();
            if ui.button("Reset filters").clicked() {
                self.filters = Filters::default();
                self.order_dirty = true;
                ui.close();
            }
        })
        .response
        .on_hover_text(
            "Task presets and state switches. The list always says which view is active.",
        );
    }

    /// The scope bar: the one place that says whether the repository side is hidden,
    /// still loading, or unavailable. It sits above the column headings so the source
    /// of the list is always on screen. The row is folded away by default; the funnel
    /// button in the nav bar unfolds it.
    fn ui_scope_bar(&mut self, ui: &mut egui::Ui) {
        if self.filters_open {
            self.ui_filter_row(ui);
        }
        self.ui_scope_warnings(ui);
    }

    /// The filter row itself: scope, presets and state switches.
    fn ui_filter_row(&mut self, ui: &mut egui::Ui) {
        let (_, remote) = self.scope_counts();
        let remote_label = match self.remote_state {
            RemoteState::Off => "Not checked out (—)".to_string(),
            RemoteState::Loading if self.remote_found > 0 => {
                format!("Not checked out (checking… {})", self.remote_found)
            }
            RemoteState::Loading => "Not checked out (checking…)".to_string(),
            RemoteState::Ready => format!("Not checked out ({remote})"),
            RemoteState::Stopped => format!("Not checked out ({remote} partial)"),
            RemoteState::Failed => "Not checked out (unavailable)".to_string(),
        };
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui
                .selectable_label(
                    self.filters.scope == ViewScope::WorkingCopy,
                    "Local",
                )
                .on_hover_text(
                    "Rows that belong to this local checkout, including missing and untracked files.",
                )
                .clicked()
            {
                chosen = Some(ViewScope::WorkingCopy);
            }
            if ui
                .selectable_label(
                    self.filters.scope == ViewScope::NotCheckedOut,
                    remote_label,
                )
                .on_hover_text(
                    "Repository names that are not in this folder. Read with rlog -R for this folder only.",
                )
                .clicked()
            {
                chosen = Some(ViewScope::NotCheckedOut);
            }
            if ui
                .selectable_label(self.filters.scope == ViewScope::All, "All names")
                .on_hover_text("Local rows together with repository-only names.")
                .clicked()
            {
                chosen = Some(ViewScope::All);
            }
            self.ui_filter_menu(ui);
            ui.separator();
            ui.label(
                egui::RichText::new(format!("{} of {} rows", self.order.len(), self.rows.len()))
                    .small()
                    .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.has_active_filter() && ui.small_button("Clear").clicked() {
                    self.filters = Filters::default();
                    self.search.clear();
                    self.order_dirty = true;
                }
                if !self.selected.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("{} selected", self.selected.len()))
                            .small()
                            .weak(),
                    );
                }
            });
        });
        if let Some(scope) = chosen {
            if scope != self.filters.scope {
                self.filters.scope = scope;
                self.order_dirty = true;
            }
        }
    }

    /// Warnings that stay visible even while the filter row is folded: selection
    /// hidden behind the filters, and a partial checkout that update will not fill.
    fn ui_scope_warnings(&mut self, ui: &mut egui::Ui) {
        let hidden = self.hidden_selected_count();
        if hidden > 0 {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} selected · {} hidden by filters",
                        self.selected.len(),
                        hidden
                    ))
                    .small()
                    .color(egui::Color32::from_rgb(150, 90, 0)),
                );
                if ui.small_button("Clear hidden").clicked() {
                    self.clear_hidden_selection();
                }
            });
        }

        if self.partial_checkout {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(
                        "Partial checkout (CVS/Entries.Static) — Update will not add missing files here.",
                    )
                    .small()
                    .color(egui::Color32::from_rgb(70, 100, 135)),
                )
                .on_hover_text(
                    "CVS marked this folder static, so update will not fetch new files into it.",
                );
            });
        }

        // The folder tag is shown once here, not repeated on every row. A row only gets
        // its own glyph when its tag differs from this one.
        if !self.sticky.is_empty() {
            let full = sticky_banner(&self.sticky);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(sticky_icon(&self.sticky))
                        .font(fonts::icon_font(STICKY_ICON_SIZE))
                        .color(STICKY_COLOR),
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(&full).small().color(STICKY_COLOR))
                        .truncate(),
                )
                .on_hover_text(format!("{full}\nClear it with update -A."));
            });
        }
    }

    pub(super) fn ui_file_list(&mut self, ui: &mut egui::Ui) {
        self.ensure_order();
        if !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A))
        {
            self.select_all_shown();
        }
        self.ui_nav_bar(ui);
        ui.separator();
        self.ui_scope_bar(ui);

        // One heading band, drawn from the same column geometry as the rows.
        self.ui_list_header(ui);

        let mut clicked_at: Option<usize> = None;
        let mut right_clicked_at: Option<usize> = None;
        let mut menu_action: Option<(FileMenuAction, usize)> = None;
        let mut open_dir: Option<PathBuf> = None;
        let mut open_missing_dir: Option<String> = None;
        // The spacing the list would put between rows moves inside the row band, so
        // adjacent selected rows paint as one block; the text keeps its distance and so
        // does the total scroll height.
        let row_gap = ui.spacing().item_spacing.y;
        let row_h = typography::LIST_ROW_H + row_gap;
        // Only vertical: every cell truncates to its column, so horizontal scrolling
        // could never line the rows up with a heading band that stays put.
        let mut restore = None;
        if let Some((name, within)) = self.pending_scroll.take() {
            restore = anchored_offset(&self.order, &self.rows, &name, within, row_h);
        }
        let mut area = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("file-list");
        if let Some(offset) = restore {
            area = area.vertical_scroll_offset(offset);
        }
        let list = ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            area.show_rows(ui, row_h, self.order.len(), |ui, range| {
                    ui.set_min_width(ui.available_width());
                    // Every column is painted from the same body font, and a Chinese letter
                    // takes two of its cells; asking the font keeps truncation in step with
                    // the scale instead of guessing a cell width.
                    let body = egui::TextStyle::Body.resolve(ui.style());
                    let cell = body.size / 2.0;
                    for pos in range {
                        let idx = self.order[pos];
                        let r = &self.rows[idx];
                        let sel = self.selected.contains(&idx);
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_h),
                            egui::Sense::click(),
                        );
                        if resp.clicked() {
                            clicked_at = Some(pos);
                        }
                        // A folder opens on the second click, as in Explorer and Xftp. A
                        // folder that is only a server name or a missing `D` record gets its
                        // empty control-file skeleton first.
                        if r.is_dir && resp.double_clicked() {
                            let path = self.dir.join(&r.name);
                            if path.is_dir() {
                                open_dir = Some(path);
                            } else {
                                open_missing_dir = Some(display_name(&r.name, true).to_string());
                            }
                        }
                        if resp.secondary_clicked() {
                            right_clicked_at = Some(pos);
                        }
                        let remote = r.origin == Origin::Remote;
                        resp.context_menu(|ui| {
                            let mut pick = |ui: &mut egui::Ui, label: &str, action: FileMenuAction| {
                                if ui.button(label).clicked() {
                                    menu_action = Some((action, idx));
                                    ui.close();
                                }
                            };
                            pick(ui, "Open", FileMenuAction::Open);
                            if !remote {
                                ui.separator();
                                pick(ui, "Update", FileMenuAction::Update);
                                pick(ui, "Commit…", FileMenuAction::Commit);
                                pick(ui, "Diff", FileMenuAction::Diff);
                                if !r.is_dir {
                                    pick(ui, "History", FileMenuAction::History);
                                }
                                pick(ui, "Tags…", FileMenuAction::Tags);
                                pick(ui, "Status", FileMenuAction::Status);
                            }
                            ui.separator();
                            pick(ui, "Copy name", FileMenuAction::CopyName);
                            pick(ui, "Copy full path", FileMenuAction::CopyPath);
                            if !remote {
                                ui.separator();
                                pick(ui, "Show in Explorer", FileMenuAction::ShowInExplorer);
                                pick(ui, "Move To Trash", FileMenuAction::MoveToTrash);
                            }
                        });
                        let bg = if sel {
                            ROW_SELECTED_BG
                        } else if resp.hovered() {
                            ROW_HOVER_BG
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let mut tip = if r.is_dir {
                            if remote {
                                format!(
                                    "{}\nDouble-click to create this empty folder (files are not checked out)",
                                    r.name
                                )
                            } else {
                                format!("{}\nDouble-click to open this folder", r.name)
                            }
                        } else {
                            r.name.clone()
                        };
                        if !r.tag.is_empty() {
                            let (kind, name) = sticky_kind(&r.tag);
                            tip.push_str(&format!("\nSticky {kind}: {name}"));
                            if !self.sticky.is_empty() && r.tag != self.sticky {
                                tip.push_str("\nThis file's tag differs from the folder's.");
                            }
                        }
                        resp.on_hover_text(tip);
                        let painter = ui.painter();
                        painter.rect_filled(rect, 0.0, bg);
                        let color = state_color(r.state);
                        let kind = FileKind::of(&r.name, r.is_dir);
                        let icon_color = if r.state == State::Missing {
                            icons::MISSING
                        } else {
                            icons::color(kind)
                        };
                        let g = ListGeometry::for_width(rect.width());
                        let left = rect.left();
                        icons::paint(painter, left + ICON_INSET, rect.center().y, kind, icon_color);
                        let name_x = left + ICON_INSET + icons::GAP;
                        let clip = egui::Rect::from_min_max(
                            egui::pos2(name_x, rect.top()),
                            egui::pos2(name_x + g.name_w, rect.bottom()),
                        );
                        painter.with_clip_rect(clip).text(
                            egui::pos2(name_x, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            truncate_columns(
                                display_name(&r.name, r.is_dir),
                                (g.name_w / cell) as usize,
                            ),
                            body.clone(),
                            color,
                        );
                        let put = |txt: &str, x: f32, w: f32, color: egui::Color32| {
                            painter.text(
                                egui::pos2(x, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                truncate_columns(txt, (w / cell) as usize),
                                body.clone(),
                                color,
                            );
                        };
                        put(r.state.label(), left + g.state_x, STATE_W, color);
                        // A row whose tag equals the folder's inherits it; only a file
                        // with its own tag (or an untagged folder) needs the glyph.
                        let show_sticky = !r.tag.is_empty()
                            && (self.sticky.is_empty() || r.tag != self.sticky);
                        let revision_w = if show_sticky {
                            REVISION_W - STICKY_ICON_W - STICKY_ICON_GAP
                        } else {
                            REVISION_W
                        };
                        let revision = truncate_columns(
                            if r.revision.is_empty() { "-" } else { &r.revision },
                            (revision_w / cell) as usize,
                        );
                        put(&revision, left + g.revision_x, revision_w, color);
                        if show_sticky {
                            // The glyph follows the number itself, not the column edge.
                            let revision_text_w = painter
                                .layout_no_wrap(revision, body.clone(), color)
                                .size()
                                .x;
                            painter.text(
                                egui::pos2(
                                    left + g.revision_x + revision_text_w + STICKY_ICON_GAP,
                                    rect.center().y,
                                ),
                                egui::Align2::LEFT_CENTER,
                                sticky_icon(&r.tag),
                                fonts::icon_font(STICKY_ICON_SIZE),
                                STICKY_COLOR,
                            );
                        }
                        put(
                            if r.options.is_empty() { "" } else { &r.options },
                            left + g.opts_x,
                            OPTS_W,
                            egui::Color32::from_gray(120),
                        );
                        put(
                            &pretty_time(&r.timestamp),
                            left + g.date_x,
                            DATE_W,
                            egui::Color32::from_gray(110),
                        );
                    }
                    if self.order.is_empty() {
                        ui.add_space(12.0);
                        let message = if self.loading.is_some() {
                            "Reading folder…".to_string()
                        } else if !self.search.trim().is_empty() {
                            format!("No name matches \"{}\".", self.search.trim())
                        } else {
                            "Nothing to show with these filters.".to_string()
                        };
                        ui.label(egui::RichText::new(message).italics().weak());
                    }
                })
        });
        let out = list.inner;
        let offset = out.state.clamped_offset().y;
        let index = (offset / row_h).floor() as usize;
        if let Some(&row_index) = self.order.get(index) {
            self.list_anchor = Some((
                self.rows[row_index].name.clone(),
                offset - index as f32 * row_h,
            ));
        } else {
            self.list_anchor = None;
        }
        self.list_interacting = ui.input(|i| i.pointer.any_down() || i.is_scrolling());

        // A right click selects the row under the pointer unless it is already part of
        // the selection, so a command can act on the whole selection it was opened on.
        if let Some(pos) = right_clicked_at {
            if let Some(&idx) = self.order.get(pos) {
                if !self.selected.contains(&idx) {
                    self.selected = HashSet::from([idx]);
                    self.anchor = Some(idx);
                }
            }
        }
        if let Some((action, index)) = menu_action {
            self.apply_file_menu(ui.ctx(), action, index);
        }
        if let Some(pos) = clicked_at {
            let (ctrl, shift) = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
            let (selection, anchor) =
                click_selection(&self.order, &self.selected, self.anchor, pos, ctrl, shift);
            self.selected = selection;
            self.anchor = anchor;
        }
        if let Some(name) = open_missing_dir {
            self.ensure_and_open_dir(name);
        }
        if let Some(dir) = open_dir {
            self.go_to(dir);
        }
    }

    /// Carries out a choice from the list's context menu. The menu only records what
    /// was picked while the rows are borrowed; this runs once that borrow is over.
    fn apply_file_menu(&mut self, ctx: &egui::Context, action: FileMenuAction, index: usize) {
        match action {
            FileMenuAction::Open => {
                let Some(row) = self.rows.get(index) else {
                    return;
                };
                let is_dir = row.is_dir;
                let name = row.name.clone();
                if is_dir {
                    let path = self.dir.join(&name);
                    if path.is_dir() {
                        self.go_to(path);
                    } else {
                        self.ensure_and_open_dir(display_name(&name, true).to_string());
                    }
                } else {
                    self.edit_selected();
                }
            }
            FileMenuAction::Update => self.cvs_command(Cmd::Update),
            FileMenuAction::Commit => {
                let template = self.prefs.log_template.clone();
                self.open_commit_dialog(template);
            }
            FileMenuAction::Diff => {
                self.open_diff_dialog();
            }
            FileMenuAction::History => {
                let Some(row) = self.rows.get(index) else {
                    return;
                };
                if row.is_dir {
                    self.status = "File History needs a file, not a folder".into();
                    return;
                }
                if row.origin == Origin::Remote {
                    self.status = "The selected file is not checked out; check it out first".into();
                    return;
                }
                let name = row.name.clone();
                let revision = row.revision.clone();
                let state = row.state;
                self.request_history(
                    self.dir.clone(),
                    self.root.clone(),
                    name,
                    revision,
                    state,
                    false,
                );
            }
            FileMenuAction::Tags => self.open_tags(),
            FileMenuAction::Status => self.cvs_command(Cmd::Status),
            FileMenuAction::CopyName => {
                ctx.copy_text(self.selected_visible_names().join("\n"));
            }
            FileMenuAction::CopyPath => {
                let paths: Vec<String> = self
                    .selected_visible_names()
                    .iter()
                    .map(|name| self.dir.join(name).display().to_string())
                    .collect();
                ctx.copy_text(paths.join("\n"));
            }
            FileMenuAction::ShowInExplorer => {
                let Some(row) = self.rows.get(index) else {
                    return;
                };
                if row.origin == Origin::Remote {
                    self.status = "The selected item is not checked out; check it out first".into();
                    return;
                }
                let name = row.name.clone();
                self.reveal_in_explorer(&name);
            }
            FileMenuAction::MoveToTrash => self.move_to_trash(),
        }
    }
}

/// Applies one click to the selection and returns the new selection plus the row a
/// later Shift-click extends from. `pos` is the clicked row's place in the display
/// order, not its index in `rows`, so the range follows what is actually on screen.
fn click_selection(
    order: &[usize],
    selected: &HashSet<usize>,
    anchor: Option<usize>,
    pos: usize,
    ctrl: bool,
    shift: bool,
) -> (HashSet<usize>, Option<usize>) {
    let Some(&clicked) = order.get(pos) else {
        return (selected.clone(), anchor);
    };
    if shift {
        let from = anchor.and_then(|a| order.iter().position(|&row| row == a));
        let mut new = if ctrl {
            selected.clone()
        } else {
            HashSet::new()
        };
        match from {
            Some(from) => {
                let (lo, hi) = if from <= pos {
                    (from, pos)
                } else {
                    (pos, from)
                };
                new.extend(order[lo..=hi].iter().copied());
                (new, anchor)
            }
            None => {
                new.insert(clicked);
                (new, Some(clicked))
            }
        }
    } else if ctrl {
        let mut new = selected.clone();
        if !new.remove(&clicked) {
            new.insert(clicked);
        }
        (new, Some(clicked))
    } else {
        (HashSet::from([clicked]), Some(clicked))
    }
}

/// The end of `path` that fits `max_width`, measured in the address bar's own font.
/// A hidden beginning is marked with `…`; when the cut lands inside a folder name, the
/// partial name is dropped so the bar starts at a real component boundary.
fn address_label(ui: &egui::Ui, path: &Path, max_width: f32, font: &egui::FontId) -> String {
    let full = path.display().to_string();
    let width_of = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
    };
    if width_of(&full) <= max_width {
        return full;
    }

    let chars: Vec<char> = full.chars().collect();
    let (mut low, mut high) = (1usize, chars.len());
    while low < high {
        let mid = (low + high).div_ceil(2);
        let candidate: String =
            format!("…{}", chars[chars.len() - mid..].iter().collect::<String>());
        if width_of(&candidate) <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }

    let mut tail: Vec<char> = chars[chars.len() - low..].to_vec();
    let sep = std::path::MAIN_SEPARATOR;
    let has_sep = if let Some(at) = tail.iter().position(|&c| c == sep || c == '/') {
        if at + 1 < tail.len() {
            tail.drain(..=at);
            true
        } else {
            false
        }
    } else {
        false
    };
    if tail.is_empty() {
        return "…".to_string();
    }
    if has_sep {
        format!("…{}{}", sep, tail.iter().collect::<String>())
    } else {
        format!("…{}", tail.iter().collect::<String>())
    }
}

/// CVS/Entries records the date in the 24-character form ("Sat Nov 22 09:14:03 2026").
/// The list shows it the way a person reads a date when it parses, and as recorded otherwise,
/// so a specially written field is never lost.
fn pretty_time(recorded: &str) -> String {
    entries::friendly_time(recorded)
}

/// The glyph for a sticky tag: a branch, a tag/revision, a date, or a pin for a value
/// this client did not write (`Entries` also accepts a bare tag name).
fn sticky_icon(tag: &str) -> &'static str {
    match tag.as_bytes().first() {
        Some(b'T') => egui_remixicon::icons::GIT_BRANCH_LINE,
        Some(b'N') => egui_remixicon::icons::PRICE_TAG_3_LINE,
        Some(b'D') => egui_remixicon::icons::CALENDAR_LINE,
        _ => egui_remixicon::icons::PUSHPIN_LINE,
    }
}

/// Split `CVS/Tag`'s marker from its name: `T` is a branch, `N` a tag or revision, `D` a
/// date. Anything else is shown as a tag, marker included, rather than lost.
fn sticky_kind(tag: &str) -> (&'static str, &str) {
    let (kind, rest) = match tag.as_bytes().first() {
        Some(b'T') => ("branch", &tag[1..]),
        Some(b'N') => ("tag", &tag[1..]),
        Some(b'D') => ("date", &tag[1..]),
        _ => ("tag", tag),
    };
    if rest.is_empty() {
        ("tag", tag)
    } else {
        (kind, rest)
    }
}

/// The folder-level sentence: what the pin means for the next update and commit.
fn sticky_banner(tag: &str) -> String {
    let (kind, name) = sticky_kind(tag);
    match kind {
        "branch" => format!("Sticky branch: {name} — updates follow it; commits go to it."),
        "date" => format!("Sticky date: {name} — updates stay on it; commits are refused."),
        _ => format!("Sticky tag: {name} — updates stay on it; commits are refused."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::app::tests::{app_with_rows, row};
    use crate::app::ui::test_run;
    use cvsdesk::entries::Row;

    #[test]
    fn a_plain_click_selects_one_row_and_sets_the_anchor() {
        let order = vec![3, 1, 2];
        let selected = HashSet::from([1]);
        let (sel, anchor) = click_selection(&order, &selected, Some(1), 2, false, false);
        assert_eq!(sel, HashSet::from([2]));
        assert_eq!(anchor, Some(2));
    }

    #[test]
    fn shift_click_selects_the_whole_range_from_the_anchor() {
        let order = vec![5, 2, 9, 4];
        let selected = HashSet::from([4]);
        let (sel, anchor) = click_selection(&order, &selected, Some(5), 2, false, true);
        assert_eq!(sel, HashSet::from([5, 2, 9]));
        assert_eq!(anchor, Some(5), "shift-click must keep its anchor");
    }

    #[test]
    fn shift_click_extends_upwards_too() {
        let order = vec![5, 2, 9, 4];
        let (sel, anchor) = click_selection(&order, &HashSet::new(), Some(4), 1, false, true);
        assert_eq!(sel, HashSet::from([2, 9, 4]));
        assert_eq!(anchor, Some(4));
    }

    #[test]
    fn shift_click_without_an_anchor_selects_only_the_clicked_row() {
        let order = vec![5, 2, 9];
        let selected = HashSet::from([5, 9]);
        let (sel, anchor) = click_selection(&order, &selected, None, 1, false, true);
        assert_eq!(sel, HashSet::from([2]));
        assert_eq!(anchor, Some(2));
    }

    #[test]
    fn ctrl_click_toggles_a_row_and_moves_the_anchor() {
        let order = vec![5, 2, 9];
        let selected = HashSet::from([5, 2]);
        let (sel, anchor) = click_selection(&order, &selected, Some(5), 1, true, false);
        assert_eq!(sel, HashSet::from([5]));
        assert_eq!(anchor, Some(2));
    }

    #[test]
    fn ctrl_shift_click_adds_the_range_to_the_selection() {
        let order = vec![5, 2, 9, 4];
        let selected = HashSet::from([4]);
        let (sel, anchor) = click_selection(&order, &selected, Some(5), 2, true, true);
        assert_eq!(sel, HashSet::from([5, 2, 9, 4]));
        assert_eq!(anchor, Some(5));
    }

    #[test]
    fn shift_click_with_a_filtered_out_anchor_selects_only_the_clicked_row() {
        let order = vec![5, 9];
        let selected = HashSet::from([5, 9]);
        let (sel, anchor) = click_selection(&order, &selected, Some(2), 0, false, true);
        assert_eq!(sel, HashSet::from([5]));
        assert_eq!(anchor, Some(5));
    }

    #[test]
    fn a_long_path_shows_the_current_folder_at_the_end() {
        let ctx = egui::Context::default();
        let mut shown = String::new();
        let _ = test_run(&ctx, Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let font = egui::TextStyle::Body.resolve(ui.style());
                shown = address_label(
                    ui,
                    Path::new(r"C:\a\very\long\chain\of\folders\current"),
                    100.0,
                    &font,
                );
            });
        });
        assert!(shown.starts_with('…'), "{shown}");
        assert!(shown.ends_with("current"), "{shown}");
    }

    #[test]
    fn typing_in_the_search_box_filters_the_list() {
        fn input() -> egui::RawInput {
            let mut input = egui::RawInput::default();
            input.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            ));
            input
        }
        let mut app = app_with_rows(vec![
            row("alpha.txt", false),
            row("beta.txt", false),
            row("alphabet.txt", false),
        ]);
        let ctx = egui::Context::default();
        fonts::install(&ctx);

        app.open_search();
        let _ = test_run(&ctx, input(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| app.ui_file_list(ui));
        });

        let mut typing = input();
        typing.events.push(egui::Event::Text("alpha".into()));
        let _ = test_run(&ctx, typing, |ui| {
            egui::CentralPanel::default().show(ui, |ui| app.ui_file_list(ui));
        });
        assert_eq!(app.search, "alpha", "typing reaches the search box");

        let _ = test_run(&ctx, input(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| app.ui_file_list(ui));
        });
        assert_eq!(
            app.order,
            vec![0, 2],
            "only the two alpha names stay listed"
        );
    }

    #[test]
    fn closing_the_search_clears_the_filter_and_restores_the_list() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(vec![row("alpha.txt", false), row("beta.txt", false)]);
        app.open_search();
        app.search = "alpha".into();
        app.order_dirty = true;
        app.ensure_order();
        assert_eq!(app.order, vec![0]);

        app.close_search(&ctx);

        assert!(!app.search_open);
        assert!(app.search.is_empty());
        app.ensure_order();
        assert_eq!(app.order, vec![0, 1], "closing the box drops the filter");
    }

    fn list_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        fonts::install(&ctx);
        ctx
    }

    fn list_input(events: Vec<egui::Event>) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 800.0),
        ));
        input.events = events;
        input
    }

    fn run_list(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) -> egui::FullOutput {
        test_run(ctx, list_input(events), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                app.ui_file_list(ui);
            });
        })
    }

    /// The text the frame put on the clipboard, if any.
    fn copied_text(frame: &egui::FullOutput) -> Option<&str> {
        frame
            .platform_output
            .commands
            .iter()
            .rev()
            .find_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            })
    }

    fn primary(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn secondary(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn collect_text_shapes(shape: &egui::Shape, out: &mut Vec<(String, egui::Pos2)>) {
        match shape {
            egui::Shape::Text(text) => out.push((text.galley.text().to_owned(), text.pos)),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect_text_shapes(shape, out);
                }
            }
            _ => {}
        }
    }

    fn output_texts(output: &egui::FullOutput) -> Vec<(String, egui::Pos2)> {
        let mut found = Vec::new();
        for clipped in &output.shapes {
            collect_text_shapes(&clipped.shape, &mut found);
        }
        found
    }

    fn text_pos(output: &egui::FullOutput, needle: &str) -> Option<egui::Pos2> {
        output_texts(output)
            .into_iter()
            .rev()
            .find(|(text, _)| text == needle)
            .map(|(_, pos)| pos)
    }

    /// The backgrounds painted under selected rows, top to bottom.
    fn selection_bands(output: &egui::FullOutput) -> Vec<egui::Rect> {
        fn collect(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Rect(rect) if rect.fill == ROW_SELECTED_BG => {
                    out.push(rect.rect);
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
        out.sort_by(|a, b| a.top().total_cmp(&b.top()));
        out
    }

    /// Font metrics differ slightly between machines; a sub-pixel difference is not a
    /// layout bug. Keep the tolerance under one logical pixel.
    const LAYOUT_TOL: f32 = 0.5;

    fn assert_size_close(actual: egui::Vec2, expected: egui::Vec2, what: &str) {
        assert!(
            (actual.x - expected.x).abs() < LAYOUT_TOL
                && (actual.y - expected.y).abs() < LAYOUT_TOL,
            "{what}: {actual:?} vs {expected:?}"
        );
    }

    /// A row with every column filled, so each heading can be matched to its data.
    fn detailed_row(name: &str, tag: &str) -> Row {
        let mut r = row(name, false);
        r.state = State::Unchanged;
        r.revision = "1.2".into();
        r.options = "-kb".into();
        r.tag = tag.into();
        r.timestamp = "Sat Nov 22 09:14:03 2026".into();
        r
    }

    fn click_heading(ctx: &egui::Context, app: &mut App, label: &str) -> egui::FullOutput {
        let first = run_list(ctx, app, Vec::new());
        let at = text_pos(&first, label).unwrap_or_else(|| panic!("{label:?} is not on screen"))
            + egui::vec2(4.0, 4.0);
        run_list(ctx, app, vec![egui::Event::PointerMoved(at)]);
        run_list(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), primary(at, true)],
        );
        run_list(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), primary(at, false)],
        )
    }

    fn arrow_positions(frame: &egui::FullOutput, glyph: &str) -> Vec<egui::Pos2> {
        output_texts(frame)
            .into_iter()
            .filter(|(text, _)| text.as_str() == glyph)
            .map(|(_, pos)| pos)
            .collect()
    }

    /// One frame of the list with a dialog laid over it, the way the File History window
    /// sits on the file list. `closed` records a click on the dialog's own button, so a
    /// test can tell a click that reached the dialog from one that never happened.
    fn run_list_with_dialog(
        ctx: &egui::Context,
        app: &mut App,
        events: Vec<egui::Event>,
        pos: egui::Pos2,
        closed: &mut bool,
    ) -> egui::FullOutput {
        test_run(ctx, list_input(events), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                app.ui_file_list(ui);
            });
            egui::Window::new("Graph")
                .id(egui::Id::unique("graph_over_the_list"))
                .fixed_pos(pos)
                .resizable(false)
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    if ui.button("Close").clicked() {
                        *closed = true;
                    }
                });
        })
    }

    #[test]
    fn each_heading_starts_where_its_column_starts() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![detailed_row("alpha.txt", "")]);
        let frame = run_list(&ctx, &mut app, Vec::new());
        let x = |text: &str| {
            text_pos(&frame, text)
                .unwrap_or_else(|| panic!("{text:?} is not on screen"))
                .x
        };

        assert_eq!(
            x("File"),
            x("alpha.txt") - icons::GAP,
            "the File heading sits over the icon"
        );
        assert_eq!(x("State"), x("Up-to-date"));
        assert_eq!(x("Revision"), x("1.2"));
        assert_eq!(x("Opts"), x("-kb"));
        assert_eq!(x("Recorded"), x(&pretty_time("Sat Nov 22 09:14:03 2026")));
        assert!(
            text_pos(&frame, "Sticky tag").is_none(),
            "the tag has no column of its own"
        );
    }

    #[test]
    fn a_selected_range_reads_as_one_continuous_band() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![
            row("alpha.txt", false),
            row("beta.txt", false),
            row("gamma.txt", false),
        ]);
        app.selected = HashSet::from([1, 2]);

        let frame = run_list(&ctx, &mut app, Vec::new());
        let bands = selection_bands(&frame);

        assert_eq!(bands.len(), 2, "one background per selected row");
        assert!(
            (bands[0].bottom() - bands[1].top()).abs() < 0.01,
            "adjacent selected rows must meet without a white stripe: {:?} then {:?}",
            bands[0],
            bands[1]
        );
    }

    #[test]
    fn clicking_a_heading_flips_the_sort_and_the_arrow() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![detailed_row("alpha.txt", "HEAD")]);

        let first = run_list(&ctx, &mut app, Vec::new());
        let up = arrow_positions(&first, egui_remixicon::icons::ARROW_UP_S_FILL);
        assert_eq!(up.len(), 1, "only the active column shows an arrow");
        assert!(up[0].x > text_pos(&first, "File").unwrap().x);

        click_heading(&ctx, &mut app, "State");
        assert_eq!(app.sort, SortKey::State);
        assert!(app.sort_asc);
        let frame = run_list(&ctx, &mut app, Vec::new());
        let up = arrow_positions(&frame, egui_remixicon::icons::ARROW_UP_S_FILL);
        assert_eq!(up.len(), 1);
        let state = text_pos(&frame, "State").unwrap().x;
        let revision = text_pos(&frame, "Revision").unwrap().x;
        assert!(
            up[0].x > state && up[0].x < revision,
            "the arrow follows State"
        );

        click_heading(&ctx, &mut app, "State");
        assert!(!app.sort_asc);
        let frame = run_list(&ctx, &mut app, Vec::new());
        let down = arrow_positions(&frame, egui_remixicon::icons::ARROW_DOWN_S_FILL);
        assert_eq!(down.len(), 1, "descending shows the down arrow");
        let state = text_pos(&frame, "State").unwrap().x;
        let revision = text_pos(&frame, "Revision").unwrap().x;
        assert!(down[0].x > state && down[0].x < revision);
    }

    #[test]
    fn sticky_markers_name_their_kind() {
        assert_eq!(sticky_kind("TBRANCH"), ("branch", "BRANCH"));
        assert_eq!(sticky_kind("N1.2"), ("tag", "1.2"));
        assert_eq!(sticky_kind("D2026.11.24"), ("date", "2026.11.24"));
        assert_eq!(sticky_kind("HEAD"), ("tag", "HEAD"));
        assert_eq!(
            sticky_kind("N"),
            ("tag", "N"),
            "a bare marker is not a name"
        );
    }

    #[test]
    fn a_sticky_row_shows_a_tag_icon_after_its_revision() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![detailed_row("alpha.txt", "TBRANCH")]);
        let frame = run_list(&ctx, &mut app, Vec::new());
        let revision = text_pos(&frame, "1.2").expect("the revision is on screen");
        let icons = arrow_positions(&frame, sticky_icon("TBRANCH"));

        assert_eq!(icons.len(), 1, "one glyph for the pinned row");
        assert!(icons[0].x > revision.x, "the glyph follows the revision");
    }

    #[test]
    fn a_row_matching_the_folder_tag_does_not_repeat_the_icon() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![detailed_row("alpha.txt", "TBRANCH")]);
        app.sticky = "TBRANCH".into();
        let frame = run_list(&ctx, &mut app, Vec::new());
        let header_y = text_pos(&frame, "Revision").unwrap().y;
        let row_icons: Vec<_> = arrow_positions(&frame, sticky_icon("TBRANCH"))
            .into_iter()
            .filter(|pos| pos.y > header_y)
            .collect();

        assert!(
            row_icons.is_empty(),
            "the folder line already names this tag"
        );
        assert!(
            text_pos(&frame, &sticky_banner("TBRANCH")).is_some(),
            "the folder tag is named once above the list"
        );
    }

    #[test]
    fn a_folder_tag_is_named_once_above_the_list() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![row("alpha.txt", false)]);
        app.sticky = "N1.2".into();
        let frame = run_list(&ctx, &mut app, Vec::new());
        let header_y = text_pos(&frame, "Revision").unwrap().y;
        let banner_icon = arrow_positions(&frame, sticky_icon("N1.2"))
            .into_iter()
            .find(|pos| pos.y < header_y);

        assert!(
            text_pos(&frame, &sticky_banner("N1.2")).is_some(),
            "the tag's name is on screen, not only in a tooltip"
        );
        assert!(banner_icon.is_some(), "the banner carries the type glyph");
    }

    #[test]
    fn clicking_opts_does_not_change_the_sort() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![detailed_row("alpha.txt", "HEAD")]);
        app.sort = SortKey::State;
        app.order_dirty = true;

        click_heading(&ctx, &mut app, "Opts");

        assert_eq!(app.sort, SortKey::State);
        assert!(app.sort_asc);
    }

    #[test]
    fn a_click_in_a_dialog_over_the_headings_does_not_sort_the_list() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![detailed_row("alpha.txt", "HEAD")]);
        let mut closed = false;

        // The dialog covers the heading band, as the File History window does. Its own
        // button is lined up with the "State" heading, so a click that leaks through
        // would sort the list by State.
        let first = run_list(&ctx, &mut app, Vec::new());
        let state = text_pos(&first, "State").expect("the heading is on screen");
        let revision = text_pos(&first, "Revision").expect("the next heading is on screen");
        let mut pos = state - egui::vec2(24.0, 8.0);
        // The window fades in over its first frames, so measure it until its button shows.
        let mut at = None;
        for _ in 0..16 {
            let frame = run_list_with_dialog(&ctx, &mut app, Vec::new(), pos, &mut closed);
            let Some(close) = text_pos(&frame, "Close") else {
                continue;
            };
            let delta = state - close;
            if delta.x.abs() < 1.0 && delta.y.abs() < 1.0 {
                at = Some(close + egui::vec2(5.0, 4.0));
                break;
            }
            pos += delta;
        }
        let at = at.expect("the dialog is on screen with its Close button");
        assert!(
            at.x >= state.x && at.x < revision.x,
            "the click must land on the State heading, not beside it: {at:?} vs {state:?}..{revision:?}"
        );
        assert!(
            at.y >= state.y && at.y < state.y + typography::LIST_HEADER_H,
            "the click must land on the heading band, not above or below it: {at:?} vs {state:?}"
        );

        run_list_with_dialog(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at)],
            pos,
            &mut closed,
        );
        run_list_with_dialog(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, true)],
            pos,
            &mut closed,
        );
        run_list_with_dialog(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, false)],
            pos,
            &mut closed,
        );

        assert!(closed, "the click must reach the dialog's own button");
        assert_eq!(
            app.sort,
            SortKey::Name,
            "a click in the dialog must not sort the list underneath it"
        );
        assert!(app.sort_asc, "the sort direction must not change");
    }

    /// Right-click a row by its name and return the frame that has the menu on screen.
    fn open_row_menu(ctx: &egui::Context, app: &mut App, name: &str) -> egui::FullOutput {
        let first = run_list(ctx, app, Vec::new());
        let at = text_pos(&first, name).expect("the row is on screen") + egui::vec2(6.0, 6.0);
        run_list(ctx, app, vec![egui::Event::PointerMoved(at)]);
        run_list(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), secondary(at, true)],
        );
        run_list(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), secondary(at, false)],
        );
        run_list(ctx, app, vec![egui::Event::PointerMoved(at)])
    }

    /// Click a menu row by its label. The returned frame is the release that runs the action.
    fn click_menu_row(
        ctx: &egui::Context,
        app: &mut App,
        menu: &egui::FullOutput,
        label: &str,
    ) -> egui::FullOutput {
        let at = text_pos(menu, label).expect("the menu row is on screen") + egui::vec2(6.0, 6.0);
        run_list(ctx, app, vec![egui::Event::PointerMoved(at)]);
        run_list(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), primary(at, true)],
        );
        run_list(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), primary(at, false)],
        )
    }

    #[test]
    fn right_clicking_a_row_opens_the_file_menu() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![row("alpha.txt", false), row("beta.txt", false)]);

        let menu = open_row_menu(&ctx, &mut app, "beta.txt");

        let labels: Vec<String> = output_texts(&menu)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        for expected in [
            "Open",
            "Update",
            "Commit…",
            "Diff",
            "History",
            "Status",
            "Copy name",
            "Copy full path",
            "Show in Explorer",
            "Move To Trash",
        ] {
            assert!(
                labels.iter().any(|label| label == expected),
                "the file menu did not show {expected:?}: {labels:?}"
            );
        }
        assert!(
            !labels.iter().any(|label| label == "Edit"),
            "the menu opens files, it does not call the action Edit: {labels:?}"
        );
    }

    #[test]
    fn right_clicking_an_unselected_row_selects_only_it() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![row("alpha.txt", false), row("beta.txt", false)]);
        app.selected = HashSet::from([0]);
        app.anchor = Some(0);

        open_row_menu(&ctx, &mut app, "beta.txt");

        assert_eq!(app.selected, HashSet::from([1]));
        assert_eq!(app.anchor, Some(1));
    }

    #[test]
    fn right_clicking_a_selected_row_keeps_the_whole_selection() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![row("alpha.txt", false), row("beta.txt", false)]);
        app.selected = HashSet::from([0, 1]);
        app.anchor = Some(0);

        open_row_menu(&ctx, &mut app, "beta.txt");

        assert_eq!(app.selected, HashSet::from([0, 1]));
        assert_eq!(app.anchor, Some(0), "the shift-click anchor must not move");
    }

    #[test]
    fn copy_name_copies_each_selected_name_on_its_own_line() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![row("alpha.txt", false), row("beta.txt", false)]);
        app.selected = HashSet::from([0, 1]);

        let menu = open_row_menu(&ctx, &mut app, "beta.txt");
        let clicked = click_menu_row(&ctx, &mut app, &menu, "Copy name");

        assert_eq!(copied_text(&clicked), Some("alpha.txt\nbeta.txt"));
    }

    #[test]
    fn copy_full_path_copies_the_folder_and_name() {
        let ctx = list_ctx();
        let mut app = app_with_rows(vec![row("alpha.txt", false), row("beta.txt", false)]);
        app.selected = HashSet::from([0, 1]);

        let menu = open_row_menu(&ctx, &mut app, "beta.txt");
        let clicked = click_menu_row(&ctx, &mut app, &menu, "Copy full path");

        assert_eq!(
            copied_text(&clicked),
            Some("C:\\work\\alpha.txt\nC:\\work\\beta.txt")
        );
    }

    #[test]
    fn the_open_row_enters_a_folder() {
        let root = std::env::temp_dir().join(format!("cvsdesk-menu-{}", std::process::id()));
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let ctx = list_ctx();
        let mut app = app_with_rows(vec![row("sub", true)]);
        app.dir = root.clone();
        app.address = root.display().to_string();
        app.history = crate::app::nav::History::new(root.clone());

        let menu = open_row_menu(&ctx, &mut app, "sub");
        click_menu_row(&ctx, &mut app, &menu, "Open");

        assert_eq!(app.dir, sub);
        assert_eq!(app.address, sub.display().to_string());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn address_bar_focus_is_in_the_accesskit_tree() {
        let root = std::env::temp_dir().join(format!("cvsdesk-a11y-addr-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let ctx = list_ctx();
        ctx.enable_accesskit();
        let mut app = app_with_rows(Vec::new());
        app.dir = root.clone();
        app.address = root.display().to_string();

        let first = run_list(&ctx, &mut app, Vec::new());
        let at = text_pos(&first, &root.display().to_string())
            .expect("the address label is on screen")
            + egui::vec2(6.0, 6.0);
        run_list(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
        run_list(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, true)],
        );
        let release = run_list(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, false)],
        );
        assert!(app.address_editing, "the click must open the address field");

        // The click frame itself already carries the new focus, so it must already carry
        // the address field's node too.
        check_accesskit_focus(&release);
        let next = run_list(&ctx, &mut app, Vec::new());
        check_accesskit_focus(&next);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn opening_the_address_field_does_not_move_the_list() {
        let ctx = list_ctx();
        typography::install(&ctx);
        let mut app = app_with_rows(vec![row("alpha.txt", false)]);

        let first = run_list(&ctx, &mut app, Vec::new());
        let heading_before = text_pos(&first, "File")
            .expect("the list heading is on screen")
            .y;
        let path_before = text_pos(&first, &app.address).expect("the path is on screen");

        let at = path_before + egui::vec2(6.0, 6.0);
        run_list(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
        run_list(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, true)],
        );
        run_list(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, false)],
        );
        assert!(app.address_editing, "the click must open the address field");

        // The field is drawn from the next frame on.
        let editing = run_list(&ctx, &mut app, Vec::new());
        let heading_after = text_pos(&editing, "File")
            .expect("the list heading is still on screen")
            .y;
        assert!(
            (heading_after - heading_before).abs() < LAYOUT_TOL,
            "opening the field moved the list: {heading_before} -> {heading_after}"
        );
        let path_after = text_pos(&editing, &app.address).expect("the path is in the field");
        assert!(
            (path_after.y - path_before.y).abs() < LAYOUT_TOL,
            "the path jumped when the field opened: {} -> {}",
            path_before.y,
            path_after.y
        );
    }

    #[test]
    fn opening_the_search_box_does_not_move_the_list() {
        let ctx = list_ctx();
        typography::install(&ctx);
        let mut app = app_with_rows(vec![row("alpha.txt", false)]);

        let first = run_list(&ctx, &mut app, Vec::new());
        let before = text_pos(&first, "File")
            .expect("the list heading is on screen")
            .y;

        app.open_search();
        let opened = run_list(&ctx, &mut app, Vec::new());
        let after = text_pos(&opened, "File")
            .expect("the list heading is still on screen")
            .y;
        assert!(
            (after - before).abs() < LAYOUT_TOL,
            "opening the search moved the list: {before} -> {after}"
        );

        app.close_search(&ctx);
        let closed = run_list(&ctx, &mut app, Vec::new());
        let back = text_pos(&closed, "File")
            .expect("the list heading is still on screen")
            .y;
        assert!(
            (back - before).abs() < LAYOUT_TOL,
            "closing the search moved the list: {before} -> {back}"
        );
    }

    #[test]
    fn the_filter_bar_is_folded_into_the_funnel_button_until_clicked() {
        let ctx = list_ctx();
        typography::install(&ctx);
        let mut app = app_with_rows(vec![row("alpha.txt", false)]);

        let closed = run_list(&ctx, &mut app, Vec::new());
        assert!(!app.filters_open, "the filter row starts folded");
        assert!(
            text_pos(&closed, "Local").is_none(),
            "a folded filter row must not take a line above the list"
        );

        let funnel = text_pos(&closed, FILTER).expect("the funnel button is in the nav bar");
        let at = funnel + egui::vec2(icons::SIZE / 2.0, icons::SIZE / 2.0);
        run_list(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, true)],
        );
        let opened = run_list(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, false)],
        );

        assert!(app.filters_open, "the click must unfold the filter row");
        assert!(
            text_pos(&opened, "Local").is_some(),
            "the unfolded row shows the scope choices"
        );
    }

    #[test]
    fn the_filter_button_sits_right_of_the_search_button() {
        let ctx = list_ctx();
        typography::install(&ctx);
        let mut app = app_with_rows(vec![row("alpha.txt", false)]);

        let output = run_list(&ctx, &mut app, Vec::new());
        let search = text_pos(&output, SEARCH).expect("the search button is in the nav bar");
        let filter = text_pos(&output, FILTER).expect("the funnel button is in the nav bar");
        assert!(
            filter.x > search.x,
            "the funnel ({filter:?}) must sit right of the search ({search:?})"
        );
    }

    #[test]
    fn hovering_and_focusing_the_path_keeps_the_fields_frame() {
        let ctx = list_ctx();
        typography::install(&ctx);
        ctx.all_styles_mut(|style| style.visuals = egui::Visuals::light());
        let mut app = app_with_rows(vec![row("alpha.txt", false)]);

        let idle = run_list(&ctx, &mut app, Vec::new());
        let idle_frame = address_frame(&idle);
        let at =
            text_pos(&idle, &app.address).expect("the path is on screen") + egui::vec2(6.0, 6.0);

        let hovered = run_list(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
        let hover_frame = address_frame(&hovered);
        assert_size_close(
            hover_frame.size(),
            idle_frame.size(),
            "hover must keep the frame's size the way a text field does",
        );

        run_list(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, true)],
        );
        run_list(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, false)],
        );
        let editing = run_list(&ctx, &mut app, Vec::new());
        let focus_frame = address_frame(&editing);
        assert_size_close(
            focus_frame.size(),
            hover_frame.size(),
            "clicking into the path must not resize its frame",
        );
        assert!(
            (focus_frame.center() - hover_frame.center()).length() < LAYOUT_TOL,
            "the frame must stay where the hovered one was"
        );
    }

    /// The largest white frame in one output. The address bar is the only white box
    /// the list draws, and its frame is the widest of them.
    fn address_frame(output: &egui::FullOutput) -> egui::Rect {
        fn collect(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Rect(rect) if rect.fill == egui::Color32::WHITE => {
                    out.push(rect.rect);
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
        *out.iter()
            .max_by(|a, b| a.width().total_cmp(&b.width()))
            .expect("the address bar's frame is painted")
    }

    fn check_accesskit_focus(frame: &egui::FullOutput) {
        let update = frame
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("accesskit is on, so the frame carries an update");
        let tree_root = update.tree.as_ref().expect("a tree").root;
        let by_id: std::collections::HashMap<_, _> =
            update.nodes.iter().map(|(id, node)| (*id, node)).collect();
        let mut reachable = HashSet::new();
        let mut stack = vec![tree_root];
        while let Some(id) = stack.pop() {
            if reachable.insert(id) {
                if let Some(node) = by_id.get(&id) {
                    stack.extend(node.children().iter().copied());
                }
            }
        }
        assert!(
            reachable.contains(&update.focus),
            "focus {:?} is not reachable from the root among this frame's {} nodes",
            update.focus,
            update.nodes.len()
        );
    }

    #[test]
    fn the_accesskit_tree_keeps_its_focused_node_after_the_file_menu() {
        let root = std::env::temp_dir().join(format!("cvsdesk-a11y-{}", std::process::id()));
        let junk = root.join("junk");
        std::fs::create_dir_all(&junk).unwrap();

        let ctx = list_ctx();
        ctx.enable_accesskit();
        let mut app = app_with_rows(vec![row("junk", true)]);
        app.dir = root.clone();
        app.address = root.display().to_string();
        app.history = crate::app::nav::History::new(root.clone());

        let menu = open_row_menu(&ctx, &mut app, "junk");
        let frame = click_menu_row(&ctx, &mut app, &menu, "Move To Trash");
        check_accesskit_focus(&frame);

        // The folder read that reload() started finishes on a later frame; the focused
        // node must still be in the tree when the new rows replace the old ones.
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.poll();
            let frame = run_list(&ctx, &mut app, Vec::new());
            check_accesskit_focus(&frame);
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_folder_with_many_files_paints_only_what_is_on_screen() {
        let rows: Vec<Row> = (0..20_000)
            .map(|i| row(&format!("file-{i}.txt"), false))
            .collect();
        let mut app = app_with_rows(rows);
        let ctx = egui::Context::default();
        fonts::install(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 800.0),
        ));
        let output = test_run(&ctx, input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                app.ui_file_list(ui);
            });
        });
        let texts = output
            .shapes
            .iter()
            .filter(|s| matches!(&s.shape, egui::Shape::Text(_)))
            .count();
        assert!(
            texts < 500,
            "painted {texts} text shapes for 20,000 rows; the list must paint only the visible rows"
        );
    }

    #[test]
    fn a_restored_row_keeps_its_place_when_names_arrive_above_it() {
        let old = vec![
            row("a.txt", false),
            row("b.txt", false),
            row("c.txt", false),
        ];
        assert_eq!(
            anchored_offset(&[0, 1, 2], &old, "b.txt", 3.0, 46.0),
            Some(49.0)
        );

        let new = vec![
            row("a.txt", false),
            row("aa.txt", false),
            row("b.txt", false),
            row("c.txt", false),
        ];
        assert_eq!(
            anchored_offset(&[0, 1, 2, 3], &new, "b.txt", 3.0, 46.0),
            Some(95.0)
        );
        assert_eq!(
            anchored_offset(&[0, 1, 2, 3], &new, "gone.txt", 3.0, 46.0),
            None
        );
    }
}
