use super::*;

#[path = "ui_render_bench.rs"]
mod render_bench;

use std::sync::Mutex;

use anyhow::Result;
use gpui::{EntityInputHandler, TestAppContext, VisualTestContext};
use tempfile::TempDir;

use crate::{
    platform::{Backend, BackendEvent, Node},
    session::Config,
    storage::Store,
};

#[derive(Default)]
struct BackendState {
    commands: Vec<(String, String)>,
    events: Vec<BackendEvent>,
    nodes: Vec<Node>,
    starts: usize,
    output_volume: Option<f32>,
    volume_sets: Vec<f32>,
    volume_gate: Option<Arc<VolumeGate>>,
}

/// Each set_output_volume call waits for one permit, so tests control publish order.
#[derive(Default)]
struct VolumeGate {
    entered: std::sync::atomic::AtomicUsize,
    permits: std::sync::atomic::AtomicUsize,
}

struct FakeBackend(Arc<Mutex<BackendState>>);

impl Backend for FakeBackend {
    fn command(&mut self, command: &str, params: &str) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .commands
            .push((command.into(), params.into()));
        Ok(if command == "reginfo" {
            "User Agents (1)\n<sip:100@example.com> OK".into()
        } else {
            String::new()
        })
    }

    fn poll_events(&mut self) -> Result<Vec<BackendEvent>> {
        Ok(std::mem::take(&mut self.0.lock().unwrap().events))
    }

    fn discover_audio(&mut self) -> Result<Vec<Node>> {
        Ok(self.0.lock().unwrap().nodes.clone())
    }

    fn notify(&mut self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn pause_media(&mut self) -> Result<()> {
        Ok(())
    }
    fn focus(&mut self) -> Result<()> {
        Ok(())
    }
    fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
    fn output_volume(&mut self, _output: &str) -> Result<Option<f32>> {
        Ok(self.0.lock().unwrap().output_volume)
    }
    fn set_output_volume(&mut self, _output: &str, volume: f32) -> Result<()> {
        let gate = self.0.lock().unwrap().volume_gate.clone();
        if let Some(gate) = gate {
            let call = gate.entered.fetch_add(1, Ordering::SeqCst) + 1;
            while gate.permits.load(Ordering::SeqCst) < call {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let mut state = self.0.lock().unwrap();
        state.volume_sets.push(volume);
        state.output_volume = Some(volume);
        Ok(())
    }
}

struct Fixture {
    session: Arc<SessionHandle>,
    backend: Arc<Mutex<BackendState>>,
    directory: Arc<TempDir>,
}

impl Fixture {
    fn new() -> Self {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        Store::new(directory.path()).ensure_config().unwrap();
        Store::new(directory.path())
            .save_account(&AccountCredentials {
                server: "pbx.example.com".into(),
                username: "100".into(),
                domain: "example.com".into(),
                login: "100".into(),
                password: "saved-secret".into(),
                secure: Some(true),
            })
            .unwrap();
        let backend = Arc::new(Mutex::new(BackendState {
            output_volume: Some(1.0),
            nodes: vec![
                Node {
                    name: "sink.speakers".into(),
                    description: "Speakers".into(),
                    kind: NodeKind::Output,
                    is_default: true,
                },
                Node {
                    name: "sink.headset".into(),
                    description: "Headset".into(),
                    kind: NodeKind::Output,
                    is_default: false,
                },
                Node {
                    name: "source.mic".into(),
                    description: "Microphone".into(),
                    kind: NodeKind::Input,
                    is_default: true,
                },
            ],
            ..BackendState::default()
        }));
        let state = backend.clone();
        // Workspace can outlive this fixture during GPUI teardown.
        let keep_directory = directory.clone();
        let session = SessionHandle::start_with_factory(
            Config::new(directory.path()),
            Box::new(move |_| {
                let _ = &keep_directory;
                state.lock().unwrap().starts += 1;
                Ok(Box::new(FakeBackend(state.clone())))
            }),
            Box::new(|| DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
        )
        .unwrap();
        Self {
            session: Arc::new(session),
            backend,
            directory,
        }
    }

    fn open<'a>(
        &self,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        cx.update(crate::input::init);
        let session = self.session.clone();
        let (workspace, cx) = cx.add_window_view(move |window, cx| {
            Workspace::new(session, Arc::new(AtomicBool::new(false)), window, cx)
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (workspace, cx)
    }

    fn settle(&self, workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        // Wait outside any GPUI update. The second request fences poll_events even
        // when the worker was already waiting for a request when an event arrived.
        self.session.dispatch_wait(Action::RefreshAudio).unwrap();
        self.session.dispatch_wait(Action::RefreshAudio).unwrap();
        workspace.update(cx, |this, cx| this.refresh(cx));
        cx.run_until_parked();
    }

    fn event(&self, kind: &str, id: &str, peer: &str) {
        self.backend.lock().unwrap().events.push(BackendEvent {
            kind: kind.into(),
            id: id.into(),
            peer_uri: peer.into(),
            ..BackendEvent::default()
        });
    }

    fn commands(&self, command: &str) -> Vec<String> {
        self.backend
            .lock()
            .unwrap()
            .commands
            .iter()
            .filter(|(name, _)| name == command)
            .map(|(_, params)| params.clone())
            .collect()
    }
}

/// Clicks the element tagged with `debug_selector` in the last rendered frame.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not rendered"));
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    cx.run_until_parked();
}

/// Opens a view through the navigation, the only way to switch views.
fn show(cx: &mut VisualTestContext, view: View) {
    let index = View::ALL.iter().position(|v| *v == view).unwrap();
    click(
        cx,
        ["view-0", "view-1", "view-2", "view-3", "view-4", "view-5"][index],
    );
}

#[gpui::test]
fn clicks_switch_views_and_typing_on_the_phone_view_goes_to_the_dial_field(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    let dial_state = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            let this = workspace.read(cx);
            (
                this.active,
                this.dial.read(cx).value().to_owned(),
                this.dial.focus_handle(cx).is_focused(window),
            )
        })
    };
    assert_eq!(dial_state(cx), (View::Phone, String::new(), true));
    cx.simulate_input("23");
    assert_eq!(dial_state(cx), (View::Phone, "23".into(), true));
    for view in [View::Contacts, View::Audio, View::Account, View::History] {
        show(cx, view);
        cx.simulate_keystrokes("1 2 3 4 5");
        cx.update(|window, cx| {
            let this = workspace.read(cx);
            assert_eq!(this.active, view);
            assert!(!this.typing(window, cx));
        });
    }
    show(cx, View::Phone);
    assert_eq!(dial_state(cx), (View::Phone, "23".into(), true));
    // After Esc the next key still lands in the dial field.
    cx.simulate_keystrokes("escape");
    assert!(!dial_state(cx).2);
    cx.simulate_keystrokes("4");
    assert_eq!(dial_state(cx), (View::Phone, "234".into(), true));
    click(cx, "dialpad-5");
    click(cx, "dialpad-#");
    click(cx, "dial-backspace");
    assert_eq!(dial_state(cx), (View::Phone, "2345".into(), true));
    assert!(fixture.commands("dial").is_empty());
    click(cx, "dial");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("dial"), ["2345"]);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.snapshot.state.call_state, CallState::Outgoing)
    });
    cx.simulate_keystrokes("7");
    assert_eq!(dial_state(cx), (View::Phone, "2345".into(), false));
}

