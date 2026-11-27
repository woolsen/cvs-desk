//! The bottom output pane: console, file states, log, diff and the rest.

use egui::text::{CCursor, CCursorRange};

use crate::app::{App, OutputKind};

/// What the output pane's context menu asked for. The menu only records the choice;
/// the pane applies it after the text has been drawn, where it can borrow the window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputMenuAction {
    Copy,
    CopyAll,
    SelectAll,
    ClearConsole,
}

impl App {
    pub(super) fn ui_output(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.output == OutputKind::State && self.state_dirty && self.loading.is_none() {
            self.refresh_state_sheet();
        }
        let mut stop_clicked = false;
        egui::Panel::bottom("output")
            .resizable(true)
            .default_size(200.0)
            .min_size(80.0)
            .show(ui, |ui| {
                let text = ui
                    .horizontal(|ui| {
                        for k in [
                            OutputKind::Console,
                            OutputKind::State,
                            OutputKind::Log,
                            OutputKind::Diff,
                            OutputKind::Status,
                            OutputKind::Annotate,
                            OutputKind::History,
                            OutputKind::Editors,
                        ] {
                            let on = self.output == k;
                            if ui.selectable_label(on, k.title()).clicked() {
                                if k != self.output {
                                    // The selection belonged to the pane being left.
                                    self.output_selection = None;
                                }
                                self.output = k;
                                if k == OutputKind::State && self.state_dirty && self.loading.is_none() {
                                    self.refresh_state_sheet();
                                }
                            }
                        }
                        let text = self.output_text();
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button("Copy")
                                .on_hover_text(
                                    "Copies the selected text, or everything when none is selected.",
                                )
                                .clicked()
                            {
                                let copy = self
                                    .output_selection
                                    .clone()
                                    .unwrap_or_else(|| text.clone());
                                ctx.copy_text(copy);
                            }
                            // The tool that used to sit in the toolbar. It lives beside the
                            // output it stops, and only exists while there is something to stop.
                            if self.runner.running {
                                ui.separator();
                                if ui
                                    .button(
                                        egui::RichText::new("Stop")
                                            .color(egui::Color32::from_rgb(190, 120, 0)),
                                    )
                                    .on_hover_text("Stop the running cvs command")
                                    .clicked()
                                {
                                    stop_clicked = true;
                                }
                            }
                        });
                        text
                    })
                    .inner;
                ui.separator();
                let body = text;
                let selection_before = self.output_selection.clone();
                let is_console = self.output == OutputKind::Console;
                let mut menu_action: Option<OutputMenuAction> = None;
                let mut text_id: Option<egui::Id> = None;
                let (focused, selected, right_pressed, menu_open) = egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    // Choosing text stops the console following new output: otherwise a
                    // running command would scroll the selection out from under the pointer.
                    .stick_to_bottom(
                        is_console && self.output_selection.is_none(),
                    )
                    .show(ui, |ui| {
                        let text_edit_id = ui.make_persistent_id(self.output.title());
                        let range_before_click = egui::TextEdit::load_state(ui.ctx(), text_edit_id)
                            .and_then(|state| state.cursor.char_range());
                        // An immutable buffer makes the pane read-only while the pointer can
                        // still select it; `interactive(false)` would block selection too.
                        let mut view = body.as_str();
                        let shown = egui::TextEdit::multiline(&mut view)
                            .id_salt(self.output.title())
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(8)
                            .show(ui);
                        text_id = Some(shown.response.id);
                        // egui's TextEdit folds the selection to the pointer on any press,
                        // right clicks included. The range is put back below, once the menu
                        // has had its chance to handle this frame's press.
                        let right_pressed = shown.response.hovered()
                            && ui.input(|i| {
                                i.pointer.button_pressed(egui::PointerButton::Secondary)
                            });
                        let has_text = !body.is_empty();
                        shown.response.context_menu(|ui| {
                            if ui
                                .add_enabled(has_text, egui::Button::new("Copy"))
                                .on_hover_text(
                                    "Copies the selected text, or everything when none is selected.",
                                )
                                .clicked()
                            {
                                menu_action = Some(OutputMenuAction::Copy);
                                ui.close();
                            }
                            if ui
                                .add_enabled(has_text, egui::Button::new("Copy all"))
                                .clicked()
                            {
                                menu_action = Some(OutputMenuAction::CopyAll);
                                ui.close();
                            }
                            if ui
                                .add_enabled(has_text, egui::Button::new("Select all"))
                                .clicked()
                            {
                                menu_action = Some(OutputMenuAction::SelectAll);
                                ui.close();
                            }
                            if is_console {
                                ui.separator();
                                if ui
                                    .add_enabled(
                                        has_text,
                                        egui::Button::new("Clear console"),
                                    )
                                    .clicked()
                                {
                                    menu_action = Some(OutputMenuAction::ClearConsole);
                                    ui.close();
                                }
                            }
                        });
                        let menu_open = shown.response.context_menu_opened();
                        // A press on a menu row lands outside the text edit, which makes egui
                        // surrender its focus; the selection is only painted while the edit is
                        // focused. Keep both the range and the focus while the menu works.
                        let keep_selection =
                            right_pressed || menu_open || menu_action.is_some();
                        if keep_selection {
                            if let Some(range) = range_before_click {
                                let mut state = shown.state.clone();
                                state.cursor.set_char_range(Some(range));
                                egui::TextEdit::store_state(ui.ctx(), shown.response.id, state);
                            }
                            ui.memory_mut(|m| m.request_focus(shown.response.id));
                        }
                        let selected = shown
                            .cursor_range
                            .filter(|range| !range.is_empty())
                            .map(|range| range.slice_str(body.as_str()).to_owned());
                        (
                            shown.response.has_focus(),
                            selected,
                            right_pressed,
                            menu_open || menu_action.is_some(),
                        )
                    })
                    .inner;
                let pressed_inside = ctx.input(|i| {
                    i.pointer.any_pressed()
                        && i.pointer
                            .interact_pos()
                            .is_some_and(|pos| ui.max_rect().contains(pos))
                });
                self.output_selection = remembered_selection(
                    focused,
                    selected,
                    pressed_inside,
                    right_pressed,
                    menu_open,
                    self.output_selection.take(),
                );
                match menu_action {
                    Some(OutputMenuAction::Copy) => {
                        ctx.copy_text(selection_before.unwrap_or_else(|| body.clone()));
                    }
                    Some(OutputMenuAction::CopyAll) => ctx.copy_text(body.clone()),
                    Some(OutputMenuAction::SelectAll) => {
                        if let Some(id) = text_id {
                            let mut state =
                                egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
                            state.cursor.set_char_range(Some(CCursorRange::two(
                                CCursor::new(0),
                                CCursor::new(body.chars().count()),
                            )));
                            egui::TextEdit::store_state(&ctx, id, state);
                            ctx.memory_mut(|m| m.request_focus(id));
                            self.output_selection = None;
                        }
                    }
                    Some(OutputMenuAction::ClearConsole) => {
                        self.runner.lines.clear();
                        self.output_selection = None;
                    }
                    None => {}
                }
            });
        if stop_clicked {
            self.stop();
        }
    }

    pub(super) fn output_text(&self) -> String {
        match self.output {
            OutputKind::Console => self.runner.lines.join("\n"),
            OutputKind::State => {
                if self.loading.is_some() {
                    "Reading folder…".to_string()
                } else {
                    self.state_sheet.clone()
                }
            }
            _ => {
                if self.runner.running && self.running_kind == Some(self.output) {
                    self.pending.join("\n")
                } else if let Some(lines) = self.pane_output.get(&self.output) {
                    lines.join("\n")
                } else {
                    match self.output {
                        OutputKind::Editors => {
                            "Nothing listed yet. Choose Query > Editors or Query > Watchers to ask\ncvs who is editing or watching the selected files.".to_string()
                        }
                        _ => format!(
                            "No output yet. Select files and choose Query > {} to see them here.",
                            self.output.title()
                        ),
                    }
                }
            }
        }
    }
}

