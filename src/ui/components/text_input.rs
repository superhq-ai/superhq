use std::ops::Range;
use std::time::Duration;

use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, Bounds, ClipboardItem,
    Context, CursorStyle, DispatchPhase, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    Render, ScrollWheelEvent, SharedString, Style, Task, TextRun, UTF16Selection, UnderlineStyle,
    Window,
};
use unicode_segmentation::*;

use crate::ui::theme as t;
use super::actions::Cancel;
use super::context_menu::{ContextMenu, MenuEntry, MenuItem};

const MASK_CHAR: &str = "\u{2022}";
/// Pixels of breathing room kept between the 1.5px caret and the right edge.
const CARET_PAD_PX: f32 = 2.0;
const AUTO_SCROLL_TICK: Duration = Duration::from_millis(30);
/// Auto-scroll: pixels per overshoot-pixel per tick.
const AUTO_SCROLL_VELOCITY: f32 = 0.4;
/// Auto-scroll: max pixels per tick (clamps very-fast drags).
const AUTO_SCROLL_MAX_PX_PER_TICK: f32 = 30.0;

fn max_scroll(line_width: Pixels, view_width: Pixels) -> Pixels {
    (line_width - view_width + px(CARET_PAD_PX)).max(px(0.0))
}

// Actions
actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
    ]
);

/// Register key bindings for text input. Call once at app startup.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("TextInput")),
        KeyBinding::new("cmd-v", Paste, Some("TextInput")),
        KeyBinding::new("cmd-c", Copy, Some("TextInput")),
        KeyBinding::new("cmd-x", Cut, Some("TextInput")),
        KeyBinding::new("cmd-z", Undo, Some("TextInput")),
        KeyBinding::new("cmd-shift-z", Redo, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("TextInput")),
    ]);
}

// Events
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum TextInputEvent {
    Changed(SharedString),
    Submit(SharedString),
    Blur,
}

// Undo history
struct HistoryEntry {
    content: String,
    cursor: usize,
}

struct UndoHistory {
    entries: Vec<HistoryEntry>,
    index: usize,
}

impl UndoHistory {
    fn new() -> Self {
        Self {
            entries: vec![HistoryEntry { content: String::new(), cursor: 0 }],
            index: 0,
        }
    }

    fn push(&mut self, content: &str, cursor: usize) {
        // Skip if identical to current
        if let Some(current) = self.entries.get(self.index) {
            if current.content == content {
                return;
            }
        }
        // Truncate redo history
        self.entries.truncate(self.index + 1);
        self.entries.push(HistoryEntry {
            content: content.to_string(),
            cursor,
        });
        // Cap at 100 entries, evict oldest
        if self.entries.len() > 100 {
            self.entries.remove(0);
        }
        self.index = self.entries.len() - 1;
    }

    fn undo(&mut self) -> Option<&HistoryEntry> {
        if self.index > 0 {
            self.index -= 1;
            Some(&self.entries[self.index])
        } else {
            None
        }
    }

    fn redo(&mut self) -> Option<&HistoryEntry> {
        if self.index + 1 < self.entries.len() {
            self.index += 1;
            Some(&self.entries[self.index])
        } else {
            None
        }
    }
}