#[gpui::test]
fn ctrl_digits_and_ctrl_tab_switch_views_even_from_text_fields(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    let active = |cx: &mut VisualTestContext| workspace.read_with(cx, |this, _| this.active);
    // The dial field has focus here and must not receive the digit.
    cx.simulate_keystrokes("ctrl-2");
    assert_eq!(active(cx), View::Contacts);
    click(cx, "input-Search");
    cx.simulate_keystrokes("ctrl-tab");
    assert_eq!(active(cx), View::Audio);
    cx.simulate_keystrokes("ctrl-shift-tab ctrl-shift-tab ctrl-shift-tab");
    assert_eq!(active(cx), View::Settings);
    cx.simulate_keystrokes("ctrl-tab");
    assert_eq!(active(cx), View::Phone);
    cx.simulate_keystrokes("ctrl-6");
    assert_eq!(active(cx), View::Settings);
    cx.simulate_keystrokes("ctrl-1 ctrl-7 ctrl-0 ctrl-shift-3");
    cx.update(|window, cx| {
        let this = workspace.read(cx);
        assert_eq!(this.active, View::Phone);
        assert!(this.dial.read(cx).value().is_empty());
        assert!(this.search.read(cx).value().is_empty());
        assert!(this.dial.focus_handle(cx).is_focused(window));
    });
    cx.simulate_keystrokes("ctrl-5");
    assert_eq!(active(cx), View::History);
    fixture.event("CALL_INCOMING", "incoming-1", "sip:alice@example.com");
    fixture.settle(&workspace, cx);
    assert!(!cx.simulate_close());
    cx.simulate_keystrokes("ctrl-3 ctrl-tab");
    assert_eq!(active(cx), View::Phone);
}

