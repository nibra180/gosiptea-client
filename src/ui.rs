use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Datelike, Local, Utc};
use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, KeyDownEvent, ScrollHandle,
    SharedString, Stateful, Subscription, Task, Timer, Window, div, prelude::*, px, rgb, rgba,
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

const BACKGROUND: u32 = 0x1f232b;
const PANEL: u32 = 0x181b22;
const BORDER: u32 = 0x353b48;
const TEXT: u32 = 0xdce0e8;
const MUTED: u32 = 0x959cab;
const ACCENT: u32 = 0x9985ff;
const GOOD: u32 = 0x7fc99a;
const WARNING: u32 = 0xe5b567;
const ERROR: u32 = 0xef8585;
/// Five-percent steps on the wpctl scale.
const VOLUME_STEPS: i32 = 20;
const VOLUME_HOLD: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Phone,
    Contacts,
    Audio,
    Account,
    History,
}

impl View {
    const ALL: [Self; 5] = [
        Self::Phone,
        Self::Contacts,
        Self::Audio,
        Self::Account,
        Self::History,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Phone => "Phone",
            Self::Contacts => "Contacts",
            Self::Audio => "Audio",
            Self::Account => "Account",
            Self::History => "History",
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
    quit_pending: bool,
    volume_request: Option<(f32, Instant)>,
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
            loop {
                Timer::after(Duration::from_millis(100)).await;
                if this
                    .update(cx, |this, cx| {
                        this.refresh(cx);
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
            quit_pending: false,
            volume_request: None,
            dispatch_error: String::new(),
            _subscriptions: subscriptions,
            _poll: poll,
        };
        this.sync_account(cx);
        this.sync_audio(None);
        this
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
        self.sync_audio(Some(&old));
        self.contact_cursor = self
            .contact_cursor
            .min(self.contacts(cx).len().saturating_sub(1));
        self.history_cursor = self
            .history_cursor
            .min(self.snapshot.history.len().saturating_sub(1));
        cx.notify();
    }

    fn sync_audio(&mut self, old: Option<&Snapshot>) {
        for field in 0..3 {
            let selected = audio_selection(&self.snapshot, field);
            let changed = old.is_none_or(|old| audio_selection(old, field) != selected);
            let options = audio_options(&self.snapshot, field);
            if changed {
                self.audio_cursors[field] =
                    options.iter().position(|o| o.0 == selected).unwrap_or(0);
            } else if let Some(old) = old {
                let previous = audio_options(old, field)
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
        if (self.active == View::Audio) != (view == View::Audio) {
            self.dispatch(Action::WatchOutputVolume(view == View::Audio), cx);
        }
        self.active = view;
        window.focus(&self.focus);
        if view == View::Account {
            self.account_cursor = 0;
        }
        cx.notify();
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

    // View switching is the only keyboard shortcut; everything else is clicked.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_pending || self.typing(window, cx) {
            return;
        }
        let key = event.keystroke.key.as_str();
        if let Some(index) = ["1", "2", "3", "4", "5"]
            .iter()
            .position(|digit| *digit == key)
        {
            self.change_view(View::ALL[index], window, cx);
        }
    }

    fn navigation(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let mut navigation = div()
            .flex()
            .gap_1()
            .p_2()
            .bg(rgb(PANEL))
            .border_color(rgb(BORDER));
        if compact {
            navigation = navigation.flex_row().border_b_1();
        } else {
            navigation = navigation
                .flex_col()
                .w(px(190.))
                .h_full()
                .border_r_1()
                .child(
                    div()
                        .px_3()
                        .py_4()
                        .text_lg()
                        .text_color(rgb(ACCENT))
                        .child("GoSipTea"),
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
                    .bg(rgb(if self.active == view { 0x302c44 } else { PANEL }))
                    .text_color(rgb(if self.active == view { ACCENT } else { MUTED }))
                    .hover(|style| style.bg(rgb(0x2a2f3a)))
                    .child(format!("{} {}", i + 1, view.name()))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.change_view(view, window, cx)),
                    )
            }))
            .into_any_element()
    }