// State
pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<gpui::ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    history: UndoHistory,
    disabled: bool,
    masked: bool,
    scroll_offset: Pixels,
    last_seen_cursor: Option<usize>,
    last_mouse_position: Option<Point<Pixels>>,
    auto_scroll_task: Option<Task<()>>,
    context_menu: Option<Entity<ContextMenu>>,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle().tab_stop(true),
            content: "".into(),
            placeholder: "".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            history: UndoHistory::new(),
            disabled: false,
            masked: false,
            scroll_offset: px(0.0),
            last_seen_cursor: None,
            last_mouse_position: None,
            auto_scroll_task: None,
            context_menu: None,
        }
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>) {
        self.placeholder = text.into();
    }

    #[allow(dead_code)]
    pub fn set_value(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = text.into();
        self.selected_range = self.content.len()..self.content.len();
        self.history.push(&self.content, self.content.len());
        cx.notify();
    }

    pub fn value(&self) -> &str {
        &self.content
    }

    #[allow(dead_code)]
    pub fn set_disabled(&mut self, disabled: bool) {
        self.disabled = disabled;
    }

    pub fn set_masked(&mut self, masked: bool) {
        self.masked = masked;
    }

    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    #[allow(dead_code)]
    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled { return; }
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled { return; }
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled { return; }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace('\n', " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled { return; }
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled { return; }
        if let Some(entry) = self.history.undo() {
            self.content = entry.content.clone().into();
            self.selected_range = entry.cursor..entry.cursor;
            cx.emit(TextInputEvent::Changed(self.content.clone()));
            cx.notify();
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled { return; }
        if let Some(entry) = self.history.redo() {
            self.content = entry.content.clone().into();
            self.selected_range = entry.cursor..entry.cursor;
            cx.emit(TextInputEvent::Changed(self.content.clone()));
            cx.notify();
        }
    }

    fn show_character_palette(&mut self, _: &ShowCharacterPalette, window: &mut Window, _: &mut Context<Self>) {
        window.show_character_palette();
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.is_some() {
            self.context_menu = None;
            cx.notify();
        } else {
            // Nothing to dismiss — let it bubble to the parent (e.g., dialog)
            cx.propagate();
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        let index = self.index_for_mouse_position(event.position);
        match event.click_count {
            2 => {
                self.is_selecting = false;
                self.select_word_at(index, cx);
            }
            n if n >= 3 => {
                self.is_selecting = false;
                self.move_to(0, cx);
                self.select_to(self.content.len(), cx);
            }
            _ => {
                self.is_selecting = true;
                if event.modifiers.shift {
                    self.select_to(index, cx);
                } else {
                    self.move_to(index, cx);
                }
            }
        }
    }

    fn select_word_at(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.content.is_empty() { return; }
        let offset = offset.min(self.content.len());
        let mut start = 0usize;
        let mut end = self.content.len();
        for (idx, word) in self.content.split_word_bound_indices() {
            let word_end = idx + word.len();
            if offset >= idx && offset <= word_end {
                start = idx;
                end = word_end;
                break;
            }
        }
        self.selected_range = start..end;
        self.selection_reversed = false;
        cx.notify();
    }

    fn on_right_click(&mut self, event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let has_selection = !self.selected_range.is_empty();
        let input = cx.entity().clone();
        let input2 = input.clone();
        let input3 = input.clone();
        let input4 = input.clone();

        let entries = vec![
            MenuEntry::Item(
                MenuItem::new("Cut", move |window, cx| {
                    input.update(cx, |this, cx| this.cut(&Cut, window, cx));
                }).disabled(!has_selection || self.disabled)
            ),
            MenuEntry::Item(
                MenuItem::new("Copy", move |window, cx| {
                    input2.update(cx, |this, cx| this.copy(&Copy, window, cx));
                }).disabled(!has_selection)
            ),
            MenuEntry::Item(
                MenuItem::new("Paste", move |window, cx| {
                    input3.update(cx, |this, cx| this.paste(&Paste, window, cx));
                }).disabled(self.disabled)
            ),
            MenuEntry::Separator,
            MenuEntry::Item(
                MenuItem::new("Select All", move |window, cx| {
                    input4.update(cx, |this, cx| this.select_all(&SelectAll, window, cx));
                })
            ),
        ];

        let position = event.position;
        let menu = cx.new(|cx| ContextMenu::new(position, entries, cx));
        // Focus on next frame after deferred element is laid out
        let menu_focus = menu.read(cx).focus_handle.clone();
        cx.defer(move |cx| {
            if let Some(w) = cx.active_window() {
                w.update(cx, |_, window, _| {
                    menu_focus.focus(window);
                }).ok();
            }
        });
        cx.subscribe(&menu, |this, _, _event: &super::context_menu::ContextMenuEvent, cx| {
            this.context_menu = None;
            let fh = this.focus_handle.clone();
            cx.defer(move |cx| {
                if let Some(w) = cx.active_window() {
                    w.update(cx, |_, window, _| {
                        fh.focus(window);
                    }).ok();
                }
            });
            cx.notify();
        }).detach();
        self.context_menu = Some(menu);
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        self.last_mouse_position = None;
        self.auto_scroll_task = None;
    }

    fn on_scroll_wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(bounds), Some(line)) = (self.last_bounds, self.last_layout.as_ref())
        else { return; };
        let max = max_scroll(line.width, bounds.size.width);
        if max <= px(0.0) { return; }

        let delta = event.delta.pixel_delta(window.line_height());
        // Prefer horizontal trackpad delta; fall back to vertical (mouse wheel + shift).
        let dx = if f32::from(delta.x) != 0.0 { f32::from(delta.x) } else { f32::from(delta.y) };
        if dx == 0.0 { return; }

        let new_scroll = (self.scroll_offset - px(dx)).clamp(px(0.0), max);
        if new_scroll != self.scroll_offset {
            self.scroll_offset = new_scroll;
            cx.notify();
        }
    }

    fn is_mouse_outside_horizontally(&self, position: Point<Pixels>) -> bool {
        let Some(bounds) = self.last_bounds else { return false; };
        position.x < bounds.left() || position.x > bounds.right()
    }

    fn ensure_auto_scroll(&mut self, cx: &mut Context<Self>) {
        if self.auto_scroll_task.is_some() { return; }
        let this = cx.entity().downgrade();
        self.auto_scroll_task = Some(cx.spawn(async move |_, cx| {
            loop {
                gpui::Timer::after(AUTO_SCROLL_TICK).await;
                let keep_going = cx
                    .update(|cx| {
                        this.update(cx, |input, cx| {
                            if !input.is_selecting { return false; }
                            let Some(pos) = input.last_mouse_position else { return false; };
                            let Some(bounds) = input.last_bounds else { return false; };

                            let dx_outside = if pos.x < bounds.left() {
                                pos.x - bounds.left()
                            } else if pos.x > bounds.right() {
                                pos.x - bounds.right()
                            } else {
                                return false;
                            };

                            let step = (f32::from(dx_outside) * AUTO_SCROLL_VELOCITY)
                                .clamp(-AUTO_SCROLL_MAX_PX_PER_TICK, AUTO_SCROLL_MAX_PX_PER_TICK);
                            let prev_scroll = input.scroll_offset;
                            let prev_cursor = input.cursor_offset();
                            let unclamped = (prev_scroll + px(step)).max(px(0.0));
                            input.scroll_offset = match input.last_layout.as_ref() {
                                Some(line) => unclamped.min(max_scroll(line.width, bounds.size.width)),
                                None => unclamped,
                            };

                            let new_index = input.index_for_mouse_position(pos);
                            // Skip notify when neither selection nor scroll moved
                            // (e.g. mouse held past content-end with scroll already at max).
                            if new_index != prev_cursor || input.scroll_offset != prev_scroll {
                                input.select_to(new_index, cx);
                            }
                            true
                        })
                        .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if !keep_going { break; }
            }
            cx.update(|cx| {
                this.update(cx, |input, _| { input.auto_scroll_task = None; }).ok();
            }).ok();
        }));
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;

        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() { return 0; }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else { return 0; };
        if position.y < bounds.top() { return 0; }
        if position.y > bounds.bottom() { return self.content.len(); }
        let display_index = line.closest_index_for_x(position.x - bounds.left() + self.scroll_offset);
        if self.masked {
            // Map masked-character index back to byte offset in real content
            let bullet_len = MASK_CHAR.len();
            let char_index = display_index / bullet_len;
            self.content
                .char_indices()
                .nth(char_index)
                .map(|(i, _)| i)
                .unwrap_or(self.content.len())
        } else {
            display_index
        }
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }

        cx.notify()
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    // UTF-16 helpers for IME
    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset { break; }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset { break; }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

// IME integration
impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self, range_utf16: Range<usize>, actual_range: &mut Option<Range<usize>>,
        _: &mut Window, _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self, range_utf16: Option<Range<usize>>, new_text: &str,
        _: &mut Window, cx: &mut Context<Self>,
    ) {
        if self.disabled { return; }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        let len = self.content.len();
        let start = range.start.min(len);
        let end = range.end.min(len);
        self.content = (self.content[0..start].to_owned() + new_text + &self.content[end..]).into();
        self.selected_range = start + new_text.len()..start + new_text.len();
        self.marked_range.take();

        self.history.push(&self.content, self.selected_range.start);
        cx.emit(TextInputEvent::Changed(self.content.clone()));
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self, range_utf16: Option<Range<usize>>, new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>,
    ) {
        if self.disabled { return; }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        let len = self.content.len();
        let start = range.start.min(len);
        let end = range.end.min(len);
        self.content = (self.content[0..start].to_owned() + new_text + &self.content[end..]).into();
        self.marked_range = if !new_text.is_empty() {
            Some(range.start..range.start + new_text.len())
        } else {
            None
        };
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + range.start..r.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self, range_utf16: Range<usize>, bounds: Bounds<Pixels>,
        _: &mut Window, _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(bounds.left() + layout.x_for_index(range.start), bounds.top()),
            point(bounds.left() + layout.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self, pt: Point<Pixels>, _: &mut Window, _: &mut Context<Self>,
    ) -> Option<usize> {
        let line_pt = self.last_bounds?.localize(&pt)?;
        let layout = self.last_layout.as_ref()?;
        let utf8_index = layout.index_for_x(pt.x - line_pt.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// Custom Element for text rendering
struct TextInputElement {
    input: Entity<TextInput>,
}

struct TextInputPrepaint {
    line: Option<gpui::ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextInputElement {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl gpui::Element for TextInputElement {
    type RequestLayoutState = ();
    type PrepaintState = TextInputPrepaint;

    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(
        &mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>,
        window: &mut Window, cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App,
    ) -> TextInputPrepaint {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let content_str = content.to_string();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let masked = input.masked;

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), t::text_ghost().into())
        } else if masked {
            (SharedString::from(MASK_CHAR.repeat(content.chars().count())), t::text_secondary().into())
        } else {
            (content, t::text_secondary().into())
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun { len: marked_range.start, ..run.clone() },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false }),
                    ..run.clone()
                },
                TextRun { len: display_text.len() - marked_range.end, ..run },
            ].into_iter().filter(|r| r.len > 0).collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(display_text, font_size, &runs, None);

        // When masked, map cursor byte offset from real content to masked string
        let display_cursor = if masked && !content_str.is_empty() {
            let char_offset = content_str[..cursor].chars().count();
            char_offset * MASK_CHAR.len()
        } else {
            cursor
        };
        let cursor_pos = line.x_for_index(display_cursor);
        let focused = input.focus_handle.is_focused(window);

        // Scroll the cursor into view when it moves; otherwise preserve the
        // user's manual scroll (e.g. from the scroll wheel). Always clamp.
        let caret_pad = px(CARET_PAD_PX);
        let line_width = line.width;
        let view_width = bounds.size.width;
        let cursor_changed = input.last_seen_cursor != Some(cursor);
        let mut scroll = input.scroll_offset;
        if line_width + caret_pad <= view_width {
            scroll = px(0.0);
        } else {
            if cursor_changed {
                if cursor_pos - scroll < px(0.0) {
                    scroll = cursor_pos;
                } else if cursor_pos - scroll > view_width - caret_pad {
                    scroll = cursor_pos - (view_width - caret_pad);
                }
            }
            scroll = scroll.clamp(px(0.0), max_scroll(line_width, view_width));
        }
        if scroll != input.scroll_offset || cursor_changed {
            self.input.update(cx, |input, _| {
                input.scroll_offset = scroll;
                input.last_seen_cursor = Some(cursor);
            });
        }

        let (selection, cursor_quad) = if selected_range.is_empty() {
            let cursor_quad = if focused {
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos - scroll, bounds.top()),
                        size(px(1.5), bounds.bottom() - bounds.top()),
                    ),
                    t::accent(),
                ))
            } else {
                None
            };
            (None, cursor_quad)
        } else {
            {
                let (sel_start, sel_end) = if masked && !content_str.is_empty() {
                    let s = content_str[..selected_range.start].chars().count() * MASK_CHAR.len();
                    let e = content_str[..selected_range.end].chars().count() * MASK_CHAR.len();
                    (s, e)
                } else {
                    (selected_range.start, selected_range.end)
                };
                (
                    Some(fill(
                        Bounds::from_corners(
                            point(bounds.left() + line.x_for_index(sel_start) - scroll, bounds.top()),
                            point(bounds.left() + line.x_for_index(sel_end) - scroll, bounds.bottom()),
                        ),
                        t::selection_bg(),
                    )),
                    None,
                )
            }
        };

        TextInputPrepaint { line: Some(line), cursor: cursor_quad, selection }
    }

    fn paint(
        &mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>, _: &mut (), prepaint: &mut TextInputPrepaint,
        window: &mut Window, cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);

        // Window-level drag handler: fires even when the mouse leaves the
        // input's bounds, so drag-select keeps following the cursor anywhere
        // on the screen until the button is released.
        let drag_input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _win, cx| {
            if phase != DispatchPhase::Bubble { return; }
            if event.pressed_button != Some(MouseButton::Left) { return; }
            drag_input.update(cx, |input, cx| {
                if !input.is_selecting { return; }
                input.last_mouse_position = Some(event.position);
                let new_index = input.index_for_mouse_position(event.position);
                input.select_to(new_index, cx);
                if input.is_mouse_outside_horizontally(event.position) {
                    input.ensure_auto_scroll(cx);
                }
            });
        });

        let scroll = self.input.read(cx).scroll_offset;
        let line = prepaint.line.take().unwrap();

        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection);
            }
            let origin = point(bounds.origin.x - scroll, bounds.origin.y);
            let _ = line.paint(origin, window.line_height(), window, cx);
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        });

        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