#[gpui::test]
fn settings_switch_live_and_persist_without_losing_input(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let store = Store::new(fixture.directory.path());
    let (workspace, cx) = fixture.open(cx);
    workspace.update(cx, |this, cx| this.load_settings(store.clone(), cx));
    cx.simulate_input("123");
    show(cx, View::Settings);
    click(cx, "language-de");
    click(cx, "theme-light");
    workspace.read_with(cx, |this, cx| {
        assert_eq!(this.active, View::Settings);
        assert_eq!(
            preferences(cx),
            Preferences {
                language: Language::German,
                theme: Theme::Light
            }
        );
        assert_eq!(tr(cx, "Settings"), "Einstellungen");
        assert_eq!(palette(cx).background, 0xf8efe7);
        assert_eq!(this.dial.read(cx).value(), "123");
        assert!(this.settings_error.is_empty());
    });
    assert_eq!(
        store.load_preferences().unwrap(),
        Preferences {
            language: Language::German,
            theme: Theme::Light
        }
    );
    for view in View::ALL {
        show(cx, view);
    }
    click(cx, "language-en");
    click(cx, "theme-dark");
    workspace.read_with(cx, |_, cx| {
        assert_eq!(preferences(cx), Preferences::default());
        assert_eq!(tr(cx, "Settings"), "Settings");
        assert_eq!(palette(cx).background, 0x2d2a2e);
    });
    assert_eq!(store.load_preferences().unwrap(), Preferences::default());
    store
        .save_preferences(&Preferences {
            language: Language::German,
            theme: Theme::Light,
        })
        .unwrap();
    workspace.update(cx, |this, cx| this.load_settings(store, cx));
    cx.run_until_parked();
    workspace.read_with(cx, |_, cx| {
        assert_eq!(preferences(cx).language, Language::German)
    });
}

#[gpui::test]
fn settings_remain_reachable_in_compact_german_navigation(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (_, cx) = fixture.open(cx);
    cx.simulate_resize(gpui::size(px(540.), px(440.)));
    show(cx, View::Settings);
    let english_width = cx.debug_bounds("view-5").unwrap().size.width;
    click(cx, "language-de");
    assert!(cx.debug_bounds("view-5").unwrap().size.width > english_width);
    for selector in ["view-0", "view-1", "view-2", "view-3", "view-4", "view-5"] {
        let bounds = cx.debug_bounds(selector).unwrap();
        assert!(bounds.left() >= px(0.) && bounds.right() <= px(540.));
        assert!(bounds.top() >= px(0.) && bounds.bottom() <= px(440.));
    }
    click(cx, "theme-light");
    for view in View::ALL {
        show(cx, view);
    }
    click(cx, "theme-dark");
}