    fn phone(&self, cx: &Context<Self>) -> AnyElement {
        let state = &self.snapshot.state;
        let mut pane = column()
            .child(heading("Phone"))
            .child(detail(
                "Registration",
                registration_text(&self.snapshot),
                registration_color(&self.snapshot),
            ))
            .child(detail(
                "Call",
                call_text(state.call_state),
                call_color(state.call_state),
            ));
        if state.call_state != CallState::Idle {
            pane = pane.child(detail("Peer", display_peer(&state.peer), TEXT));
        }
        if state.call_state == CallState::Active {
            pane = pane
                .child(detail(
                    "Duration",
                    duration(state.call_started_at, self.snapshot.now),
                    GOOD,
                ))
                .child(detail(
                    "Microphone",
                    if state.muted { "Muted" } else { "Live" },
                    if state.muted { WARNING } else { TEXT },
                ));
        }
        pane = pane
            .child(detail(
                "Do not disturb",
                if state.dnd { "On" } else { "Off" },
                if state.dnd { WARNING } else { TEXT },
            ))
            .child(div().mt_5().child(input_row("Dial", self.dial.clone())));
        let mut controls = div().flex().flex_wrap().gap_2().mt_3();
        match state.call_state {
            CallState::Incoming => {
                controls =
                    controls
                        .child(button("answer", "Answer", GOOD).on_click(
                            cx.listener(|this, _, _, cx| this.dispatch(Action::Answer, cx)),
                        ))
                        .child(button("reject", "Reject", ERROR).on_click(
                            cx.listener(|this, _, _, cx| this.dispatch(Action::Reject, cx)),
                        ))
            }
            CallState::Active | CallState::Outgoing => {
                controls = controls.child(
                    button("hangup", "Hang up", ERROR)
                        .on_click(cx.listener(|this, _, _, cx| this.dispatch(Action::Hangup, cx))),
                );
                if state.call_state == CallState::Active {
                    controls = controls.child(
                        button("mute", if state.muted { "Unmute" } else { "Mute" }, TEXT).on_click(
                            cx.listener(|this, _, _, cx| this.dispatch(Action::ToggleMute, cx)),
                        ),
                    );
                }
            }
            _ => {
                controls = controls.child(button("dial", "Dial", ACCENT).on_click(cx.listener(
                    |this, _, window, cx| {
                        if this.dial.read(cx).value().trim().is_empty() {
                            this.focus_input(Field::Dial, window, cx);
                        } else {
                            window.focus(&this.focus);
                            this.dial_number(cx);
                        }
                    },
                )))
            }
        }
        pane.child(
            controls.child(
                button("dnd", "DND", if state.dnd { WARNING } else { TEXT })
                    .on_click(cx.listener(|this, _, _, cx| this.dispatch(Action::ToggleDnd, cx))),
            ),
        )
        .into_any_element()
    }