/// What the output pane remembers as its copyable selection after a frame. While the text
/// itself has focus, its own answer wins, an empty one included. A press on the pane's own
/// buttons keeps the old selection: the press takes focus away from the text, and Copy only
/// fires on the release that follows.
fn remembered_selection(
    focused: bool,
    selected: Option<String>,
    pressed_inside: bool,
    right_pressed: bool,
    menu_open: bool,
    previous: Option<String>,
) -> Option<String> {
    if (right_pressed || menu_open) && previous.is_some() {
        previous
    } else if focused {
        selected
    } else if pressed_inside {
        previous
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::remembered_selection;
    use crate::app::tests::app_with_rows;
    use crate::app::ui::test_run;
    use crate::app::App;
    use egui::text::CCursor;

    fn some(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    fn pane_input(events: Vec<egui::Event>) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 800.0),
        ));
        input.events = events;
        input
    }

    fn run_pane(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) -> egui::FullOutput {
        test_run(ctx, pane_input(events), |ui| {
            app.ui_output(ui);
            egui::CentralPanel::default().show(ui, |_| {});
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

    fn secondary(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn primary(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
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

    fn has_text(output: &egui::FullOutput, needle: &str) -> bool {
        output_texts(output)
            .into_iter()
            .any(|(text, _)| text == needle)
    }

    fn find_text_shape(
        shape: &egui::Shape,
        needle: &str,
    ) -> Option<(egui::Pos2, std::sync::Arc<egui::Galley>)> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == needle => {
                Some((text.pos, text.galley.clone()))
            }
            egui::Shape::Vec(shapes) => shapes
                .iter()
                .find_map(|shape| find_text_shape(shape, needle)),
            _ => None,
        }
    }

    fn text_shape(
        output: &egui::FullOutput,
        needle: &str,
    ) -> Option<(egui::Pos2, std::sync::Arc<egui::Galley>)> {
        output
            .shapes
            .iter()
            .find_map(|clipped| find_text_shape(&clipped.shape, needle))
    }

    fn text_pos(output: &egui::FullOutput, needle: &str) -> Option<egui::Pos2> {
        output_texts(output)
            .into_iter()
            .rev()
            .find(|(text, _)| text == needle)
            .map(|(_, pos)| pos)
    }

    /// Right-click the pane's text and return the frame that has the menu on screen.
    fn open_menu(ctx: &egui::Context, app: &mut App, text: &str) -> egui::FullOutput {
        let first = run_pane(ctx, app, Vec::new());
        let at = text_pos(&first, text).expect("the pane text is on screen") + egui::vec2(8.0, 6.0);
        run_pane(ctx, app, vec![egui::Event::PointerMoved(at)]);
        run_pane(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), secondary(at, true)],
        );
        run_pane(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), secondary(at, false)],
        );
        run_pane(ctx, app, vec![egui::Event::PointerMoved(at)])
    }

    /// Click a menu row by its label. The returned frame is the release that runs the action.
    fn click_menu_row(
        ctx: &egui::Context,
        app: &mut App,
        menu: &egui::FullOutput,
        label: &str,
    ) -> egui::FullOutput {
        let at = text_pos(menu, label).expect("the menu row is on screen") + egui::vec2(6.0, 6.0);
        run_pane(ctx, app, vec![egui::Event::PointerMoved(at)]);
        run_pane(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), primary(at, true)],
        );
        run_pane(
            ctx,
            app,
            vec![egui::Event::PointerMoved(at), primary(at, false)],
        )
    }

    #[test]
    fn a_focused_pane_keeps_the_selection_it_reports() {
        assert_eq!(
            remembered_selection(true, some("one line"), false, false, false, some("older")),
            some("one line")
        );
    }

    #[test]
    fn a_focused_pane_with_nothing_selected_forgets_the_old_selection() {
        assert_eq!(
            remembered_selection(true, None, false, false, false, some("older")),
            None
        );
    }

    #[test]
    fn pressing_the_panes_buttons_keeps_the_selection_for_copy() {
        assert_eq!(
            remembered_selection(false, None, true, false, false, some("chosen")),
            some("chosen")
        );
    }

    #[test]
    fn pressing_outside_the_pane_drops_the_selection() {
        assert_eq!(
            remembered_selection(false, None, false, false, false, some("chosen")),
            None
        );
    }

    #[test]
    fn a_right_click_inside_the_pane_keeps_the_selection() {
        assert_eq!(
            remembered_selection(true, None, true, true, false, some("chosen")),
            some("chosen")
        );
    }

    #[test]
    fn an_open_context_menu_keeps_the_selection() {
        assert_eq!(
            remembered_selection(false, None, false, false, true, some("chosen")),
            some("chosen")
        );
    }

    #[test]
    fn right_clicking_the_output_text_opens_the_copy_menu() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(Vec::new());
        app.runner.lines.push("first line".into());
        app.runner.lines.push("second line".into());

        let settled = open_menu(&ctx, &mut app, "first line\nsecond line");

        let labels: Vec<String> = output_texts(&settled)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        for expected in ["Copy", "Copy all", "Select all", "Clear console"] {
            assert!(
                labels.iter().any(|label| label == expected),
                "the context menu did not show {expected:?}: {labels:?}"
            );
        }
    }

    #[test]
    fn copy_all_copies_every_line() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(Vec::new());
        app.runner.lines.push("first line".into());
        app.runner.lines.push("second line".into());

        let menu = open_menu(&ctx, &mut app, "first line\nsecond line");
        let clicked = click_menu_row(&ctx, &mut app, &menu, "Copy all");

        assert_eq!(copied_text(&clicked), Some("first line\nsecond line"));
    }

    #[test]
    fn select_all_then_copy_keeps_the_selection() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(Vec::new());
        app.runner.lines.push("first line".into());
        app.runner.lines.push("second line".into());
        let all = "first line\nsecond line";

        let menu = open_menu(&ctx, &mut app, all);
        click_menu_row(&ctx, &mut app, &menu, "Select all");
        run_pane(&ctx, &mut app, Vec::new());
        assert_eq!(app.output_selection.as_deref(), Some(all));

        let menu = open_menu(&ctx, &mut app, all);
        let clicked = click_menu_row(&ctx, &mut app, &menu, "Copy");
        assert_eq!(copied_text(&clicked), Some(all));
    }

    #[test]
    fn copy_copies_only_the_selected_text() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(Vec::new());
        app.runner.lines.push("first line".into());
        app.runner.lines.push("second line".into());
        let all = "first line\nsecond line";

        let first = run_pane(&ctx, &mut app, Vec::new());
        let (origin, galley) = text_shape(&first, all).expect("the console text is on screen");
        let at = origin + galley.pos_from_cursor(CCursor::new(0)).center().to_vec2();

        run_pane(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
        for _ in 0..2 {
            run_pane(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(at), primary(at, true)],
            );
            run_pane(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(at), primary(at, false)],
            );
        }
        run_pane(&ctx, &mut app, Vec::new());
        assert_eq!(app.output_selection.as_deref(), Some("first"));

        let menu = open_menu(&ctx, &mut app, all);
        let clicked = click_menu_row(&ctx, &mut app, &menu, "Copy");
        assert_eq!(copied_text(&clicked), Some("first"));
    }

    #[test]
    fn another_panes_reply_does_not_fill_this_pane() {
        let mut app = app_with_rows(Vec::new());
        app.pane_output.insert(
            crate::app::OutputKind::Status,
            vec!["the status reply".into()],
        );
        app.output = crate::app::OutputKind::Status;
        assert_eq!(app.output_text(), "the status reply");

        app.output = crate::app::OutputKind::Log;
        assert!(
            app.output_text().starts_with("No output yet"),
            "the Log pane showed the Status reply: {:?}",
            app.output_text()
        );
    }

    #[test]
    fn a_finished_command_fills_only_its_pane() {
        let mut app = app_with_rows(Vec::new());
        let (tx, rx) = std::sync::mpsc::channel();
        app.rx = Some(rx);
        app.running_kind = Some(crate::app::OutputKind::Status);
        app.pending = vec!["the status reply".into()];
        tx.send(cvsdesk::cvs::Msg::Done(0)).unwrap();
        app.poll();

        app.output = crate::app::OutputKind::Status;
        assert_eq!(app.output_text(), "the status reply");
        app.output = crate::app::OutputKind::Log;
        assert!(
            app.output_text().starts_with("No output yet"),
            "the Log pane showed the Status reply: {:?}",
            app.output_text()
        );
    }

    #[test]
    fn a_background_reply_leaves_the_pane_alone() {
        let mut app = app_with_rows(Vec::new());
        app.pane_output
            .insert(crate::app::OutputKind::Log, vec!["the old log".into()]);
        app.output = crate::app::OutputKind::Log;
        let (tx, rx) = std::sync::mpsc::channel();
        app.rx = Some(rx);
        app.tag_waiting = Some(crate::app::TagWait::Query);
        app.pending = vec!["the tag list".into()];
        tx.send(cvsdesk::cvs::Msg::Done(0)).unwrap();
        app.poll();

        assert_eq!(app.output_text(), "the old log");
        assert_eq!(app.captured, vec!["the tag list"]);
    }

    #[test]
    fn the_clear_row_belongs_to_the_console_only() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(Vec::new());
        app.output = crate::app::OutputKind::Log;
        app.pane_output.insert(
            crate::app::OutputKind::Log,
            vec!["first line".into(), "second line".into()],
        );

        let settled = open_menu(&ctx, &mut app, "first line\nsecond line");

        let labels: Vec<String> = output_texts(&settled)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        assert!(
            !labels.iter().any(|label| label == "Clear console"),
            "the log pane has no console to clear: {labels:?}"
        );
        assert!(
            labels.iter().any(|label| label == "Copy all"),
            "the log text can still be copied: {labels:?}"
        );
    }

    #[test]
    fn the_selection_keeps_focus_while_the_menu_is_open() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(Vec::new());
        app.runner.lines.push("first line".into());
        app.runner.lines.push("second line".into());
        let all = "first line\nsecond line";

        let first = run_pane(&ctx, &mut app, Vec::new());
        let (origin, galley) = text_shape(&first, all).expect("the console text is on screen");
        let at = origin + galley.pos_from_cursor(CCursor::new(0)).center().to_vec2();

        run_pane(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
        for _ in 0..2 {
            run_pane(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(at), primary(at, true)],
            );
            run_pane(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(at), primary(at, false)],
            );
        }
        run_pane(&ctx, &mut app, Vec::new());
        assert_eq!(app.output_selection.as_deref(), Some("first"));
        let focused = ctx
            .memory(|m| m.focused())
            .expect("the text edit owns the keyboard");

        let menu = open_menu(&ctx, &mut app, all);
        assert_eq!(
            ctx.memory(|m| m.focused()),
            Some(focused),
            "opening the menu must not take focus from the text"
        );

        click_menu_row(&ctx, &mut app, &menu, "Copy");
        run_pane(&ctx, &mut app, Vec::new());
        assert_eq!(
            ctx.memory(|m| m.focused()),
            Some(focused),
            "choosing Copy must leave the selection focused"
        );
    }

    #[test]
    fn clear_console_empties_the_console() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(Vec::new());
        app.runner.lines.push("first line".into());
        app.runner.lines.push("second line".into());

        let menu = open_menu(&ctx, &mut app, "first line\nsecond line");
        click_menu_row(&ctx, &mut app, &menu, "Clear console");

        assert!(app.runner.lines.is_empty());
        assert!(app.output_selection.is_none());
    }

    #[test]
    fn the_stop_button_appears_only_while_cvs_runs() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(Vec::new());

        let idle = run_pane(&ctx, &mut app, Vec::new());
        assert!(
            !has_text(&idle, "Stop"),
            "an idle pane must not offer a command that can do nothing"
        );

        app.runner.running = true;
        let busy = run_pane(&ctx, &mut app, Vec::new());
        assert!(
            has_text(&busy, "Stop"),
            "a running command must be stoppable"
        );
    }

    #[test]
    fn clicking_stop_asks_the_running_command_to_end() {
        let ctx = egui::Context::default();
        let mut app = app_with_rows(Vec::new());
        app.runner.running = true;
        let busy = run_pane(&ctx, &mut app, Vec::new());
        let at =
            text_pos(&busy, "Stop").expect("the stop button is on screen") + egui::vec2(6.0, 6.0);

        run_pane(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
        run_pane(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, true)],
        );
        run_pane(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(at), primary(at, false)],
        );

        // The client runs in this process now, so Stop only asks it to return; the runner
        // stays busy until the worker reports Done.
        assert!(app.stop_requested, "Stop must ask the command to end");
        assert_eq!(app.status, "CVS stopped");
        app.runner.finish(1);
        assert!(!app.runner.running, "Done makes the runner idle again");
    }
}