#[gpui::test]
fn settings_errors_are_visible_and_changes_still_apply(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let store = Store::new(fixture.directory.path());
    let path = store.paths().dir.join("gosiptea-settings.json");
    std::fs::write(&path, "invalid json").unwrap();
    let (workspace, cx) = fixture.open(cx);
    workspace.update(cx, |this, cx| this.load_settings(store, cx));
    workspace.read_with(cx, |this, _| assert!(!this.settings_error.is_empty()));
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    show(cx, View::Settings);
    click(cx, "theme-light");
    workspace.read_with(cx, |this, cx| {
        assert_eq!(preferences(cx).theme, Theme::Light);
        assert!(!this.settings_error.is_empty());
    });
    std::fs::remove_dir(&path).unwrap();
    click(cx, "language-de");
    workspace.read_with(cx, |this, _| assert!(this.settings_error.is_empty()));
}

#[gpui::test]
fn empty_dial_button_focuses_the_field_and_enter_dials(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    cx.simulate_keystrokes("escape");
    click(cx, "dial");
    fixture.settle(&workspace, cx);
    assert!(fixture.commands("dial").is_empty());
    cx.update(|window, cx| {
        assert!(workspace.read(cx).dial.focus_handle(cx).is_focused(window));
    });
    cx.simulate_input("sip:bob@example.com");
    cx.simulate_keystrokes("enter");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("dial"), ["sip:bob@example.com"]);
}

#[gpui::test]
fn keys_outside_text_fields_trigger_nothing(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    fixture.event("CALL_INCOMING", "incoming-1", "sip:alice@example.com");
    fixture.settle(&workspace, cx);
    let commands = fixture.backend.lock().unwrap().commands.len();
    let unchanged = |cx: &mut VisualTestContext, view: View| {
        fixture.settle(&workspace, cx);
        assert_eq!(fixture.backend.lock().unwrap().commands.len(), commands);
        cx.update(|window, cx| {
            let this = workspace.read(cx);
            assert_eq!(this.active, view);
            assert!(!this.quit_pending && !this.adding_contact);
            assert!(!this.typing(window, cx));
            assert!(!this.snapshot.state.dnd);
            assert_eq!(this.snapshot.state.call_state, CallState::Incoming);
            assert!(this.dial.read(cx).value().is_empty());
        });
    };
    cx.simulate_keystrokes("1 i / enter a r h m n q ctrl-c escape");
    unchanged(cx, View::Phone);
    show(cx, View::Contacts);
    cx.simulate_keystrokes("2 a s / d x down enter delete");
    unchanged(cx, View::Contacts);
    show(cx, View::Audio);
    cx.simulate_keystrokes("3 tab down enter space left right - + =");
    unchanged(cx, View::Audio);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.audio_field, 0);
        assert_eq!(this.audio_cursors, [0, 0, 0]);
    });
    assert!(fixture.backend.lock().unwrap().volume_sets.is_empty());
    show(cx, View::Account);
    cx.simulate_keystrokes("4 down tab enter space");
    unchanged(cx, View::Account);
    workspace.read_with(cx, |this, _| assert_eq!(this.account_cursor, 0));
    show(cx, View::History);
    cx.simulate_keystrokes("5 down enter d");
    unchanged(cx, View::History);
    assert!(!cx.simulate_close());
    cx.simulate_keystrokes("y enter 1");
    workspace.read_with(cx, |this, _| {
        assert!(this.quit_pending);
        assert_eq!(this.active, View::History);
    });
}