    fn contacts_view(&self, cx: &Context<Self>) -> AnyElement {
        if self.adding_contact {
            return column()
                .child(heading("Add contact"))
                .child(input_row("Name", self.contact_name.clone()))
                .child(input_row("SIP address", self.contact_uri.clone()))
                .child(
                    div()
                        .mt_3()
                        .text_color(rgb(MUTED))
                        .child("Enter saves from the address field. Esc cancels."),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(button("save-contact", "Save contact", ACCENT).on_click(
                            cx.listener(|this, _, window, cx| this.save_contact(window, cx)),
                        ))
                        .child(
                            button("cancel-contact", "Cancel", TEXT).on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.adding_contact = false;
                                    window.focus(&this.focus);
                                    cx.notify();
                                },
                            )),
                        ),
                )
                .into_any_element();
        }
        let contacts = self.contacts(cx);
        let empty = contacts.is_empty();
        let selected = contacts.get(self.contact_cursor).cloned();
        let mut pane = column()
            .child(heading("Contacts"))
            .child(input_row("Search", self.search.clone()));
        let mut controls = div().flex().gap_2().mt_2().child(
            button("add-contact", "Add", ACCENT)
                .on_click(cx.listener(|this, _, window, cx| this.begin_contact(window, cx))),
        );
        if let Some(contact) = selected {
            let target = contact.uri.clone();
            controls = controls
                .child(button("dial-contact", "Dial", TEXT).on_click(cx.listener(
                    move |this, _, _, cx| this.dispatch(Action::Dial(target.clone()), cx),
                )))
                .child(
                    button("remove-contact", "Remove", ERROR).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.dispatch(Action::RemoveContact(contact.uri.clone()), cx)
                        },
                    )),
                );
        }
        pane = pane.child(controls);
        if empty {
            pane = pane.child(
                div()
                    .mt_4()
                    .text_color(rgb(MUTED))
                    .child("No matching contacts."),
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
                    let name = if contact.name.trim().is_empty() {
                        contact.uri.clone()
                    } else {
                        domain::clamp_text(&contact.name, MAX_CONTACT_NAME)
                    };
                    row(("contact", i), self.contact_cursor == i)
                        .child(div().w(px(210.)).flex_shrink_0().truncate().child(name))
                        .child(
                            div()
                                .text_color(rgb(MUTED))
                                .truncate()
                                .child(domain::clamp_text(&contact.uri, MAX_FIELD_LENGTH)),
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

    fn audio(&self, cx: &Context<Self>) -> AnyElement {
        let mut pane = column().child(heading("Audio"));
        if self.snapshot.ringtone_restart_required {
            pane = pane.child(
                div()
                    .text_color(rgb(WARNING))
                    .child("Restart GoSipTea to ring on the selected ringtone output."),
            );
        }
        pane = pane.child(self.volume_control(cx));
        for (field, title) in ["Output", "Input", "Ringtone"].into_iter().enumerate() {
            let selected = audio_selection(&self.snapshot, field);
            pane = pane.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(100.))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .rounded_sm()
                    .p_2()
                    .gap_1()
                    .child(
                        div()
                            .text_color(rgb(if field == self.audio_field {
                                ACCENT
                            } else {
                                TEXT
                            }))
                            .child(title),
                    )
                    .child(
                        div()
                            .id(("audio-list", field))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.audio_scroll[field])
                            .children(
                                audio_options(&self.snapshot, field)
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, (name, label))| {
                                        row(
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
        let level = self.snapshot.output_volume.map(volume_steps).unwrap_or(0);
        div().flex().flex_col().gap_1().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(150.)).flex_shrink_0().child("Output volume"))
                .child(
                    div()
                        .id("volume-bar")
                        .flex()
                        .gap(px(2.))
                        .children((1..=VOLUME_STEPS).map(|step| {
                            div()
                                .id(("volume-step", step as usize))
                                .debug_selector(|| format!("volume-step-{step}"))
                                .w(px(10.))
                                .h(px(14.))
                                .rounded_sm()
                                .cursor_pointer()
                                .bg(rgb(if step <= level { ACCENT } else { BORDER }))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    window.focus(&this.focus);
                                    this.set_volume_steps(step, cx);
                                }))
                        })),
                )
                .child(if let Some(volume) = self.snapshot.output_volume {
                    format!("{:>3}%", (volume * 100.0).round() as i32)
                } else {
                    "Unavailable".to_owned()
                }),
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
        let mut pane = column().child(heading("Account"));
        for (i, label) in ["Server", "User", "Domain", "Login", "Password"]
            .into_iter()
            .enumerate()
        {
            pane = pane.child(
                div()
                    .border_l_2()
                    .border_color(rgb(if self.account_cursor == i {
                        ACCENT
                    } else {
                        BACKGROUND
                    }))
                    .pl_2()
                    .child(input_row(label, self.account[i].clone())),
            );
        }
        if self.snapshot.account.has_password && self.account[4].read(cx).value().is_empty() {
            pane = pane.child(
                div()
                    .pl(px(152.))
                    .text_color(rgb(MUTED))
                    .text_sm()
                    .child("Saved password will be kept"),
            );
        }
        pane.child(
            div().mt_3().child(
                button(
                    "secure",
                    if self.account_secure {
                        "[x] TLS and SRTP"
                    } else {
                        "[ ] TLS and SRTP"
                    },
                    if self.account_cursor == 5 {
                        ACCENT
                    } else {
                        TEXT
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
                    "save-account",
                    "Save account",
                    if self.account_cursor == 6 {
                        ACCENT
                    } else {
                        TEXT
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| this.save_account(window, cx))),
            ),
        )
        .into_any_element()
    }

    fn history(&self, cx: &Context<Self>) -> AnyElement {
        let mut pane = column().child(heading("Call history"));
        if self.snapshot.history.is_empty() {
            pane = pane.child(div().text_color(rgb(MUTED)).child("No calls yet."));
        }
        if let Some(call) = self.snapshot.history.get(self.history_cursor) {
            let target = call.target.clone();
            pane = pane.child(button("redial", "Dial", ACCENT).on_click(
                cx.listener(move |this, _, _, cx| this.dispatch(Action::Dial(target.clone()), cx)),
            ));
        }
        pane.child(
            div()
                .id("history-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.history_scroll)
                .children(self.snapshot.history.iter().enumerate().map(|(i, call)| {
                    let peer = if call.peer.is_empty() {
                        &call.target
                    } else {
                        &call.peer
                    };
                    row(("history", i), self.history_cursor == i)
                        .child(if call.direction == CallDirection::Incoming {
                            "↓"
                        } else {
                            "↑"
                        })
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .child(domain::clamp_text(peer, domain::MAX_PEER_DISPLAY_LENGTH)),
                        )
                        .child(div().w(px(112.)).child(history_result(call)))
                        .child(
                            div()
                                .w(px(148.))
                                .text_color(rgb(MUTED))
                                .child(history_time(call.ended_at, self.snapshot.now)),
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
        let mut call = call_text(state.call_state).to_owned();
        if state.call_state == CallState::Active {
            call.push_str(&format!(
                " {}",
                duration(state.call_started_at, self.snapshot.now)
            ));
        }
        if !compact && state.call_state != CallState::Idle {
            call.push_str(&format!(" {}", display_peer(&state.peer)));
        }
        let mut error = if self.dispatch_error.is_empty() {
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
            .border_color(rgb(BORDER));
        if !error.is_empty() {
            footer = footer.child(div().px_4().py_2().text_color(rgb(ERROR)).child(format!(
                "Error: {}",
                domain::clamp_text(&error, domain::MAX_COMMAND_ERROR)
            )));
        }
        footer
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .bg(rgb(if incoming { WARNING } else { PANEL }))
                    .text_color(rgb(if incoming { PANEL } else { TEXT }))
                    .child(
                        div()
                            .text_color(rgb(if incoming {
                                PANEL
                            } else {
                                registration_color(&self.snapshot)
                            }))
                            .child(format!("● {}", short_registration(&self.snapshot))),
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
                    .when(incoming && self.active != View::Phone, |bar| {
                        bar.child(
                            div()
                                .id("incoming-phone")
                                .cursor_pointer()
                                .child("[1] answer")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.change_view(View::Phone, window, cx)
                                })),
                        )
                    })
                    .when(
                        state.call_state == CallState::Active && state.muted,
                        |bar| bar.child("Muted"),
                    )
                    .child(if state.dnd { "DND on" } else { "DND off" }),
            )
            .child(
                div()
                    .px_4()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("1-5 switch views"),
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
        let compact = window.viewport_size().width < px(760.);
        let body = match self.active {
            View::Phone => self.phone(cx),
            View::Contacts => self.contacts_view(cx),
            View::Audio => self.audio(cx),
            View::Account => self.account_view(cx),
            View::History => self.history(cx),
        };
        div()
            .id("workspace")
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
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
                        .bg(rgba(0x000000aa))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .p_6()
                                .bg(rgb(PANEL))
                                .border_1()
                                .border_color(rgb(WARNING))
                                .rounded_md()
                                .child("A call is in progress. Quit anyway?")
                                .child(
                                    div()
                                        .flex()
                                        .gap_3()
                                        .child(
                                            button("confirm-quit", "Quit", ERROR)
                                                .on_click(cx.listener(|_, _, _, cx| cx.quit())),
                                        )
                                        .child(button("cancel-quit", "Stay", TEXT).on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.quit_pending = false;
                                                cx.notify();
                                            }),
                                        )),
                                ),
                        ),
                )
            })
    }
}

