// SPDX-License-Identifier: Apache-2.0
// Adapted from gpui 0.2.2 examples/input.rs, Copyright 2022 - 2025 Zed Industries, Inc.
// This adaptation adds bounded editing, password masking, scrolling, and parent events.
// The upstream Apache-2.0 license is reproduced at the end of this file.

//! Single-line input. Call [`init`] once, create an entity with [`TextInput::new`],
//! and subscribe to [`InputEvent`] on that entity. The parent handles focus traversal.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine, SharedString, Style, TextRun,
    UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, relative,
    rgb, rgba, size,
};

use editor::{Editor, byte_to_utf16, range_from_utf16, range_to_utf16};

actions!(
    gosiptea_input,
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
        SelectHome,
        SelectEnd,
        Copy,
        Cut,
        Paste,
        Submit,
        Escape,
        Next,
        Previous,
    ]
);

/// Register once when the application starts. Bindings only apply inside TextInput.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("shift-home", SelectHome, Some("TextInput")),
        KeyBinding::new("shift-end", SelectEnd, Some("TextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("TextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("TextInput")),
        KeyBinding::new("ctrl-x", Cut, Some("TextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("TextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("TextInput")),
        KeyBinding::new("cmd-c", Copy, Some("TextInput")),
        KeyBinding::new("cmd-x", Cut, Some("TextInput")),
        KeyBinding::new("cmd-v", Paste, Some("TextInput")),
        KeyBinding::new("enter", Submit, Some("TextInput")),
        KeyBinding::new("escape", Escape, Some("TextInput")),
        KeyBinding::new("tab", Next, Some("TextInput")),
        KeyBinding::new("shift-tab", Previous, Some("TextInput")),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// The stored value changed, including programmatic changes and IME preedit.
    Changed,
    Submit,
    Escape,
    Next,
    Previous,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    editor: Editor,
    placeholder: SharedString,
    secret: bool,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_x: Pixels,
    is_selecting: bool,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    /// `max_chars` counts Unicode scalar values, not bytes or graphemes.
    /// All writes remove control characters and Unicode line/paragraph separators.
    pub fn new(
        placeholder: impl Into<SharedString>,
        max_chars: usize,
        secret: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = placeholder.into();
        Self {
            focus_handle: cx.focus_handle(),
            editor: Editor::new(max_chars),
            placeholder: editor::sanitize(&placeholder, usize::MAX).into(),
            secret,
            last_layout: None,
            last_bounds: None,
            scroll_x: px(0.),
            is_selecting: false,
        }
    }

    pub fn value(&self) -> &str {
        &self.editor.text
    }

    /// Applies the same limits as typing, clears composition, and puts the caret at the end.
    /// Emits Changed only if the stored value differs.
    pub fn set_value(&mut self, value: impl Into<SharedString>, cx: &mut Context<Self>) {
        let value = value.into();
        let changed = self.editor.set_value(&value);
        self.is_selecting = false;
        self.did_edit(changed, cx);
    }

    /// Synchronize a snapshot without turning the account form into a user edit.
    pub fn set_value_silent(&mut self, value: impl Into<SharedString>, cx: &mut Context<Self>) {
        let value = value.into();
        if self.editor.text != value.as_ref() {
            self.editor.set_value(&value);
            self.is_selecting = false;
            cx.notify();
        }
    }

    fn did_edit(&mut self, changed: bool, cx: &mut Context<Self>) {
        if changed {
            // Geometry from the previous frame no longer describes the current value.
            self.last_layout = None;
            cx.emit(InputEvent::Changed);
        }
        cx.notify();
    }

    fn move_to(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        self.editor.move_to(offset, extend);
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.horizontal(false, false);
        cx.notify();
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.horizontal(true, false);
        cx.notify();
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.horizontal(false, true);
        cx.notify();
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.horizontal(true, true);
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_to(0, false);
        self.move_to(self.editor.text.len(), true, cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, false, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.editor.text.len(), false, cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, true, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.editor.text.len(), true, cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let changed = self.editor.delete(false);
        self.did_edit(changed, cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        let changed = self.editor.delete(true);
        self.did_edit(changed, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.editor.clipboard_text(self.secret) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.editor.clipboard_text(self.secret) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        // Secret cuts delete the selection without touching either clipboard.
        if !self.editor.selection().is_empty() {
            self.editor.marked = None;
            let changed = self.editor.replace(None, "", None, false);
            self.did_edit(changed, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn index_for_position(&self, position: Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        if self.editor.text.is_empty() {
            return Some(0);
        }
        let display_index = line.closest_index_for_x(position.x - bounds.left() + self.scroll_x);
        let byte = self.editor.display_to_byte(display_index, self.secret);
        Some(editor::nearest_grapheme(&self.editor.text, byte))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        if let Some(index) = self.index_for_position(event.position) {
            self.move_to(index, event.modifiers.shift, cx);
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = range_from_utf16(&self.editor.text, range_utf16);
        *actual_range = Some(range_to_utf16(&self.editor.text, range.clone()));
        Some(self.editor.ime_text(range, self.secret))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: range_to_utf16(&self.editor.text, self.editor.selection()),
            reversed: self.editor.head < self.editor.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.editor
            .marked
            .clone()
            .map(|range| range_to_utf16(&self.editor.text, range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.editor.replace(range_utf16, text, None, false);
        self.did_edit(changed, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.editor.replace(range_utf16, text, selected_utf16, true);
        self.did_edit(changed, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let range = range_from_utf16(&self.editor.text, range_utf16);
        let x = |byte| {
            (bounds.left() + line.x_for_index(self.editor.byte_to_display(byte, self.secret))
                - self.scroll_x)
                .max(bounds.left())
                .min(bounds.right())
        };
        let start = x(range.start);
        let end = x(range.end);
        Some(Bounds::from_corners(
            point(start.min(end), bounds.top()),
            point(start.max(end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.last_bounds?.localize(&position)?;
        self.index_for_position(position)
            .map(|index| byte_to_utf16(&self.editor.text, index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: ShapedLine,
    origin: Point<Pixels>,
    cursor: PaintQuad,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        self.input.update(cx, |input, _| {
            let editor = &input.editor;
            let style = window.text_style();
            // Only this display string reaches the text shaper, including during preedit.
            let display: SharedString = if editor.text.is_empty() {
                input.placeholder.clone()
            } else {
                editor.display_text(input.secret).into()
            };
            let run = TextRun {
                len: display.len(),
                font: style.font(),
                color: if editor.text.is_empty() {
                    rgb(0x737c8f).into()
                } else {
                    style.color
                },
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let runs = if let Some(marked) = &editor.marked {
                let start = editor.byte_to_display(marked.start, input.secret);
                let end = editor.byte_to_display(marked.end, input.secret);
                vec![
                    TextRun {
                        len: start,
                        ..run.clone()
                    },
                    TextRun {
                        len: end - start,
                        underline: Some(UnderlineStyle {
                            color: Some(rgb(0x9985ff).into()),
                            thickness: px(1.),
                            wavy: false,
                        }),
                        ..run.clone()
                    },
                    TextRun {
                        len: display.len() - end,
                        ..run
                    },
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect::<Vec<_>>()
            } else {
                vec![run]
            };
            let line = window.text_system().shape_line(
                display,
                style.font_size.to_pixels(window.rem_size()),
                &runs,
                None,
            );
            let cursor_x = line.x_for_index(editor.byte_to_display(editor.head, input.secret));
            let content_width = if editor.text.is_empty() {
                px(0.)
            } else {
                line.width
            };
            input.scroll_x = px(editor::scroll_offset(
                input.scroll_x.into(),
                cursor_x.into(),
                content_width.into(),
                bounds.size.width.into(),
            ));
            let origin = point(bounds.left() - input.scroll_x, bounds.top());
            let cursor = fill(
                Bounds::new(
                    point(origin.x + cursor_x, origin.y),
                    size(px(1.), bounds.size.height),
                ),
                rgb(0x9985ff),
            );
            let selected = editor.selection();
            let selection = if selected.is_empty() {
                None
            } else {
                let start = line.x_for_index(editor.byte_to_display(selected.start, input.secret));
                let end = line.x_for_index(editor.byte_to_display(selected.end, input.secret));
                Some(fill(
                    Bounds::from_corners(
                        point(origin.x + start.min(end), bounds.top()),
                        point(origin.x + start.max(end), bounds.bottom()),
                    ),
                    rgba(0x9985ff55),
                ))
            };
            input.last_bounds = Some(bounds);
            input.last_layout = Some(line.clone());
            PrepaintState {
                line,
                origin,
                cursor,
                selection,
            }
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let focused = focus.is_focused(window);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if focused && let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection);
            }
            let _ = prepaint
                .line
                .paint(prepaint.origin, bounds.size.height, window, cx);
            if focused {
                window.paint_quad(prepaint.cursor.clone());
            }
        });

        // Div's move listener only fires over its hitbox. Track drags outside it too.
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            input.update(cx, |input, cx| {
                if !input.is_selecting {
                    return;
                }
                if event.pressed_button != Some(MouseButton::Left)
                    || !input.focus_handle.is_focused(window)
                {
                    input.is_selecting = false;
                    return;
                }
                if let Some(index) = input.index_for_position(event.position) {
                    input.move_to(index, true, cx);
                }
            });
        });
        let input = self.input.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                input.update(cx, |input, _| input.is_selecting = false);
            }
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-input")
            .key_context("TextInput")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .w_full()
            .min_w(px(0.))
            .px(px(10.))
            .py(px(7.))
            .border_1()
            .rounded(px(5.))
            .bg(rgb(0x181b22))
            .border_color(rgb(0x353b48))
            .focus(|style| style.border_color(rgb(0x9985ff)))
            .text_color(rgb(0xdce0e8))
            .text_size(px(14.))
            .line_height(px(22.))
            .overflow_hidden()
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(|_, _: &Submit, _, cx| cx.emit(InputEvent::Submit)))
            .on_action(cx.listener(|_, _: &Escape, _, cx| cx.emit(InputEvent::Escape)))
            .on_action(cx.listener(|_, _: &Next, _, cx| cx.emit(InputEvent::Next)))
            .on_action(cx.listener(|_, _: &Previous, _, cx| cx.emit(InputEvent::Previous)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .child(TextElement { input: cx.entity() })
    }
}

// This module has no GPUI dependencies so editing invariants can be tested without a window.
mod editor {
    use std::ops::Range;
    use unicode_segmentation::UnicodeSegmentation;

    const MASK: &str = "\u{2022}";

    pub(super) struct Editor {
        pub text: String,
        pub anchor: usize,
        pub head: usize,
        pub marked: Option<Range<usize>>,
        max_chars: usize,
    }

    impl Editor {
        pub fn new(max_chars: usize) -> Self {
            Self {
                text: String::new(),
                anchor: 0,
                head: 0,
                marked: None,
                max_chars,
            }
        }

        pub fn selection(&self) -> Range<usize> {
            self.anchor.min(self.head)..self.anchor.max(self.head)
        }

        pub fn set_value(&mut self, value: &str) -> bool {
            let value = sanitize(value, self.max_chars);
            let changed = self.text != value;
            self.text = value;
            self.move_to(self.text.len(), false);
            changed
        }

        pub fn move_to(&mut self, offset: usize, extend: bool) {
            self.head = floor_boundary(&self.text, offset);
            if !extend {
                self.anchor = self.head;
            }
            self.marked = None;
        }

        pub fn horizontal(&mut self, right: bool, extend: bool) {
            let selection = self.selection();
            let offset = if !extend && !selection.is_empty() {
                if right {
                    selection.end
                } else {
                    selection.start
                }
            } else if right {
                next_grapheme(&self.text, self.head)
            } else {
                previous_grapheme(&self.text, self.head)
            };
            self.move_to(offset, extend);
        }

        pub fn delete(&mut self, forward: bool) -> bool {
            self.marked = None;
            if self.selection().is_empty() {
                // IME selections may end inside a grapheme. Delete the whole cluster.
                let head = self.head;
                let start = nearest_grapheme_floor(&self.text, head);
                if start != head {
                    self.anchor = start;
                    self.head = next_grapheme(&self.text, head);
                } else {
                    self.horizontal(forward, true);
                }
            }
            self.replace(None, "", None, false)
        }

        /// Explicit replacement ranges win over preedit, which wins over selection.
        pub fn replace(
            &mut self,
            range_utf16: Option<Range<usize>>,
            text: &str,
            selected_utf16: Option<Range<usize>>,
            composing: bool,
        ) -> bool {
            let range = range_utf16
                .map(|r| range_from_utf16(&self.text, r))
                .or_else(|| self.marked.clone())
                .unwrap_or_else(|| self.selection());
            let retained =
                self.text[..range.start].chars().count() + self.text[range.end..].chars().count();
            let capacity = self.max_chars.saturating_sub(retained);
            let inserted = sanitize(text, capacity);
            let changed = self.text[range.clone()] != inserted;
            self.text.replace_range(range.clone(), &inserted);
            let end = range.start + inserted.len();
            self.marked = (composing && !inserted.is_empty()).then_some(range.start..end);
            let selected = if composing {
                selected_utf16.map(|r| {
                    // Selection is relative to the incoming preedit, not the document.
                    let original = range_from_utf16(text, r);
                    let start = sanitize(&text[..original.start], capacity).len();
                    let end = sanitize(&text[..original.end], capacity).len();
                    range.start + start..range.start + end
                })
            } else {
                None
            }
            .unwrap_or(end..end);
            self.anchor = selected.start;
            self.head = selected.end;
            changed
        }

        pub fn clipboard_text(&self, secret: bool) -> Option<String> {
            (!secret && !self.selection().is_empty())
                .then(|| self.text[self.selection()].to_owned())
        }

        pub fn display_text(&self, secret: bool) -> String {
            if secret {
                MASK.repeat(self.text.chars().count())
            } else {
                self.text.clone()
            }
        }

        pub fn byte_to_display(&self, byte: usize, secret: bool) -> usize {
            let byte = floor_boundary(&self.text, byte);
            if secret {
                self.text[..byte].chars().count() * MASK.len()
            } else {
                byte
            }
        }

        pub fn display_to_byte(&self, byte: usize, secret: bool) -> usize {
            if secret {
                self.text
                    .char_indices()
                    .nth(byte / MASK.len())
                    .map_or(self.text.len(), |(i, _)| i)
            } else {
                floor_boundary(&self.text, byte)
            }
        }

        pub fn ime_text(&self, range: Range<usize>, secret: bool) -> String {
            let text = &self.text[range];
            // OS surrounding-text queries must preserve UTF-16 length without disclosing secrets.
            // Rendering uses one bullet per scalar; OS queries use one per UTF-16 code unit.
            if secret {
                MASK.repeat(text.encode_utf16().count())
            } else {
                text.to_owned()
            }
        }
    }

    pub(super) fn sanitize(text: &str, max_chars: usize) -> String {
        text.chars()
            .filter(|ch| !ch.is_control() && !matches!(ch, '\u{2028}' | '\u{2029}'))
            .take(max_chars)
            .collect()
    }

    fn floor_boundary(text: &str, offset: usize) -> usize {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    pub(super) fn byte_to_utf16(text: &str, byte: usize) -> usize {
        text[..floor_boundary(text, byte)].encode_utf16().count()
    }

    fn utf16_to_byte(text: &str, offset: usize, round_up: bool) -> usize {
        let mut units = 0;
        for (byte, ch) in text.char_indices() {
            if offset <= units {
                return byte;
            }
            units += ch.len_utf16();
            if offset < units {
                return if round_up { byte + ch.len_utf8() } else { byte };
            }
        }
        text.len()
    }

    pub(super) fn range_from_utf16(text: &str, range: Range<usize>) -> Range<usize> {
        let start = range.start.min(range.end);
        let end = range.start.max(range.end);
        let start_byte = utf16_to_byte(text, start, false);
        let end_byte = if start == end {
            start_byte
        } else {
            utf16_to_byte(text, end, true)
        };
        start_byte..end_byte
    }

    pub(super) fn range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
        byte_to_utf16(text, range.start)..byte_to_utf16(text, range.end)
    }

    fn previous_grapheme(text: &str, byte: usize) -> usize {
        text.grapheme_indices(true)
            .rev()
            .find(|(i, _)| *i < byte)
            .map_or(0, |(i, _)| i)
    }

    fn next_grapheme(text: &str, byte: usize) -> usize {
        text.grapheme_indices(true)
            .find(|(i, _)| *i > byte)
            .map_or(text.len(), |(i, _)| i)
    }

    fn nearest_grapheme_floor(text: &str, byte: usize) -> usize {
        if byte >= text.len() {
            return text.len();
        }
        text.grapheme_indices(true)
            .rev()
            .find(|(i, _)| *i <= byte)
            .map_or(0, |(i, _)| i)
    }

    pub(super) fn nearest_grapheme(text: &str, byte: usize) -> usize {
        let before = nearest_grapheme_floor(text, byte);
        let after = next_grapheme(text, before);
        if byte.saturating_sub(before) <= after.saturating_sub(byte) {
            before
        } else {
            after
        }
    }

    pub(super) fn scroll_offset(old: f32, cursor: f32, width: f32, viewport: f32) -> f32 {
        let visible = (viewport - 1.).max(0.);
        let max_scroll = (width - visible).max(0.);
        let mut scroll = old.clamp(0., max_scroll);
        if cursor < scroll {
            scroll = cursor;
        }
        if cursor > scroll + visible {
            scroll = cursor - visible;
        }
        scroll.clamp(0., max_scroll)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn input(text: &str, max: usize) -> Editor {
            let mut editor = Editor::new(max);
            editor.set_value(text);
            editor
        }

        #[test]
        fn utf16_ranges_never_split_surrogates_or_utf8() {
            let text = "a😀é中";
            assert_eq!(range_from_utf16(text, 1..3), 1..5);
            assert_eq!(range_from_utf16(text, 2..2), 1..1);
            assert_eq!(range_from_utf16(text, 2..3), 1..5);
            assert_eq!(range_from_utf16(text, 0..2), 0..5);
            assert_eq!(
                range_from_utf16(text, 99..usize::MAX),
                text.len()..text.len()
            );
            assert_eq!(range_from_utf16(text, Range { start: 4, end: 1 }), 1..7);
            for byte in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
                let utf16 = byte_to_utf16(text, byte);
                assert_eq!(range_from_utf16(text, utf16..utf16), byte..byte);
            }
            assert_eq!(byte_to_utf16(text, 3), 1);
        }

        #[test]
        fn keyboard_and_deletion_use_graphemes() {
            let mut e = input("e\u{301}👨‍👩‍👧‍👦🇩🇪", 100);
            e.horizontal(false, false);
            assert_eq!(&e.text[e.head..], "🇩🇪");
            e.delete(false);
            assert_eq!(e.text, "e\u{301}🇩🇪");
            e.delete(false);
            assert_eq!(e.text, "🇩🇪");
            e.delete(true);
            assert_eq!(e.text, "");
            assert!(!e.delete(false));
            assert!(!e.delete(true));
        }

        #[test]
        fn deletion_inside_ime_grapheme_removes_whole_cluster() {
            for forward in [false, true] {
                let mut e = input("e\u{301}x", 10);
                e.move_to(1, false);
                e.delete(forward);
                assert_eq!(e.text, "x");
            }
        }

        #[test]
        fn selection_can_reverse_and_collapse() {
            let mut e = input("abc", 10);
            e.move_to(1, false);
            e.move_to(3, true);
            e.move_to(0, true);
            assert_eq!(e.selection(), 0..1);
            assert!(e.head < e.anchor);
            e.horizontal(true, false);
            assert_eq!(e.selection(), 1..1);
            e.move_to(3, true);
            e.horizontal(false, false);
            assert_eq!(e.selection(), 1..1);
            assert_eq!(e.anchor, e.head);
        }

        #[test]
        fn limits_count_scalars_and_strip_controls_on_all_writes() {
            let mut e = input("a\n\r\t\0\u{7f}\u{85}\u{2028}\u{2029}😀éz", 3);
            assert_eq!(e.text, "a😀é");
            assert!(!e.replace(None, "z", None, false));
            assert!(e.replace(Some(1..3), "\n中xy", None, false));
            assert_eq!(e.text, "a中é");
            assert_eq!(e.selection(), 4..4);
            let mut zero = input("abc", 0);
            assert!(!zero.replace(None, "😀", Some(0..2), true));
            assert!(zero.text.is_empty());
            assert_eq!(zero.marked, None);
            assert_eq!(zero.selection(), 0..0);
        }

        #[test]
        fn ime_selection_is_relative_to_filtered_insert_not_document() {
            let mut e = input("abXYz", 6);
            e.replace(Some(2..4), "\n😀\téxy", Some(1..5), true);
            assert_eq!(e.text, "ab😀éxz");
            assert_eq!(e.marked, Some(2..9));
            assert_eq!(e.selection(), 2..8);
            assert_eq!(range_to_utf16(&e.text, e.selection()), 2..5);
            e.replace(None, "中", Some(1..100), true);
            assert_eq!(e.text, "ab中z");
            assert_eq!(e.marked, Some(2..5));
            assert_eq!(e.selection(), 5..5);
            e.replace(None, "文", None, false);
            assert_eq!(e.text, "ab文z");
            assert_eq!(e.marked, None);
            assert_eq!(e.selection(), 5..5);
        }

        #[test]
        fn explicit_ime_range_overrides_mark_and_empty_preedit_clears_it() {
            let mut e = input("abcd", 10);
            e.replace(Some(1..3), "XY", None, true);
            e.replace(Some(0..1), "Z", None, true);
            assert_eq!(e.text, "ZXYd");
            assert_eq!(e.marked, Some(0..1));
            e.replace(None, "", Some(99..100), true);
            assert_eq!(e.text, "XYd");
            assert_eq!(e.marked, None);
            assert_eq!(e.selection(), 0..0);
        }

        #[test]
        fn setting_same_value_resets_composition_without_change() {
            let mut e = input("abc", 3);
            e.replace(Some(0..3), "abc", Some(0..1), true);
            assert!(!e.set_value("abc"));
            assert_eq!(e.marked, None);
            assert_eq!(e.selection(), 3..3);
        }

        #[test]
        fn passwords_never_leave_via_render_clipboard_or_surrounding_text() {
            let mut e = input("a😀e\u{301}", 10);
            e.move_to(0, true);
            assert_eq!(e.display_text(true), "••••");
            assert_eq!(e.ime_text(0..e.text.len(), true), "•••••");
            assert_eq!(e.clipboard_text(true), None);
            assert_eq!(e.clipboard_text(false).as_deref(), Some("a😀e\u{301}"));
            for byte in e.text.char_indices().map(|(i, _)| i).chain([e.text.len()]) {
                let display = e.byte_to_display(byte, true);
                assert!(e.display_text(true).is_char_boundary(display));
                assert_eq!(e.display_to_byte(display, true), byte);
                assert_eq!(
                    e.display_to_byte(e.byte_to_display(byte, false), false),
                    byte
                );
            }
            assert_eq!(e.byte_to_display(5, true), 6);
            assert_eq!(e.display_to_byte(6, true), 5);
            assert_eq!(e.display_to_byte(usize::MAX, true), e.text.len());
        }

        #[test]
        fn mouse_snaps_combining_sequences_to_grapheme_boundaries() {
            assert_eq!(nearest_grapheme("e\u{301}x", 1), 0);
            assert_eq!(nearest_grapheme("e\u{301}x", 3), 3);
            assert_eq!(nearest_grapheme("", 0), 0);
        }

        #[test]
        fn scrolling_keeps_caret_visible_and_shrinks_after_deletion() {
            assert_eq!(scroll_offset(0., 200., 200., 50.), 151.);
            assert_eq!(scroll_offset(151., 0., 200., 50.), 0.);
            assert_eq!(scroll_offset(151., 10., 10., 50.), 0.);
            assert_eq!(scroll_offset(30., 50., 200., 50.), 30.);
            assert_eq!(scroll_offset(0., 10., 10., 0.), 10.);
            for cursor in 0..=200 {
                let scroll = scroll_offset(80., cursor as f32, 200., 50.);
                assert!(cursor as f32 >= scroll);
                assert!(cursor as f32 + 1. <= scroll + 50.);
            }
        }

        #[test]
        fn arbitrary_ime_offsets_preserve_editor_invariants() {
            for secret in [false, true] {
                for start in 0..12 {
                    for end in 0..12 {
                        let mut e = input("a😀e\u{301}中", 6);
                        e.replace(Some(start..end), "\n🇩🇪\tx", Some(start..end), true);
                        assert!(e.text.chars().count() <= 6);
                        assert!(e.text.is_char_boundary(e.anchor));
                        assert!(e.text.is_char_boundary(e.head));
                        assert!(e.anchor <= e.text.len() && e.head <= e.text.len());
                        if let Some(mark) = &e.marked {
                            assert!(e.text.is_char_boundary(mark.start));
                            assert!(e.text.is_char_boundary(mark.end));
                            assert!(e.anchor >= mark.start && e.head <= mark.end);
                        }
                        let display = e.display_text(secret);
                        assert!(display.is_char_boundary(e.byte_to_display(e.head, secret)));
                    }
                }
            }
        }
    }
}

/*
Upstream gpui 0.2.2 LICENSE-APACHE:

Copyright 2022 - 2025 Zed Industries, Inc.


   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at


       http://www.apache.org/licenses/LICENSE-2.0


   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.




Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/


   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION


   1. Definitions.


      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.


      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.


      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.


      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.


      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.


      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.


      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work
      (an example is provided in the Appendix below).


      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.


      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."


      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.


   2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.


   3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.


   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:


      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and


      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and


      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and


      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.


      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.


   5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.


   6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.


   7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.


   8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.


   9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.


   END OF TERMS AND CONDITIONS
*/