#[gpui::test]
fn contact_add_normalizes_redisplays_searches_and_removes(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    show(cx, View::Contacts);
    click(cx, "add-contact");
    cx.simulate_input("  Alice  ");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("  12345  ");
    workspace.read_with(cx, |this, cx| {
        assert_eq!(this.active, View::Contacts);
        assert!(this.adding_contact);
        assert_eq!(this.contact_uri.read(cx).value(), "  12345  ");
    });
    cx.simulate_keystrokes("enter");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, cx| {
        assert!(!this.adding_contact);
        assert_eq!(
            this.contacts(cx),
            [Contact {
                name: "Alice".into(),
                uri: "sip:12345@example.com".into()
            }]
        );
    });
    click(cx, "add-contact");
    cx.simulate_input("Bob");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("bob@elsewhere.example");
    click(cx, "save-contact");
    fixture.settle(&workspace, cx);
    click(cx, "contact-1");
    workspace.read_with(cx, |this, _| assert_eq!(this.contact_cursor, 1));
    click(cx, "input-Search");
    cx.simulate_input("ALICE");
    workspace.read_with(cx, |this, cx| {
        assert_eq!(this.contact_cursor, 0);
        assert_eq!(this.contacts(cx).len(), 1);
        assert_eq!(this.contacts(cx)[0].name, "Alice");
    });
    cx.simulate_keystrokes("escape");
    show(cx, View::History);
    show(cx, View::Contacts);
    workspace.read_with(cx, |this, cx| {
        assert_eq!(this.search.read(cx).value(), "ALICE");
        assert_eq!(this.contacts(cx)[0].uri, "sip:12345@example.com");
    });
    click(cx, "remove-contact");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, cx| {
        assert!(this.contacts(cx).is_empty());
        assert_eq!(this.contact_cursor, 0);
        assert_eq!(this.snapshot.contacts.len(), 1);
        assert_eq!(this.snapshot.contacts[0].name, "Bob");
    });
    click(cx, "input-Search");
    cx.simulate_keystrokes("ctrl-a backspace escape");
    workspace.read_with(cx, |this, cx| {
        assert_eq!(this.contacts(cx)[0].uri, "sip:bob@elsewhere.example")
    });
    assert_eq!(
        Store::new(fixture.directory.path())
            .load_contacts()
            .unwrap()
            .len(),
        1
    );
}

#[gpui::test]
fn contact_cancel_and_empty_address_do_not_save(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    show(cx, View::Contacts);
    click(cx, "add-contact");
    cx.simulate_input("Unfinished");
    cx.simulate_keystrokes("tab enter");
    workspace.read_with(cx, |this, _| assert!(this.adding_contact));
    click(cx, "save-contact");
    workspace.read_with(cx, |this, _| assert!(this.adding_contact));
    click(cx, "cancel-contact");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, _| {
        assert!(!this.adding_contact);
        assert!(this.snapshot.contacts.is_empty());
    });
    click(cx, "add-contact");
    workspace.read_with(cx, |this, cx| {
        assert!(this.contact_name.read(cx).value().is_empty());
        assert!(this.contact_uri.read(cx).value().is_empty());
    });
    cx.simulate_keystrokes("escape");
    workspace.read_with(cx, |this, _| assert!(!this.adding_contact));
}

#[gpui::test]
fn account_programmatic_sync_does_not_mark_the_form_dirty(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    workspace.read_with(cx, |this, _| assert!(!this.account_dirty));
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, _| assert!(!this.account_dirty));
}

#[gpui::test]
fn account_edits_survive_refresh_and_navigation_and_blank_password_is_preserved(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    show(cx, View::Account);
    click(cx, "input-Server");
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input("new.example.com");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, cx| {
        assert!(this.account_dirty);
        assert_eq!(this.account[0].read(cx).value(), "new.example.com");
        assert!(this.account[4].read(cx).value().is_empty());
        assert!(this.snapshot.account.has_password);
    });
    cx.simulate_keystrokes("escape");
    show(cx, View::Contacts);
    show(cx, View::Account);
    workspace.read_with(cx, |this, cx| {
        assert_eq!(this.account[0].read(cx).value(), "new.example.com")
    });
    click(cx, "save-account");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, cx| {
        assert_eq!(this.snapshot.account.server, "new.example.com");
        assert!(this.snapshot.account.has_password);
        assert!(this.account[4].read(cx).value().is_empty());
    });
    assert_eq!(fixture.backend.lock().unwrap().starts, 2);
    let saved = std::fs::read_to_string(fixture.directory.path().join("accounts")).unwrap();
    assert!(saved.contains("auth_pass=saved-secret"));
}