fn column() -> Div {
    div().flex().flex_col().gap_3().size_full()
}
fn heading(title: &'static str) -> Div {
    div().mb_3().text_xl().text_color(rgb(ACCENT)).child(title)
}
fn detail(label: &'static str, value: impl Into<SharedString>, color: u32) -> Div {
    div()
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(140.))
                .flex_shrink_0()
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(rgb(color))
                .child(value.into()),
        )
}
fn input_row(label: &'static str, input: Entity<TextInput>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(140.))
                .flex_shrink_0()
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(
            div()
                .debug_selector(|| format!("input-{label}"))
                .flex_1()
                .min_w_0()
                .child(input),
        )
}
fn button(id: &'static str, label: impl Into<SharedString>, color: u32) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(|| id.to_owned())
        .px_3()
        .py_2()
        .rounded_sm()
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(PANEL))
        .text_color(rgb(color))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(0x303544)))
        .child(label.into())
}
fn row(id: (&'static str, usize), selected: bool) -> Stateful<Div> {
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
        .bg(rgb(if selected { 0x302c44 } else { BACKGROUND }))
        .text_color(rgb(if selected { ACCENT } else { TEXT }))
        .hover(|style| style.bg(rgb(0x303544)))
}
fn volume_steps(volume: f32) -> i32 {
    (volume.clamp(0.0, 1.0) * VOLUME_STEPS as f32).round() as i32
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
fn audio_options(snapshot: &Snapshot, field: usize) -> Vec<(String, String)> {
    let mut options = vec![(
        String::new(),
        if field == 2 {
            "Same as output"
        } else {
            "System default"
        }
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
            label.push_str(" (current system default)");
        }
        options.push((node.name.clone(), label));
    }
    let selected = audio_selection(snapshot, field);
    if !options.iter().any(|o| o.0 == selected) {
        options.push((
            selected.into(),
            format!(
                "{} (unavailable)",
                domain::clamp_text(selected, MAX_FIELD_LENGTH)
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
fn call_color(state: CallState) -> u32 {
    match state {
        CallState::Incoming | CallState::Outgoing => WARNING,
        CallState::Active => GOOD,
        _ => MUTED,
    }
}
fn display_peer(peer: &str) -> String {
    if peer.trim().is_empty() {
        "Unknown peer".into()
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
fn registration_text(snapshot: &Snapshot) -> String {
    if snapshot.state.registration_detail.is_empty() {
        short_registration(snapshot).into()
    } else {
        domain::clamp_text(
            &snapshot.state.registration_detail,
            domain::MAX_COMMAND_ERROR,
        )
    }
}
fn registration_color(snapshot: &Snapshot) -> u32 {
    if snapshot.state.registered || snapshot.state.registration == RegistrationState::Registered {
        GOOD
    } else if short_registration(snapshot) == "Failed" {
        ERROR
    } else {
        WARNING
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
fn history_result(call: &HistoryEntry) -> String {
    match call.outcome {
        CallOutcome::Connected => call
            .ended_at
            .map(|end| duration(call.connected_at, end))
            .unwrap_or_else(|| "00:00".into()),
        CallOutcome::Missed => "Missed".into(),
        CallOutcome::Rejected => "Rejected".into(),
        CallOutcome::RejectedDnd => "DND".into(),
        CallOutcome::RejectedBusy => "Busy".into(),
        CallOutcome::Canceled => "Canceled".into(),
        _ => "Not connected".into(),
    }
}
fn history_time(ended: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(ended) = ended else {
        return "Unknown".into();
    };
    let local = ended.with_timezone(&Local);
    let today = now.with_timezone(&Local).date_naive();
    if local.date_naive() == today {
        format!("Today {}", local.format("%H:%M"))
    } else if Some(local.date_naive()) == today.pred_opt() {
        format!("Yesterday {}", local.format("%H:%M"))
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
    fn navigation_matches_original_order() {
        assert_eq!(
            View::ALL.map(View::name),
            ["Phone", "Contacts", "Audio", "Account", "History"]
        );
    }
}
