use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Datelike, Local, Utc};
use gpui::{
    AnyElement, App, Bounds, Context, Div, Empty, Entity, FocusHandle, Focusable, KeyDownEvent,
    Keystroke, Modifiers, MouseButton, Pixels, ScrollHandle, SharedString, Stateful, Subscription,
    Svg, Task, Timer, Window, canvas, div, img, linear_color_stop, linear_gradient, prelude::*, px,
    relative, rgb, rgba, svg,
};

use crate::{
    domain::{
        self, CallDirection, CallOutcome, CallState, Contact, HistoryEntry, RegistrationState,
    },
    input::{InputEvent, TextInput},
    platform::NodeKind,
    session::{Action, AudioField, SessionHandle, Snapshot},
    storage::{AccountCredentials, MAX_CONTACT_NAME, MAX_FIELD_LENGTH},
};

use crate::settings::{
    Language, Preferences, Theme, omarchy_available, palette, preferences, reload_omarchy_theme, tr,
};
use crate::storage::Store;
/// Five-percent steps on the wpctl scale.
const VOLUME_STEPS: i32 = 20;
const VOLUME_HOLD: Duration = Duration::from_secs(1);
const VOLUME_KNOB: f32 = 16.;
/// The dial field and the volume control share this width, so their edges line up.
const PHONE_COLUMN_WIDTH: f32 = 360.;
/// Only the tail of a long DTMF sequence stays visible above the keypad.
const MAX_DTMF_DISPLAY: usize = 24;
const SHORTCUTS: [(&str, &[(&str, &str)]); 3] = [
    (
        "Navigation",
        &[
            (
                "Ctrl+1 … Ctrl+5",
                "Opens Phone, Contacts, Account, History or Settings",
            ),
            ("Ctrl+Tab", "Next view"),
            ("Ctrl+Shift+Tab", "Previous view"),
            ("Ctrl+F", "Jumps to the contact search"),
            ("F1", "Shows this help"),
        ],
    ),
    (
        "Phone",
        &[
            ("Typing", "Enters the number, even after Esc"),
            ("Enter", "Dials the number"),
            ("0-9, * and #", "Sends DTMF while the keypad is open"),
        ],
    ),
    (
        "Text fields",
        &[
            ("Tab", "Next field"),
            ("Shift+Tab", "Previous field"),
            ("Esc", "Leaves the field and cancels a new contact"),
            ("Enter", "Saves a new contact from the address field"),
        ],
    ),
];
const KEYPAD: [(char, &str); 12] = [
    ('1', ""),
    ('2', "ABC"),
    ('3', "DEF"),
    ('4', "GHI"),
    ('5', "JKL"),
    ('6', "MNO"),
    ('7', "PQRS"),
    ('8', "TUV"),
    ('9', "WXYZ"),
    ('*', ""),
    ('0', "+"),
    ('#', ""),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Phone,
    Contacts,
    Account,
    History,
    Settings,
    /// Sits apart at the end of the navigation and outside the Ctrl+digit order.
    Help,
}