#[gpui::test]
fn account_password_is_masked_for_ime_and_cleared_after_save(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    show(cx, View::Account);
    click(cx, "input-Password");
    cx.simulate_input("replacement-secret");
    let password = workspace.read_with(cx, |this, _| this.account[4].clone());
    cx.update(|window, cx| {
        password.update(cx, |input, cx| {
            assert_eq!(input.value(), "replacement-secret");
            let mut actual_range = None;
            let exposed = input
                .text_for_range(0..18, &mut actual_range, window, cx)
                .unwrap();
            assert!(!exposed.contains("replacement-secret"));
            assert!(exposed.chars().all(|ch| ch == '•'));
        });
    });
    click(cx, "save-account");
    workspace.read_with(cx, |this, cx| {
        assert!(this.account[4].read(cx).value().is_empty())
    });
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, cx| {
        assert!(this.snapshot.account.has_password);
        assert!(this.account[4].read(cx).value().is_empty());
        assert!(!format!("{:?}", this.snapshot).contains("replacement-secret"));
        assert!(
            !this.account_dirty,
            "clearing a saved password is not a user edit"
        );
    });
    let saved = std::fs::read_to_string(fixture.directory.path().join("accounts")).unwrap();
    assert!(saved.contains("auth_pass=replacement-secret"));
}

#[gpui::test]
fn incoming_call_status_survives_every_view_and_quit_can_be_cancelled(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    fixture.event("CALL_INCOMING", "incoming-1", "sip:alice@example.com");
    fixture.settle(&workspace, cx);
    for view in View::ALL {
        show(cx, view);
        workspace.read_with(cx, |this, _| {
            assert_eq!(this.active, view);
            assert_eq!(this.snapshot.state.call_state, CallState::Incoming);
            assert!(this.snapshot.state.registered);
            assert!(this.snapshot.state.peer.contains("alice"));
        });
        assert!(!cx.simulate_close());
        workspace.read_with(cx, |this, _| assert!(this.quit_pending));
        // The quit dialog covers the navigation.
        click(cx, "view-0");
        workspace.read_with(cx, |this, _| assert_eq!(this.active, view));
        click(cx, "cancel-quit");
        workspace.read_with(cx, |this, _| assert!(!this.quit_pending));
    }
    show(cx, View::Phone);
    click(cx, "answer");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("accept").len(), 1);
    fixture.event("CALL_ESTABLISHED", "incoming-1", "sip:alice@example.com");
    fixture.settle(&workspace, cx);
    click(cx, "mute");
    click(cx, "dnd");
    fixture.settle(&workspace, cx);
    for view in [View::Contacts, View::Audio, View::Account, View::History] {
        show(cx, view);
        workspace.read_with(cx, |this, _| {
            assert_eq!(this.snapshot.state.call_state, CallState::Active);
            assert!(this.snapshot.state.muted);
            assert!(this.snapshot.state.dnd);
        });
    }
}

#[gpui::test]
fn incoming_call_takes_over_the_screen_and_decline_rejects(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    show(cx, View::Contacts);
    click(cx, "input-Search");
    fixture.event("CALL_INCOMING", "incoming-1", "sip:alice@example.com");
    fixture.settle(&workspace, cx);
    cx.update(|window, cx| {
        let this = workspace.read(cx);
        assert_eq!(this.active, View::Phone);
        assert!(!this.search.focus_handle(cx).is_focused(window));
    });
    assert!(cx.debug_bounds("answer").is_some());
    assert!(cx.debug_bounds("mute").is_none());
    show(cx, View::Audio);
    click(cx, "call-banner");
    workspace.read_with(cx, |this, _| assert_eq!(this.active, View::Phone));
    click(cx, "reject");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("hangup"), ["scode=603 reason=Decline"]);
}