// Render
impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);

        div()
            .id("text-input")
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(if self.disabled { CursorStyle::default() } else { CursorStyle::IBeam })
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_click))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            // Styling
            .px_2()
            .py(px(5.0))
            .rounded(px(6.0))
            .border_1()
            .text_xs()
            .font_family("Menlo")
            .text_color(t::text_secondary())
            .bg(t::bg_base())
            .border_color(if focused { t::border_focus() } else { t::border() })
            .when(!self.disabled, |el| {
                el.hover(|s| s.border_color(t::border_strong()))
            })
            .when(self.disabled, |el| {
                el.opacity(0.5).cursor(CursorStyle::default())
            })
            .child(TextInputElement { input: cx.entity() })
            .children(self.context_menu.as_ref().map(|menu| {
                let pos = menu.read(cx).position;
                let input_for_dismiss = cx.entity().clone();
                gpui::deferred(
                    gpui::anchored().child(
                        div()
                            .id("ctx-backdrop")
                            .size_full()
                            .occlude()
                            .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, _, cx| {
                                input_for_dismiss.update(cx, |this, cx| {
                                    this.context_menu = None;
                                    cx.notify();
                                });
                            })
                            .child(
                                gpui::anchored()
                                    .position(pos)
                                    .snap_to_window()
                                    .child(menu.clone()),
                            ),
                    ),
                )
            }))
    }
}