impl View {
    const ALL: [Self; 5] = [
        Self::Phone,
        Self::Contacts,
        Self::Account,
        Self::History,
        Self::Settings,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Phone => "Phone",
            Self::Contacts => "Contacts",
            Self::Account => "Account",
            Self::History => "History",
            Self::Settings => "Settings",
            Self::Help => "Help",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Dial,
    Search,
    ContactName,
    ContactUri,
    Account(usize),
}

pub struct Workspace {
    session: Arc<SessionHandle>,
    settings_store: Option<Store>,
    settings_error: String,
    snapshot: Snapshot,
    focus: FocusHandle,
    active: View,
    dial: Entity<TextInput>,
    search: Entity<TextInput>,
    contact_name: Entity<TextInput>,
    contact_uri: Entity<TextInput>,
    account: Vec<Entity<TextInput>>,
    account_secure: bool,
    account_dirty: bool,
    account_cursor: usize,
    contact_cursor: usize,
    history_cursor: usize,
    audio_field: usize,
    audio_cursors: [usize; 3],
    contact_scroll: ScrollHandle,
    history_scroll: ScrollHandle,
    audio_scroll: [ScrollHandle; 3],
    adding_contact: bool,
    keypad_open: bool,
    dtmf_digits: String,
    focus_pending: bool,
    quit_pending: bool,
    volume_request: Option<(f32, Instant)>,
    /// Painted slider bounds; a click maps its x position onto the volume.
    volume_bounds: Rc<Cell<Bounds<Pixels>>>,
    dispatch_error: String,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

impl Workspace {
    pub fn new(
        session: Arc<SessionHandle>,
        interrupted: Arc<AtomicBool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot = session.snapshot();
        let dial = cx.new(|cx| {
            TextInput::new(
                "Number or SIP address",
                domain::MAX_DIAL_TARGET_LENGTH,
                false,
                cx,
            )
        });
        let search = cx.new(|cx| TextInput::new("Search contacts", MAX_FIELD_LENGTH, false, cx));
        let contact_name = cx.new(|cx| TextInput::new("Display name", MAX_CONTACT_NAME, false, cx));
        let contact_uri =
            cx.new(|cx| TextInput::new("sip:user@example.com", MAX_FIELD_LENGTH, false, cx));
        let placeholders = [
            "pbx.example.com",
            "Extension",
            "Defaults to server",
            "Defaults to user",
            "Leave blank to keep saved password",
        ];
        let account: Vec<_> = placeholders
            .iter()
            .enumerate()
            .map(|(i, placeholder)| {
                cx.new(|cx| TextInput::new(*placeholder, MAX_FIELD_LENGTH, i == 4, cx))
            })
            .collect();
        let mut subscriptions = Vec::new();
        for (field, input) in [
            (Field::Dial, &dial),
            (Field::Search, &search),
            (Field::ContactName, &contact_name),
            (Field::ContactUri, &contact_uri),
        ]
        .into_iter()
        .chain(
            account
                .iter()
                .enumerate()
                .map(|(i, input)| (Field::Account(i), input)),
        ) {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |this, _, event, window, cx| this.input_event(field, event, window, cx),
            ));
        }
        let focus = cx.focus_handle();
        window.focus(&focus);
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| {
                if this.snapshot.state.call_state != CallState::Idle {
                    this.quit_pending = true;
                    window.focus(&this.focus);
                    cx.notify();
                    false
                } else {
                    true
                }
            })
            .unwrap_or(true)
        });
        let poll = cx.spawn(async move |this, cx| {
            for tick in 1u32.. {
                Timer::after(Duration::from_millis(100)).await;
                if this
                    .update(cx, |this, cx| {
                        this.refresh(cx);
                        if tick % 10 == 0 {
                            this.reload_omarchy(cx);
                        }
                        if interrupted.load(Ordering::Relaxed) || !this.snapshot.running {
                            cx.quit();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut this = Self {
            session,
            settings_store: None,
            settings_error: String::new(),
            snapshot,
            focus,
            active: View::Phone,
            dial,
            search,
            contact_name,
            contact_uri,
            account,
            account_secure: false,
            account_dirty: false,
            account_cursor: 0,
            contact_cursor: 0,
            history_cursor: 0,
            audio_field: 0,
            audio_cursors: [0; 3],
            contact_scroll: ScrollHandle::new(),
            history_scroll: ScrollHandle::new(),
            audio_scroll: std::array::from_fn(|_| ScrollHandle::new()),
            adding_contact: false,
            keypad_open: false,
            dtmf_digits: String::new(),
            focus_pending: false,
            quit_pending: false,
            volume_request: None,
            volume_bounds: Rc::default(),
            dispatch_error: String::new(),
            _subscriptions: subscriptions,
            _poll: poll,
        };
        this.sync_account(cx);
        this.sync_audio(cx, None);
        this.dispatch(Action::WatchOutputVolume(true), cx);
        this.focus_default(window, cx);
        this
    }

    pub fn load_settings(&mut self, store: Store, cx: &mut Context<Self>) {
        match store.load_preferences() {
            Ok(settings) => {
                cx.set_global(settings);
                self.settings_error.clear();
            }
            Err(error) => self.settings_error = format!("{error:#}"),
        }
        self.settings_store = Some(store);
        self.reload_omarchy(cx);
        self.refresh_settings(cx);
    }

    fn reload_omarchy(&self, cx: &mut Context<Self>) {
        if preferences(cx).theme == Theme::Omarchy && reload_omarchy_theme(cx) {
            self.refresh_settings(cx);
        }
    }

    fn refresh_settings(&self, cx: &mut Context<Self>) {
        for input in [
            &self.dial,
            &self.search,
            &self.contact_name,
            &self.contact_uri,
        ]
        .into_iter()
        .chain(self.account.iter())
        {
            input.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    fn change_settings(&mut self, settings: Preferences, cx: &mut Context<Self>) {
        cx.set_global(settings);
        self.reload_omarchy(cx);
        self.settings_error = self
            .settings_store
            .as_ref()
            .and_then(|store| store.save_preferences(&settings).err())
            .map(|error| format!("{error:#}"))
            .unwrap_or_default();
        self.refresh_settings(cx);
    }

    fn settings_view(&self, cx: &Context<Self>) -> AnyElement {
        let settings = preferences(cx);
        // Without Omarchy the option would look like Dark; a saved choice stays visible.
        let omarchy = omarchy_available(cx) || settings.theme == Theme::Omarchy;
        // Unlike column(), no full height, so the audio lists extend the scroll area.
        let mut pane = div().flex().flex_col().gap_3();
        for (title, choices) in [
            (
                "Language",
                vec![
                    (
                        "language-en",
                        "English",
                        Preferences {
                            language: Language::English,
                            ..settings
                        },
                    ),
                    (
                        "language-de",
                        "German",
                        Preferences {
                            language: Language::German,
                            ..settings
                        },
                    ),
                ],
            ),
            (
                "Theme",
                vec![
                    (
                        "theme-dark",
                        "Dark",
                        Preferences {
                            theme: Theme::Dark,
                            ..settings
                        },
                    ),
                    (
                        "theme-light",
                        "Light",
                        Preferences {
                            theme: Theme::Light,
                            ..settings
                        },
                    ),
                ]
                .into_iter()
                .chain(omarchy.then_some((
                    "theme-omarchy",
                    "Omarchy",
                    Preferences {
                        theme: Theme::Omarchy,
                        ..settings
                    },
                )))
                .collect(),
            ),
        ] {
            pane = pane
                .child(div().child(tr(cx, title)))
                .child(div().flex().gap_2().children(choices.into_iter().map(
                    |(id, label, value)| {
                        button(
                            cx,
                            id,
                            format!(
                                "{}{}",
                                if value == settings { "✓ " } else { "" },
                                tr(cx, label)
                            ),
                            if value == settings {
                                palette(cx).accent
                            } else {
                                palette(cx).text
                            },
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.change_settings(value, cx)),
                        )
                    },
                )));
        }
        pane.child(self.audio_devices(cx))
            .child(
                div()
                    .text_color(rgb(palette(cx).muted))
                    .child(tr(cx, "Changes are saved automatically.")),
            )
            .into_any_element()
    }

    fn sync_account(&mut self, cx: &mut Context<Self>) {
        let a = &self.snapshot.account;
        for (input, value) in self
            .account
            .iter()
            .zip([&a.server, &a.username, &a.domain, &a.login])
        {
            input.update(cx, |input, cx| input.set_value_silent(value.clone(), cx));
        }
        self.account_secure = a.secure;
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let next = self.session.snapshot();
        if next.revision == self.snapshot.revision
            && next.now.timestamp() == self.snapshot.now.timestamp()
        {
            return;
        }
        let old = std::mem::replace(&mut self.snapshot, next);
        // Older snapshots may still be published while the worker sets earlier values.
        match self.volume_request {
            Some((volume, at))
                if at.elapsed() < VOLUME_HOLD
                    && old.audio_config.output == self.snapshot.audio_config.output
                    && self.snapshot.output_volume.is_some() =>
            {
                self.snapshot.output_volume = Some(volume)
            }
            _ => self.volume_request = None,
        }
        if let Some(selected) = old.history.get(self.history_cursor) {
            self.history_cursor = self
                .snapshot
                .history
                .iter()
                .position(|entry| history_identity(entry, selected))
                .unwrap_or(0);
        }
        if !self.account_dirty {
            self.sync_account(cx);
        }
        self.sync_audio(cx, Some(&old));
        self.contact_cursor = self
            .contact_cursor
            .min(self.contacts(cx).len().saturating_sub(1));
        self.history_cursor = self
            .history_cursor
            .min(self.snapshot.history.len().saturating_sub(1));
        match (old.state.call_state, self.snapshot.state.call_state) {
            (CallState::Idle, CallState::Idle) => {}
            (_, CallState::Idle) => {
                self.keypad_open = false;
                self.dtmf_digits.clear();
                // Other views keep their focus, for example a contact search in progress.
                self.focus_pending = self.active == View::Phone;
            }
            // Like Android, a new call takes over the screen wherever it starts.
            (CallState::Idle, _) => {
                self.set_view(View::Phone, cx);
                self.focus_pending = true;
            }
            _ => {}
        }
        cx.notify();
    }

    fn sync_audio(&mut self, cx: &App, old: Option<&Snapshot>) {
        for field in 0..3 {
            let selected = audio_selection(&self.snapshot, field);
            let changed = old.is_none_or(|old| audio_selection(old, field) != selected);
            let options = audio_options(cx, &self.snapshot, field);
            if changed {
                self.audio_cursors[field] =
                    options.iter().position(|o| o.0 == selected).unwrap_or(0);
            } else if let Some(old) = old {
                let previous = audio_options(cx, old, field)
                    .get(self.audio_cursors[field])
                    .map(|o| o.0.clone())
                    .unwrap_or_default();
                self.audio_cursors[field] =
                    options.iter().position(|o| o.0 == previous).unwrap_or(0);
            }
        }
    }

    fn dispatch(&mut self, action: Action, cx: &mut Context<Self>) {
        self.dispatch_error = self
            .session
            .dispatch(action)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        cx.notify();
    }

    fn contacts(&self, cx: &App) -> Vec<Contact> {
        domain::filter_contacts(&self.snapshot.contacts, self.search.read(cx).value())
    }

    fn change_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.set_view(view, cx);
        self.focus_default(window, cx);
    }

    /// The idle phone view is for typing a number, so its field takes focus.
    fn focus_default(&self, window: &mut Window, cx: &App) {
        if self.active == View::Phone && self.snapshot.state.call_state == CallState::Idle {
            self.focus_input(Field::Dial, window, cx);
        } else {
            window.focus(&self.focus);
        }
    }

    fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        // The volume control sits on the phone view and polls only while it is shown.
        if (self.active == View::Phone) != (view == View::Phone) {
            self.dispatch(Action::WatchOutputVolume(view == View::Phone), cx);
        }
        self.active = view;
        if view == View::Account {
            self.account_cursor = 0;
        }
        cx.notify();
    }

    fn send_digit(&mut self, digit: char, cx: &mut Context<Self>) {
        self.dtmf_digits.push(digit);
        if let Some((cut, _)) = self.dtmf_digits.char_indices().rev().nth(MAX_DTMF_DISPLAY) {
            self.dtmf_digits.drain(..=cut);
        }
        self.dispatch(Action::SendDigit(digit), cx);
    }

    fn typing(&self, window: &Window, cx: &App) -> bool {
        [
            &self.dial,
            &self.search,
            &self.contact_name,
            &self.contact_uri,
        ]
        .into_iter()
        .chain(self.account.iter())
        .any(|input| input.focus_handle(cx).is_focused(window))
    }

    fn focus_input(&self, field: Field, window: &mut Window, cx: &App) {
        let entity = match field {
            Field::Dial => &self.dial,
            Field::Search => &self.search,
            Field::ContactName => &self.contact_name,
            Field::ContactUri => &self.contact_uri,
            Field::Account(i) => &self.account[i],
        };
        window.focus(&entity.focus_handle(cx));
    }

    fn input_event(
        &mut self,
        field: Field,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Changed => {
                if matches!(field, Field::Account(_)) {
                    self.account_dirty = true;
                }
                if field == Field::Search {
                    self.contact_cursor = 0;
                }
            }
            InputEvent::Escape => {
                window.focus(&self.focus);
                if matches!(field, Field::ContactName | Field::ContactUri) {
                    self.adding_contact = false;
                }
            }
            InputEvent::Submit | InputEvent::Next | InputEvent::Previous => {
                let submit = matches!(event, InputEvent::Submit);
                match field {
                    Field::Dial => {
                        window.focus(&self.focus);
                        if submit {
                            self.dial_number(cx);
                        }
                    }
                    Field::Search => window.focus(&self.focus),
                    Field::ContactName => self.focus_input(Field::ContactUri, window, cx),
                    Field::ContactUri if submit => self.save_contact(window, cx),
                    Field::ContactUri => self.focus_input(Field::ContactName, window, cx),
                    Field::Account(i) => {
                        self.account_cursor = if matches!(event, InputEvent::Previous) {
                            (i + 6) % 7
                        } else {
                            i + 1
                        };
                        if self.account_cursor < 5 {
                            self.focus_input(Field::Account(self.account_cursor), window, cx);
                        } else {
                            window.focus(&self.focus);
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    fn dial_number(&mut self, cx: &mut Context<Self>) {
        let target = self.dial.read(cx).value().trim().to_owned();
        if !target.is_empty() {
            self.dispatch(Action::Dial(target), cx);
        }
    }

    fn begin_contact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adding_contact = true;
        self.contact_name
            .update(cx, |input, cx| input.set_value("", cx));
        self.contact_uri
            .update(cx, |input, cx| input.set_value("", cx));
        self.focus_input(Field::ContactName, window, cx);
        cx.notify();
    }

    fn save_contact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let uri = self.contact_uri.read(cx).value().trim().to_owned();
        if uri.is_empty() {
            return;
        }
        let name = self.contact_name.read(cx).value().trim().to_owned();
        self.dispatch(Action::AddContact(Contact { name, uri }), cx);
        self.adding_contact = false;
        window.focus(&self.focus);
    }

    fn save_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values: Vec<_> = self
            .account
            .iter()
            .map(|input| input.read(cx).value().to_owned())
            .collect();
        self.account[4].update(cx, |input, cx| input.set_value_silent("", cx));
        self.account_dirty = false;
        window.focus(&self.focus);
        self.dispatch(
            Action::SaveAccount(AccountCredentials {
                server: values[0].trim().into(),
                username: values[1].trim().into(),
                domain: values[2].trim().into(),
                login: values[3].trim().into(),
                password: values[4].clone(),
                secure: Some(self.account_secure),
            }),
            cx,
        );
    }

    /// Ctrl+1 to Ctrl+5, Ctrl+(Shift+)Tab, F1 and Ctrl+F switch views, also from text fields.
    /// Typing on the idle phone view goes to the dial field, even after Esc left it.
    /// During a call, typed digits go to an open keypad.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_pending {
            return;
        }
        if let Some(view) = self.view_shortcut(&event.keystroke) {
            cx.stop_propagation();
            self.change_view(view, window, cx);
            return;
        }
        if event.keystroke.key == "f" && event.keystroke.modifiers == Modifiers::control() {
            cx.stop_propagation();
            self.search_contacts(window, cx);
            return;
        }
        if self.active != View::Phone || self.typing(window, cx) {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let Some(text) = keystroke.key_char.as_deref() else {
            return;
        };
        if text.chars().any(char::is_control) {
            return;
        }
        let state = &self.snapshot.state;
        match state.call_state {
            CallState::Idle => {
                self.focus_input(Field::Dial, window, cx);
                self.dial.update(cx, |input, cx| input.insert(text, cx));
            }
            // An open keypad takes typed digits as DTMF, like clicking its keys.
            CallState::Active if self.keypad_open && !state.on_hold => {
                let mut chars = text.chars();
                if let (Some(digit), None) = (chars.next(), chars.next())
                    && domain::is_dtmf_digit(digit)
                {
                    self.send_digit(digit, cx);
                }
            }
            _ => {}
        }
    }

    /// Cancels an open contact form, because the search sits in the list.
    fn search_contacts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adding_contact = false;
        self.set_view(View::Contacts, cx);
        self.focus_input(Field::Search, window, cx);
    }

    fn view_shortcut(&self, keystroke: &Keystroke) -> Option<View> {
        let modifiers = &keystroke.modifiers;
        if keystroke.key == "f1" && !modifiers.modified() {
            return Some(View::Help);
        }
        if !modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return None;
        }
        let count = View::ALL.len();
        // From Help, Ctrl+Tab wraps around as if Help came after Settings.
        let current = View::ALL.iter().position(|view| *view == self.active);
        match keystroke.key.as_str() {
            "tab" if modifiers.shift => {
                Some(View::ALL[current.map_or(count - 1, |i| (i + count - 1) % count)])
            }
            "tab" => Some(View::ALL[current.map_or(0, |i| (i + 1) % count)]),
            _ if modifiers.shift => None,
            key => key
                .parse::<usize>()
                .ok()
                .and_then(|number| View::ALL.get(number.checked_sub(1)?).copied()),
        }
    }

    fn navigation(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let mut navigation = div()
            .flex()
            .gap_1()
            .p_2()
            .bg(rgb(palette(cx).panel))
            .border_color(rgb(palette(cx).border));
        if compact {
            navigation = navigation.flex_row().flex_wrap().border_b_1();
        } else {
            navigation = navigation
                .flex_col()
                .w(px(190.))
                .h_full()
                .border_r_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .px_3()
                        .py_4()
                        .child(img("logo/sippy-logo.png").size(px(64.))),
                );
        }
        navigation
            .children(View::ALL.iter().enumerate().map(|(i, view)| {
                let view = *view;
                div()
                    .id(("view", i))
                    .debug_selector(|| format!("view-{i}"))
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .bg(rgb(if self.active == view {
                        palette(cx).selection
                    } else {
                        palette(cx).panel
                    }))
                    .text_color(rgb(if self.active == view {
                        palette(cx).accent
                    } else {
                        palette(cx).muted
                    }))
                    .hover(|style| style.bg(rgb(palette(cx).hover)))
                    .child(tr(cx, view.name()))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.change_view(view, window, cx)),
                    )
            }))
            .when(!compact, |navigation| navigation.child(div().flex_1()))
            .child(
                div()
                    .id("view-help")
                    .debug_selector(|| "view-help".into())
                    .when(compact, |item| item.ml_auto())
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .bg(rgb(if self.active == View::Help {
                        palette(cx).selection
                    } else {
                        palette(cx).panel
                    }))
                    .hover(|style| style.bg(rgb(palette(cx).hover)))
                    .child(icon(
                        "icons/help.svg",
                        20.,
                        if self.active == View::Help {
                            palette(cx).accent
                        } else {
                            palette(cx).muted
                        },
                    ))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.change_view(View::Help, window, cx)),
                    ),
            )
            .into_any_element()
    }

    fn help_view(&self, cx: &Context<Self>) -> AnyElement {
        column()
            .children(SHORTCUTS.iter().map(|(title, rows)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .mb_3()
                    .child(div().child(tr(cx, title)))
                    .children(rows.iter().map(|(keys, description)| {
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .child(
                                div().w(px(160.)).flex_shrink_0().flex().child(
                                    div()
                                        .px_2()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(rgb(palette(cx).border))
                                        .bg(rgb(palette(cx).panel))
                                        .child(tr(cx, keys)),
                                ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(rgb(palette(cx).muted))
                                    .child(tr(cx, description)),
                            )
                    }))
            }))
            .into_any_element()
    }

    fn phone(&self, cx: &Context<Self>) -> AnyElement {
        let state = &self.snapshot.state;
        if state.call_state != CallState::Idle {
            return self.call_screen(cx);
        }
        let dnd = self.toggle(
            "dnd",
            "icons/do_not_disturb_on.svg",
            "DND",
            state.dnd,
            true,
            |this, cx| this.dispatch(Action::ToggleDnd, cx),
            cx,
        );
        let call = pressable(circle("dial".into(), 64., palette(cx).good))
            .child(icon("icons/call.svg", 28., palette(cx).panel))
            .on_click(cx.listener(|this, _, window, cx| {
                if this.dial.read(cx).value().trim().is_empty() {
                    this.focus_input(Field::Dial, window, cx);
                } else {
                    window.focus(&this.focus);
                    this.dial_number(cx);
                }
            }));
        let backspace = pressable(circle("dial-backspace".into(), 56., palette(cx).background))
            .child(icon("icons/backspace.svg", 24., palette(cx).muted))
            .on_click(cx.listener(|this, _, window, cx| {
                this.focus_input(Field::Dial, window, cx);
                this.dial.update(cx, |input, cx| input.delete_backward(cx));
            }));
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_between()
            .gap_6()
            .size_full()
            .min_h(px(420.))
            .pt_8()
            .pb_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .w_full()
                    .child(
                        div()
                            .max_w_full()
                            .truncate()
                            .text_color(rgb(registration_color(cx, &self.snapshot)))
                            .child(registration_text(cx, &self.snapshot)),
                    )
                    .child(
                        div()
                            .debug_selector(|| "input-Dial".into())
                            // A fixed width; with w_full and max_w the field took the
                            // whole pane width once the volume row joined the view.
                            .w(px(PHONE_COLUMN_WIDTH))
                            .max_w_full()
                            .child(self.dial.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_10()
                    .child(keypad_grid("dialpad", cx, |this, digit, window, cx| {
                        this.focus_input(Field::Dial, window, cx);
                        this.dial
                            .update(cx, |input, cx| input.insert(&digit.to_string(), cx));
                    }))
                    // DND and backspace flank the call button like Android's dialer.
                    // Equal slots keep the call button centred although only DND has a label.
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_4()
                            .child(div().flex().justify_center().w(px(88.)).child(dnd))
                            .child(call)
                            .child(div().flex().justify_center().w(px(88.)).child(backspace)),
                    )
                    .child(self.volume_control(cx)),
            )
            .into_any_element()
    }

    fn call_screen(&self, cx: &Context<Self>) -> AnyElement {
        let state = &self.snapshot.state;
        let name = display_peer(cx, &state.peer);
        let address = domain::peer_display(&state.call_target);
        let (status, status_color) = call_status(cx, &self.snapshot);
        let caller = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .w_full()
            .child(div().text_color(rgb(status_color)).child(status))
            .child(avatar(cx, &name))
            .child(div().max_w_full().truncate().text_3xl().child(name.clone()))
            .when(!address.is_empty() && address != name, |caller| {
                caller.child(
                    div()
                        .max_w_full()
                        .truncate()
                        .text_color(rgb(palette(cx).muted))
                        .child(address),
                )
            });
        let middle = match state.call_state {
            CallState::Active if self.keypad_open => self.keypad(cx),
            CallState::Active => self.call_controls(cx),
            _ => div().into_any_element(),
        };
        let hangup = labeled(
            cx,
            pressable(circle("hangup".into(), 64., palette(cx).error))
                .child(icon("icons/call_end.svg", 28., palette(cx).panel))
                .on_click(cx.listener(|this, _, _, cx| this.dispatch(Action::Hangup, cx))),
            "",
        );
        let actions = div().flex().justify_center().gap(px(96.));
        let actions = match state.call_state {
            CallState::Incoming => actions
                .child(labeled(
                    cx,
                    pressable(circle("reject".into(), 64., palette(cx).error))
                        .child(icon("icons/call_end.svg", 28., palette(cx).panel))
                        .on_click(cx.listener(|this, _, _, cx| this.dispatch(Action::Reject, cx))),
                    "Decline",
                ))
                .child(labeled(
                    cx,
                    pressable(circle("answer".into(), 64., palette(cx).good))
                        .child(icon("icons/call.svg", 28., palette(cx).panel))
                        .on_click(cx.listener(|this, _, _, cx| this.dispatch(Action::Answer, cx))),
                    "Answer",
                )),
            // The hide button sits right of hang-up; a same-sized spacer keeps hang-up centred.
            CallState::Active if self.keypad_open => actions
                .gap(px(32.))
                .child(div().size(px(56.)))
                .child(hangup)
                .child(
                    pressable(circle("keypad-hide".into(), 56., palette(cx).background))
                        .child(icon("icons/keyboard_hide.svg", 24., palette(cx).muted))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.keypad_open = false;
                            cx.notify();
                        })),
                ),
            _ => actions.child(hangup),
        };
        div()
            .id("call-screen")
            .debug_selector(|| "call-screen".into())
            .flex()
            .flex_col()
            .items_center()
            .justify_between()
            .gap_6()
            .size_full()
            .min_h(px(420.))
            .pt_8()
            .pb_4()
            .child(caller)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_10()
                    .child(middle)
                    .child(actions)
                    .child(self.volume_control(cx)),
            )
            .into_any_element()
    }

    fn call_controls(&self, cx: &Context<Self>) -> AnyElement {
        let state = &self.snapshot.state;
        let hold = state.on_hold;
        div()
            .flex()
            .flex_wrap()
            .justify_center()
            .gap_6()
            .child(self.toggle(
                "mute",
                "icons/mic_off.svg",
                "Mute",
                state.muted,
                !hold,
                |this, cx| this.dispatch(Action::ToggleMute, cx),
                cx,
            ))
            .child(self.toggle(
                "keypad",
                "icons/dialpad.svg",
                "Keypad",
                false,
                !hold,
                |this, cx| {
                    this.keypad_open = true;
                    cx.notify();
                },
                cx,
            ))
            .child(self.toggle(
                "hold",
                "icons/pause.svg",
                "Hold",
                hold,
                true,
                |this, cx| this.dispatch(Action::ToggleHold, cx),
                cx,
            ))
            .child(self.toggle(
                "dnd",
                "icons/do_not_disturb_on.svg",
                "DND",
                state.dnd,
                true,
                |this, cx| this.dispatch(Action::ToggleDnd, cx),
                cx,
            ))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn toggle(
        &self,
        id: &'static str,
        path: &'static str,
        label: &'static str,
        active: bool,
        enabled: bool,
        handler: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &Context<Self>,
    ) -> Div {
        let (bg, fg) = if active {
            (palette(cx).accent, palette(cx).panel)
        } else {
            (palette(cx).panel, palette(cx).text)
        };
        let button = circle(id.into(), 56., bg)
            .border_1()
            .border_color(rgb(if active {
                palette(cx).accent
            } else {
                palette(cx).border
            }))
            .child(icon(path, 24., fg));
        let button = if enabled {
            pressable(button).on_click(cx.listener(move |this, _, _, cx| handler(this, cx)))
        } else {
            button.opacity(0.4)
        };
        labeled(cx, button, label)
    }

    fn keypad(&self, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_4()
            .child(
                div()
                    .debug_selector(|| "dtmf-digits".into())
                    .h(px(32.))
                    .text_2xl()
                    .child(self.dtmf_digits.clone()),
            )
            .child(keypad_grid("dtmf", cx, |this, digit, _, cx| {
                this.send_digit(digit, cx)
            }))
            .into_any_element()
    }

    fn call_banner(&self, cx: &Context<Self>) -> AnyElement {
        let (status, color) = call_status(cx, &self.snapshot);
        div()
            .id("call-banner")
            .debug_selector(|| "call-banner".into())
            .flex()
            .items_center()
            .gap_3()
            .mb_4()
            .px_4()
            .py_2()
            .rounded_full()
            .cursor_pointer()
            .bg(rgb(color))
            .text_color(rgb(palette(cx).panel))
            .hover(|style| style.opacity(0.9))
            .child(div().flex_shrink_0().child(status))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(display_peer(cx, &self.snapshot.state.peer)),
            )
            .child(div().flex_shrink_0().child(tr(cx, "Return to call")))
            .on_click(cx.listener(|this, _, window, cx| this.change_view(View::Phone, window, cx)))
            .into_any_element()
    }

    fn contacts_view(&self, cx: &Context<Self>) -> AnyElement {
        if self.adding_contact {
            return column()
                .child(heading(cx, "Add contact"))
                .child(input_row(cx, "Name", self.contact_name.clone()))
                .child(input_row(cx, "SIP address", self.contact_uri.clone()))
                .child(
                    div()
                        .mt_3()
                        .text_color(rgb(palette(cx).muted))
                        .child(tr(cx, "Enter saves from the address field. Esc cancels.")),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            button(
                                cx,
                                "save-contact",
                                tr(cx, "Save contact"),
                                palette(cx).accent,
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_contact(window, cx)),
                            ),
                        )
                        .child(
                            button(cx, "cancel-contact", tr(cx, "Cancel"), palette(cx).text)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.adding_contact = false;
                                    window.focus(&this.focus);
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element();
        }
        let contacts = self.contacts(cx);
        let empty = contacts.is_empty();
        let mut pane =
            column()
                .child(
                    div()
                        .debug_selector(|| "input-Search".into())
                        .child(self.search.clone()),
                )
                .child(div().flex().gap_2().mt_2().child(
                    button(cx, "add-contact", tr(cx, "Add"), palette(cx).accent).on_click(
                        cx.listener(|this, _, window, cx| this.begin_contact(window, cx)),
                    ),
                ));
        if empty {
            pane = pane.child(
                div()
                    .mt_4()
                    .text_color(rgb(palette(cx).muted))
                    .child(tr(cx, "No matching contacts.")),
            );
        }
        pane.child(
            div()
                .id("contact-list")
                .mt_3()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.contact_scroll)
                .children(contacts.into_iter().enumerate().map(|(i, contact)| {
                    let named = !contact.name.trim().is_empty();
                    let name = if named {
                        domain::clamp_text(&contact.name, MAX_CONTACT_NAME)
                    } else {
                        domain::clamp_text(&contact.uri, MAX_FIELD_LENGTH)
                    };
                    let uri = domain::clamp_text(&contact.uri, MAX_FIELD_LENGTH);
                    let dial_uri = contact.uri.clone();
                    let remove_uri = contact.uri.clone();
                    row(cx, ("contact", i), self.contact_cursor == i)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().truncate().child(name))
                                .when(named, |text| {
                                    text.child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(rgb(palette(cx).muted))
                                            .child(uri),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .gap_1()
                                .child(
                                    icon_button(
                                        format!("contact-dial-{i}").into(),
                                        "icons/call.svg",
                                        palette(cx).good,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.dispatch(Action::Dial(dial_uri.clone()), cx)
                                        },
                                    )),
                                )
                                .child(
                                    icon_button(
                                        format!("contact-remove-{i}").into(),
                                        "icons/delete.svg",
                                        palette(cx).error,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.dispatch(
                                                Action::RemoveContact(remove_uri.clone()),
                                                cx,
                                            )
                                        },
                                    )),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.contact_cursor = i;
                            window.focus(&this.focus);
                            cx.notify();
                        }))
                })),
        )
        .into_any_element()
    }

    fn audio_devices(&self, cx: &Context<Self>) -> AnyElement {
        let mut pane = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().child(tr(cx, "Audio")));
        if self.snapshot.ringtone_restart_required {
            pane = pane.child(div().text_color(rgb(palette(cx).warning)).child(tr(
                cx,
                "Restart Sippy to ring on the selected ringtone output.",
            )));
        }
        for (field, title) in ["Output", "Input", "Ringtone"].into_iter().enumerate() {
            let selected = audio_selection(&self.snapshot, field);
            pane = pane.child(
                div()
                    .flex()
                    .flex_col()
                    .h(px(160.))
                    .border_1()
                    .border_color(rgb(palette(cx).border))
                    .rounded_sm()
                    .p_2()
                    .gap_1()
                    .child(
                        div()
                            .text_color(rgb(if field == self.audio_field {
                                palette(cx).accent
                            } else {
                                palette(cx).text
                            }))
                            .child(tr(cx, title)),
                    )
                    .child(
                        div()
                            .id(("audio-list", field))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.audio_scroll[field])
                            .children(
                                audio_options(cx, &self.snapshot, field)
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, (name, label))| {
                                        row(
                                            cx,
                                            ("audio-option", field * 10000 + i),
                                            self.audio_field == field
                                                && self.audio_cursors[field] == i,
                                        )
                                        .child(if name == selected { "✓" } else { " " })
                                        .child(div().truncate().child(label))
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.audio_field = field;
                                                this.audio_cursors[field] = i;
                                                window.focus(&this.focus);
                                                this.dispatch(
                                                    Action::SetAudio {
                                                        field: audio_field(field),
                                                        name: name.clone(),
                                                    },
                                                    cx,
                                                );
                                            }),
                                        )
                                    }),
                            ),
                    ),
            );
        }
        pane.into_any_element()
    }

    fn volume_control(&self, cx: &Context<Self>) -> impl IntoElement {
        let fraction = self
            .snapshot
            .output_volume
            .map_or(0.0, |v| v.clamp(0.0, 1.0));
        let value = if let Some(volume) = self.snapshot.output_volume {
            format!("{}%", (volume * 100.0).round() as i32)
        } else {
            tr(cx, "Unavailable").to_owned()
        };
        let colors = palette(cx);
        let bounds = self.volume_bounds.clone();
        // The label matches the button captions. Slider and value span the dial field's width.
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .w(px(PHONE_COLUMN_WIDTH))
            .max_w_full()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(colors.muted))
                    .child(tr(cx, "Output volume")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .w_full()
                    .child(
                        // The margin keeps the knob inside the column at 0 and 100 %.
                        div()
                            .id("volume-slider")
                            .debug_selector(|| "volume-slider".into())
                            .relative()
                            .flex()
                            .items_center()
                            .flex_1()
                            .min_w_0()
                            .mx(px(VOLUME_KNOB / 2.))
                            .h(px(VOLUME_KNOB + 4.))
                            .cursor_pointer()
                            .child(
                                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                                    .absolute()
                                    .size_full(),
                            )
                            .child(
                                // Green to yellow to red over the whole track; the grey
                                // cover hides the part above the current level.
                                div()
                                    .relative()
                                    .w_full()
                                    .h(px(8.))
                                    .child(
                                        div()
                                            .absolute()
                                            .left_0()
                                            .w(relative(0.5))
                                            .h_full()
                                            .rounded_l_full()
                                            .bg(linear_gradient(
                                                90.,
                                                linear_color_stop(rgb(colors.good), 0.),
                                                linear_color_stop(rgb(colors.warning), 1.),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .left(relative(0.5))
                                            .w(relative(0.5))
                                            .h_full()
                                            .rounded_r_full()
                                            .bg(linear_gradient(
                                                90.,
                                                linear_color_stop(rgb(colors.warning), 0.),
                                                linear_color_stop(rgb(colors.error), 1.),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .right_0()
                                            .w(relative(1. - fraction))
                                            .h_full()
                                            .rounded_r_full()
                                            .when(fraction == 0., |d| d.rounded_l_full())
                                            .bg(rgb(colors.border)),
                                    ),
                            )
                            .when(self.snapshot.output_volume.is_some(), |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .left(relative(fraction))
                                        .ml(px(-VOLUME_KNOB / 2.))
                                        .size(px(VOLUME_KNOB))
                                        .rounded_full()
                                        .bg(rgb(colors.text))
                                        .border_2()
                                        .border_color(rgb(colors.background)),
                                )
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                                    window.focus(&this.focus);
                                    let bounds = this.volume_bounds.get();
                                    this.set_volume_steps(
                                        slider_steps(bounds, event.position.x),
                                        cx,
                                    );
                                }),
                            )
                            .on_drag(VolumeDrag, |_, _, _, cx| cx.new(|_| VolumeDrag))
                            .on_drag_move(cx.listener(
                                |this, event: &gpui::DragMoveEvent<VolumeDrag>, _, cx| {
                                    let steps = slider_steps(event.bounds, event.event.position.x);
                                    // Dragging within one step must not spawn another wpctl call.
                                    if this.snapshot.output_volume.map(volume_steps) != Some(steps)
                                    {
                                        this.set_volume_steps(steps, cx);
                                    }
                                },
                            )),
                    )
                    .child(div().flex_shrink_0().whitespace_nowrap().child(value)),
            )
    }

    fn set_volume_steps(&mut self, steps: i32, cx: &mut Context<Self>) {
        if self.snapshot.output_volume.is_none() {
            return;
        }
        let volume = steps.clamp(0, VOLUME_STEPS) as f32 / VOLUME_STEPS as f32;
        // Snapshots arrive by polling; repeated keys must step from the chosen value.
        self.snapshot.output_volume = Some(volume);
        self.dispatch(Action::SetOutputVolume(volume), cx);
        self.volume_request = self
            .dispatch_error
            .is_empty()
            .then(|| (volume, Instant::now()));
    }

    fn account_view(&self, cx: &Context<Self>) -> AnyElement {
        let mut pane = column();
        for (i, label) in ["Server", "User", "Domain", "Login", "Password"]
            .into_iter()
            .enumerate()
        {
            pane = pane.child(
                div()
                    .border_l_2()
                    .border_color(rgb(if self.account_cursor == i {
                        palette(cx).accent
                    } else {
                        palette(cx).background
                    }))
                    .pl_2()
                    .child(input_row(cx, label, self.account[i].clone())),
            );
        }
        if self.snapshot.account.has_password && self.account[4].read(cx).value().is_empty() {
            pane = pane.child(
                div()
                    .pl(px(152.))
                    .text_color(rgb(palette(cx).muted))
                    .text_sm()
                    .child(tr(cx, "Saved password will be kept")),
            );
        }
        pane.child(
            div().mt_3().child(
                button(
                    cx,
                    "secure",
                    if self.account_secure {
                        tr(cx, "[x] TLS and SRTP")
                    } else {
                        tr(cx, "[ ] TLS and SRTP")
                    },
                    if self.account_cursor == 5 {
                        palette(cx).accent
                    } else {
                        palette(cx).text
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.account_cursor = 5;
                    this.account_secure = !this.account_secure;
                    this.account_dirty = true;
                    window.focus(&this.focus);
                    cx.notify();
                })),
            ),
        )
        .child(
            div().mt_3().child(
                button(
                    cx,
                    "save-account",
                    tr(cx, "Save account"),
                    if self.account_cursor == 6 {
                        palette(cx).accent
                    } else {
                        palette(cx).text
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| this.save_account(window, cx))),
            ),
        )
        .into_any_element()
    }

    fn history(&self, cx: &Context<Self>) -> AnyElement {
        let mut pane = column();
        if self.snapshot.history.is_empty() {
            pane = pane.child(
                div()
                    .text_color(rgb(palette(cx).muted))
                    .child(tr(cx, "No calls yet.")),
            );
        }
        if let Some(call) = self.snapshot.history.get(self.history_cursor) {
            let target = call.target.clone();
            pane = pane.child(
                button(cx, "redial", tr(cx, "Dial"), palette(cx).accent).on_click(cx.listener(
                    move |this, _, _, cx| this.dispatch(Action::Dial(target.clone()), cx),
                )),
            );
        }
        pane.child(
            div()
                .id("history-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.history_scroll)
                .children(self.snapshot.history.iter().enumerate().map(|(i, call)| {
                    let target = call.target.clone();
                    let peer = if call.peer.is_empty() {
                        &call.target
                    } else {
                        &call.peer
                    };
                    row(cx, ("history", i), self.history_cursor == i)
                        .child(if call.direction == CallDirection::Incoming {
                            "↓"
                        } else {
                            "↑"
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(domain::clamp_text(peer, domain::MAX_PEER_DISPLAY_LENGTH)),
                        )
                        .child(div().w(px(112.)).child(history_result(cx, call)))
                        .child(
                            div()
                                .w(px(148.))
                                .text_color(rgb(palette(cx).muted))
                                .child(history_time(cx, call.ended_at, self.snapshot.now)),
                        )
                        .child(
                            icon_button(
                                format!("history-dial-{i}").into(),
                                "icons/call.svg",
                                palette(cx).good,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.dispatch(Action::Dial(target.clone()), cx)
                                },
                            )),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.history_cursor = i;
                            window.focus(&this.focus);
                            cx.notify();
                        }))
                })),
        )
        .into_any_element()
    }

    fn footer(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let state = &self.snapshot.state;
        let incoming = state.call_state == CallState::Incoming;
        let mut call = tr(cx, call_text(state.call_state)).to_owned();
        if state.call_state == CallState::Active {
            call.push_str(&format!(
                " {}",
                duration(state.call_started_at, self.snapshot.now)
            ));
        }
        if !compact && state.call_state != CallState::Idle {
            call.push_str(&format!(" {}", display_peer(cx, &state.peer)));
        }
        let mut error = if !self.settings_error.is_empty() {
            format!("{}: {}", tr(cx, "Settings"), self.settings_error)
        } else if self.dispatch_error.is_empty() {
            self.snapshot.last_error.clone()
        } else {
            self.dispatch_error.clone()
        };
        let password = self.account[4].read(cx).value();
        if !password.is_empty() {
            error = error.replace(password, "••••");
        }
        let mut footer = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .border_t_1()
            .border_color(rgb(palette(cx).border));
        if !error.is_empty() {
            footer = footer.child(
                div()
                    .px_4()
                    .py_2()
                    .text_color(rgb(palette(cx).error))
                    .child(format!(
                        "{}: {}",
                        tr(cx, "Error"),
                        domain::clamp_text(&error, domain::MAX_COMMAND_ERROR)
                    )),
            );
        }
        footer
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .bg(rgb(if incoming {
                        palette(cx).warning
                    } else {
                        palette(cx).panel
                    }))
                    .text_color(rgb(if incoming {
                        palette(cx).panel
                    } else {
                        palette(cx).text
                    }))
                    .child(
                        div()
                            .text_color(rgb(if incoming {
                                palette(cx).panel
                            } else {
                                registration_color(cx, &self.snapshot)
                            }))
                            .child(format!("● {}", tr(cx, short_registration(&self.snapshot)))),
                    )
                    .when(
                        !compact && !self.snapshot.account.username.is_empty(),
                        |bar| {
                            bar.child(div().max_w(px(120.)).truncate().child(domain::clamp_text(
                                &self.snapshot.account.username,
                                MAX_FIELD_LENGTH,
                            )))
                        },
                    )
                    .child("│")
                    .child(div().flex_1().truncate().child(call))
                    .when(
                        state.call_state == CallState::Active && state.muted,
                        |bar| bar.child(tr(cx, "Muted")),
                    )
                    .child(tr(cx, if state.dnd { "DND on" } else { "DND off" })),
            )
            .into_any_element()
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // refresh has no window, so focus follows call start and end here.
        if std::mem::take(&mut self.focus_pending) {
            self.focus_default(window, cx);
        }
        let compact = window.viewport_size().width < px(760.);
        let banner = (self.snapshot.state.call_state != CallState::Idle
            && self.active != View::Phone)
            .then(|| self.call_banner(cx));
        let body = match self.active {
            View::Phone => self.phone(cx),
            View::Contacts => self.contacts_view(cx),
            View::Account => self.account_view(cx),
            View::History => self.history(cx),
            View::Settings => self.settings_view(cx),
            View::Help => self.help_view(cx),
        };
        div()
            .id("workspace")
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .bg(rgb(palette(cx).background))
            .text_color(rgb(palette(cx).text))
            .font_family("sans-serif")
            .text_size(px(14.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .when(compact, |root| root.child(self.navigation(true, cx)))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(!compact, |main| main.child(self.navigation(false, cx)))
                    .child(
                        div()
                            .id("active-pane")
                            .flex_1()
                            .min_w_0()
                            .p_6()
                            .overflow_y_scroll()
                            .children(banner)
                            .child(body),
                    ),
            )
            .child(self.footer(compact, cx))
            .when(self.quit_pending, |root| {
                root.child(
                    div()
                        .occlude()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgba((palette(cx).overlay << 8) | 0xaa))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .p_6()
                                .bg(rgb(palette(cx).panel))
                                .border_1()
                                .border_color(rgb(palette(cx).warning))
                                .rounded_md()
                                .child(tr(cx, "A call is in progress. Quit anyway?"))
                                .child(
                                    div()
                                        .flex()
                                        .gap_3()
                                        .child(
                                            button(
                                                cx,
                                                "confirm-quit",
                                                tr(cx, "Quit"),
                                                palette(cx).error,
                                            )
                                            .on_click(cx.listener(|_, _, _, cx| cx.quit())),
                                        )
                                        .child(
                                            button(
                                                cx,
                                                "cancel-quit",
                                                tr(cx, "Stay"),
                                                palette(cx).text,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.quit_pending = false;
                                                    cx.notify();
                                                }),
                                            ),
                                        ),
                                ),
                        ),
                )
            })
    }
}