#[gpui::test]
fn active_call_controls_hold_and_send_keypad_digits(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    fixture.event("CALL_INCOMING", "call-1", "sip:alice@example.com");
    fixture.settle(&workspace, cx);
    click(cx, "answer");
    fixture.event("CALL_ESTABLISHED", "call-1", "sip:alice@example.com");
    fixture.settle(&workspace, cx);
    click(cx, "keypad");
    for key in ["dtmf-5", "dtmf-#", "dtmf-0"] {
        click(cx, key);
    }
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("sndcode"), ["5", "#", "0"]);
    workspace.read_with(cx, |this, _| assert_eq!(this.dtmf_digits, "5#0"));
    // Typed digits count like clicks; other keys and a hidden keypad send nothing.
    cx.simulate_keystrokes("1 a * space");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("sndcode"), ["5", "#", "0", "1", "*"]);
    workspace.read_with(cx, |this, _| assert_eq!(this.dtmf_digits, "5#01*"));
    click(cx, "keypad-hide");
    cx.simulate_keystrokes("2");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("sndcode").len(), 5);
    click(cx, "hold");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("hold").len(), 1);
    workspace.read_with(cx, |this, _| assert!(this.snapshot.state.on_hold));
    // Keypad and mute are disabled while the call is held.
    click(cx, "keypad");
    click(cx, "mute");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, _| {
        assert!(!this.keypad_open);
        assert!(!this.snapshot.state.muted);
    });
    click(cx, "hold");
    fixture.settle(&workspace, cx);
    click(cx, "mute");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("resume").len(), 1);
    workspace.read_with(cx, |this, _| assert!(this.snapshot.state.muted));
    click(cx, "keypad");
    click(cx, "hangup");
    fixture.event("CALL_CLOSED", "call-1", "sip:alice@example.com");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("hangup").len(), 1);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.snapshot.state.call_state, CallState::Idle);
        assert!(!this.keypad_open);
        assert!(this.dtmf_digits.is_empty());
    });
    // GPUI keeps debug bounds of earlier frames, so prove the idle view is back
    // through the dial button, which focuses the empty dial field.
    click(cx, "dial");
    cx.update(|window, cx| {
        assert!(workspace.read(cx).dial.focus_handle(cx).is_focused(window));
    });
}

#[gpui::test]
fn outgoing_call_blocks_window_close_until_backend_reports_closed(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    click(cx, "dial");
    cx.simulate_input("sip:bob@example.com");
    cx.simulate_keystrokes("enter");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("dial"), ["sip:bob@example.com"]);
    assert!(!cx.simulate_close());
    workspace.read_with(cx, |this, _| assert!(this.quit_pending));
    cx.simulate_keystrokes("escape n");
    workspace.read_with(cx, |this, _| assert!(this.quit_pending));
    click(cx, "cancel-quit");
    fixture.event("CALL_CLOSED", "outgoing-1", "sip:bob@example.com");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.snapshot.state.call_state, CallState::Idle)
    });
    assert!(cx.simulate_close());
}

#[gpui::test]
fn audio_click_selection_applies_each_list_and_survives_redisplay(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    show(cx, View::Audio);
    click(cx, "audio-option-2");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("auplay"), ["pipewire,sink.headset"]);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.audio_field, 0);
        assert_eq!(this.snapshot.audio_config.output, "sink.headset");
    });
    click(cx, "audio-option-10001");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("ausrc"), ["pipewire,source.mic"]);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.audio_field, 1);
        assert_eq!(this.snapshot.audio_config.output, "sink.headset");
    });
    click(cx, "audio-option-20001");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.audio_field, 2);
        assert_eq!(this.snapshot.audio_config.output, "sink.headset");
        assert_eq!(this.snapshot.audio_config.input, "source.mic");
        assert_eq!(this.snapshot.audio_config.alert, "sink.speakers");
        assert!(this.snapshot.ringtone_restart_required);
        assert_eq!(this.audio_cursors, [2, 1, 1]);
    });
    show(cx, View::Phone);
    show(cx, View::Audio);
    workspace.read_with(cx, |this, _| assert_eq!(this.audio_cursors, [2, 1, 1]));
    fixture
        .backend
        .lock()
        .unwrap()
        .nodes
        .retain(|node| node.name != "sink.headset");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, _| assert_eq!(this.audio_cursors, [2, 1, 1]));
    click(cx, "audio-option-2");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.snapshot.audio_config.output, "sink.headset")
    });
    assert_eq!(fixture.commands("auplay"), ["pipewire,sink.headset"]);
}