fn column() -> Div {
    div().flex().flex_col().gap_3().size_full()
}
fn heading(cx: &App, title: &'static str) -> Div {
    div()
        .mb_3()
        .text_xl()
        .text_color(rgb(palette(cx).accent))
        .child(tr(cx, title))
}
fn input_row(cx: &App, label: &'static str, input: Entity<TextInput>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(140.))
                .flex_shrink_0()
                .text_color(rgb(palette(cx).muted))
                .child(tr(cx, label)),
        )
        .child(
            div()
                .debug_selector(|| format!("input-{label}"))
                .flex_1()
                .min_w_0()
                .child(input),
        )
}
fn circle(id: SharedString, size: f32, bg: u32) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full()
        .bg(rgb(bg))
}
fn pressable(button: Stateful<Div>) -> Stateful<Div> {
    button.cursor_pointer().hover(|style| style.opacity(0.85))
}
fn keypad_grid(
    prefix: &'static str,
    cx: &Context<Workspace>,
    press: fn(&mut Workspace, char, &mut Window, &mut Context<Workspace>),
) -> Div {
    div()
        .grid()
        .grid_cols(3)
        .gap_3()
        .children(KEYPAD.iter().map(|&(digit, letters)| {
            pressable(circle(
                format!("{prefix}-{digit}").into(),
                56.,
                palette(cx).panel,
            ))
            .flex_col()
            .gap_0()
            .child(div().text_xl().child(digit.to_string()))
            .when(!letters.is_empty(), |key| {
                key.child(
                    div()
                        .text_size(px(9.))
                        .text_color(rgb(palette(cx).muted))
                        .child(letters),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| press(this, digit, window, cx)))
        }))
}
fn icon(path: &'static str, size: f32, color: u32) -> Svg {
    svg().path(path).size(px(size)).text_color(rgb(color))
}
fn icon_button(id: SharedString, path: &'static str, color: u32) -> Stateful<Div> {
    let selector = id.to_string();
    pressable(
        div()
            .id(id)
            .debug_selector(move || selector)
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(px(32.)),
    )
    .child(icon(path, 18., color))
}
fn labeled(cx: &App, button: impl IntoElement, label: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(button)
        .when(!label.is_empty(), |column| {
            column.child(
                div()
                    .text_sm()
                    .text_color(rgb(palette(cx).muted))
                    .child(tr(cx, label)),
            )
        })
}
fn avatar(cx: &App, name: &str) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .size(px(96.))
        .rounded_full()
        .bg(rgb(palette(cx).panel))
        .border_1()
        .border_color(rgb(palette(cx).border))
        .text_size(px(36.))
        .text_color(rgb(palette(cx).accent))
        .child(initials(name))
}
/// First letters of the first two words that start with a letter, so "Alice (203)" gives "A".
fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next().filter(|first| first.is_alphabetic()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}
fn call_status(cx: &App, snapshot: &Snapshot) -> (String, u32) {
    let state = &snapshot.state;
    match state.call_state {
        CallState::Incoming => (tr(cx, "Incoming call").into(), palette(cx).warning),
        CallState::Outgoing => (tr(cx, "Calling…").into(), palette(cx).warning),
        CallState::Active if state.on_hold => (tr(cx, "On hold").into(), palette(cx).warning),
        CallState::Active => (
            duration(state.call_started_at, snapshot.now),
            palette(cx).good,
        ),
        CallState::Idle => (String::new(), palette(cx).muted),
    }
}
fn button(cx: &App, id: &'static str, label: impl Into<SharedString>, color: u32) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(|| id.to_owned())
        .px_3()
        .py_2()
        .rounded_sm()
        .border_1()
        .border_color(rgb(palette(cx).border))
        .bg(rgb(palette(cx).panel))
        .text_color(rgb(color))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(palette(cx).hover)))
        .child(label.into())
}
fn row(cx: &App, id: (&'static str, usize), selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(|| format!("{}-{}", id.0, id.1))
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_sm()
        .cursor_pointer()
        .bg(rgb(if selected {
            palette(cx).selection
        } else {
            palette(cx).background
        }))
        .text_color(rgb(if selected {
            palette(cx).selection_text
        } else {
            palette(cx).text
        }))
        .hover(|style| style.bg(rgb(palette(cx).hover)))
}
fn volume_steps(volume: f32) -> i32 {
    (volume.clamp(0.0, 1.0) * VOLUME_STEPS as f32).round() as i32
}
fn slider_steps(bounds: Bounds<Pixels>, x: Pixels) -> i32 {
    let width = f32::from(bounds.size.width);
    if width <= 0. {
        return 0;
    }
    volume_steps(f32::from(x - bounds.origin.x) / width)
}

/// Drag payload of the volume slider; it renders nothing under the cursor.
struct VolumeDrag;

impl Render for VolumeDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

fn audio_field(index: usize) -> AudioField {
    [AudioField::Output, AudioField::Input, AudioField::Ringtone][index]
}
fn audio_selection(snapshot: &Snapshot, field: usize) -> &str {
    match field {
        1 => &snapshot.audio_config.input,
        2 => &snapshot.audio_config.alert,
        _ => &snapshot.audio_config.output,
    }
}
fn audio_options(cx: &App, snapshot: &Snapshot, field: usize) -> Vec<(String, String)> {
    let mut options = vec![(
        String::new(),
        tr(
            cx,
            if field == 2 {
                "Same as output"
            } else {
                "System default"
            },
        )
        .into(),
    )];
    let kind = if field == 1 {
        NodeKind::Input
    } else {
        NodeKind::Output
    };
    for node in snapshot.audio_nodes.iter().filter(|node| node.kind == kind) {
        let description = if node.description.trim().is_empty() {
            &node.name
        } else {
            &node.description
        };
        let mut label = domain::clamp_text(description, MAX_FIELD_LENGTH);
        if node.is_default {
            label.push_str(tr(cx, " (current system default)"));
        }
        options.push((node.name.clone(), label));
    }
    let selected = audio_selection(snapshot, field);
    if !options.iter().any(|o| o.0 == selected) {
        options.push((
            selected.into(),
            format!(
                "{} ({})",
                domain::clamp_text(selected, MAX_FIELD_LENGTH),
                tr(cx, "unavailable")
            ),
        ));
    }
    options
}
fn call_text(state: CallState) -> &'static str {
    match state {
        CallState::Incoming => "Incoming",
        CallState::Outgoing => "Calling",
        CallState::Active => "Active",
        _ => "Idle",
    }
}
fn display_peer(cx: &App, peer: &str) -> String {
    if peer.trim().is_empty() {
        tr(cx, "Unknown peer").into()
    } else {
        domain::clamp_text(peer, domain::MAX_PEER_DISPLAY_LENGTH)
    }
}
fn short_registration(snapshot: &Snapshot) -> &'static str {
    if snapshot.state.registered {
        return "Registered";
    }
    match snapshot.state.registration {
        RegistrationState::Registering => "Registering",
        RegistrationState::Failed => "Failed",
        RegistrationState::Unregistered => "Offline",
        RegistrationState::Registered => "Registered",
        _ => "Unknown",
    }
}
fn registration_text(cx: &App, snapshot: &Snapshot) -> String {
    if snapshot.state.registration_detail.is_empty() {
        tr(cx, short_registration(snapshot)).into()
    } else {
        let detail = &snapshot.state.registration_detail;
        let translated = if let Some(reason) = detail.strip_prefix("registration failed: ") {
            format!("{}: {}", tr(cx, "registration failed"), reason)
        } else {
            tr(cx, detail).to_owned()
        };
        domain::clamp_text(&translated, domain::MAX_COMMAND_ERROR)
    }
}
fn registration_color(cx: &App, snapshot: &Snapshot) -> u32 {
    if snapshot.state.registered || snapshot.state.registration == RegistrationState::Registered {
        palette(cx).good
    } else if snapshot.state.registration == RegistrationState::Failed {
        palette(cx).error
    } else {
        palette(cx).warning
    }
}
fn duration(start: Option<DateTime<Utc>>, end: DateTime<Utc>) -> String {
    let seconds = start
        .map(|start| (end - start).num_seconds().max(0))
        .unwrap_or(0);
    if seconds >= 3600 {
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}
fn history_result(cx: &App, call: &HistoryEntry) -> String {
    match call.outcome {
        CallOutcome::Connected => call
            .ended_at
            .map(|end| duration(call.connected_at, end))
            .unwrap_or_else(|| "00:00".into()),
        CallOutcome::Missed => tr(cx, "Missed").into(),
        CallOutcome::Rejected => tr(cx, "Rejected").into(),
        CallOutcome::RejectedDnd => tr(cx, "DND").into(),
        CallOutcome::RejectedBusy => tr(cx, "Busy").into(),
        CallOutcome::Canceled => tr(cx, "Canceled").into(),
        _ => tr(cx, "Not connected").into(),
    }
}
fn history_time(cx: &App, ended: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(ended) = ended else {
        return tr(cx, "Unknown").into();
    };
    let local = ended.with_timezone(&Local);
    let today = now.with_timezone(&Local).date_naive();
    if local.date_naive() == today {
        format!("{} {}", tr(cx, "Today"), local.format("%H:%M"))
    } else if Some(local.date_naive()) == today.pred_opt() {
        format!("{} {}", tr(cx, "Yesterday"), local.format("%H:%M"))
    } else if preferences(cx).language == Language::German {
        local.format("%d.%m.%Y %H:%M").to_string()
    } else if local.year() != today.year() {
        local.format("%d %b %Y %H:%M").to_string()
    } else {
        local.format("%d %b %H:%M").to_string()
    }
}
fn history_identity(left: &HistoryEntry, right: &HistoryEntry) -> bool {
    left.target == right.target
        && left.started_at == right.started_at
        && left.ended_at == right.ended_at
        && left.direction == right.direction
        && left.outcome == right.outcome
        && left.connected_at == right.connected_at
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod interaction_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations_handle_unconnected_and_clock_skew() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        assert_eq!(duration(None, now), "00:00");
        assert_eq!(
            duration(Some(now + chrono::Duration::seconds(1)), now),
            "00:00"
        );
        assert_eq!(
            duration(Some(now - chrono::Duration::seconds(3661)), now),
            "01:01:01"
        );
    }
    #[test]
    fn navigation_order() {
        assert_eq!(
            View::ALL.map(View::name),
            ["Phone", "Contacts", "Account", "History", "Settings"]
        );
    }
}