#[gpui::test]
fn history_redials_selected_backend_call_and_opens_the_call_screen(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    for (id, peer) in [
        ("first", "sip:alice@example.com"),
        ("second", "sip:bob@example.com"),
    ] {
        fixture.event("CALL_INCOMING", id, peer);
        fixture.settle(&workspace, cx);
        fixture.event("CALL_CLOSED", id, peer);
        fixture.settle(&workspace, cx);
    }
    show(cx, View::History);
    click(cx, "history-1");
    let selected = workspace.read_with(cx, |this, _| {
        assert_eq!(this.snapshot.history.len(), 2);
        assert_eq!(this.history_cursor, 1);
        this.snapshot.history[1].target.clone()
    });
    fixture.settle(&workspace, cx);
    show(cx, View::Contacts);
    show(cx, View::History);
    click(cx, "redial");
    fixture.settle(&workspace, cx);
    assert_eq!(fixture.commands("dial"), [selected]);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.active, View::Phone);
        assert_eq!(this.history_cursor, 1);
        assert_eq!(this.snapshot.state.call_state, CallState::Outgoing);
    });
}

#[gpui::test]
fn audio_volume_clicks_change_the_output_without_a_call(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (workspace, cx) = fixture.open(cx);
    show(cx, View::Audio);
    for step in ["volume-step-19", "volume-step-18", "volume-step-17"] {
        click(cx, step);
    }
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.snapshot.output_volume, Some(0.85));
    });
    assert_eq!(
        fixture.backend.lock().unwrap().volume_sets,
        [0.95, 0.9, 0.85]
    );
    click(cx, "volume-step-18");
    fixture.settle(&workspace, cx);
    assert_eq!(
        fixture.backend.lock().unwrap().volume_sets.last(),
        Some(&0.9)
    );
}

#[gpui::test]
fn audio_volume_is_unavailable_without_an_output_device(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    fixture.backend.lock().unwrap().output_volume = None;
    let (workspace, cx) = fixture.open(cx);
    fixture.settle(&workspace, cx);
    show(cx, View::Audio);
    click(cx, "volume-step-10");
    fixture.settle(&workspace, cx);
    workspace.read_with(cx, |this, _| assert_eq!(this.snapshot.output_volume, None));
    assert!(fixture.backend.lock().unwrap().volume_sets.is_empty());
}

#[gpui::test]
fn audio_volume_ignores_snapshots_published_before_the_latest_click(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let gate = Arc::new(VolumeGate::default());
    {
        let mut backend = fixture.backend.lock().unwrap();
        backend.output_volume = Some(1.0);
        backend.volume_gate = Some(gate.clone());
    }
    let (workspace, cx) = fixture.open(cx);
    let wait_entered = |count: usize| {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while gate.entered.load(Ordering::SeqCst) < count {
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not reach volume call"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    show(cx, View::Audio);
    click(cx, "volume-step-19");
    wait_entered(1);
    click(cx, "volume-step-18");
    // The worker now publishes 95 % and blocks on the queued 90 % request.
    gate.permits.store(1, Ordering::SeqCst);
    wait_entered(2);
    assert_eq!(fixture.session.snapshot().output_volume, Some(0.95));
    workspace.update(cx, |this, cx| this.refresh(cx));
    workspace.read_with(cx, |this, _| {
        assert_eq!(this.snapshot.output_volume, Some(0.9))
    });
    click(cx, "volume-step-17");
    gate.permits.store(usize::MAX, Ordering::SeqCst);
    fixture.settle(&workspace, cx);
    assert_eq!(
        fixture.backend.lock().unwrap().volume_sets,
        [0.95, 0.9, 0.85]
    );
}
