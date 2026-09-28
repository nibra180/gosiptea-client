//! Offline screenshot fixture. Never starts baresip or reads the user's account.
use std::sync::{Arc, atomic::AtomicBool};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use gosiptea_client::{
    assets::Assets,
    domain::{CallDirection, CallOutcome, HistoryEntry},
    input,
    platform::{Backend, BackendEvent, Node, NodeKind},
    session::{Config, SessionHandle},
    settings::{Language, OmarchyTheme, Preferences, Theme},
    storage::{AccountCredentials, Store, StoredContact},
    ui::Workspace,
};
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

struct DemoBackend {
    incoming: bool,
}

impl Backend for DemoBackend {
    fn command(&mut self, command: &str, _: &str) -> Result<String> {
        Ok(if command == "reginfo" {
            "User Agents (1)\n<sip:204@example.com> OK".into()
        } else {
            String::new()
        })
    }
    fn poll_events(&mut self) -> Result<Vec<BackendEvent>> {
        if std::mem::take(&mut self.incoming) {
            Ok(vec![BackendEvent {
                kind: "CALL_INCOMING".into(),
                id: "demo-call".into(),
                peer_uri: "sip:201@example.com".into(),
                peer_display_name: "Mia Sommer".into(),
                ..Default::default()
            }])
        } else {
            Ok(vec![])
        }
    }
    fn discover_audio(&mut self) -> Result<Vec<Node>> {
        Ok(vec![
            Node {
                name: "demo.speakers".into(),
                description: "Desktop-Lautsprecher".into(),
                kind: NodeKind::Output,
                is_default: true,
            },
            Node {
                name: "demo.headset".into(),
                description: "USB-Headset".into(),
                kind: NodeKind::Output,
                is_default: false,
            },
            Node {
                name: "demo.microphone".into(),
                description: "USB-Mikrofon".into(),
                kind: NodeKind::Input,
                is_default: true,
            },
        ])
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
    fn output_volume(&mut self, _: &str) -> Result<Option<f32>> {
        Ok(Some(0.65))
    }
}

fn main() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::new(directory.path());
    store.ensure_config()?;
    store.save_account(&AccountCredentials {
        server: "pbx.example.com".into(),
        username: "204".into(),
        domain: "example.com".into(),
        login: "demo.team".into(),
        password: "demo-only".into(),
        secure: Some(true),
    })?;
    store.save_preferences(&Preferences {
        language: Language::German,
        theme: Theme::Dark,
    })?;
    let contacts = [
        ("Mia Sommer", "201"),
        ("Jonas Weber", "202"),
        ("Lena Fischer", "203"),
        ("Noah Berger", "205"),
        ("Empfang", "200"),
        ("Support", "210"),
    ];
    for (name, extension) in contacts {
        store.add_contact(&StoredContact {
            name: name.into(),
            uri: format!("sip:{extension}@example.com"),
            params: String::new(),
        })?;
    }
    let now: DateTime<Utc> = "2026-09-28T08:42:00Z".parse()?;
    let history = contacts
        .iter()
        .enumerate()
        .map(|(i, (name, extension))| {
            let ended = now - Duration::minutes(12 + i as i64 * 37);
            let missed = i == 2;
            HistoryEntry {
                direction: if i % 2 == 0 {
                    CallDirection::Incoming
                } else {
                    CallDirection::Outgoing
                },
                outcome: if missed {
                    CallOutcome::Missed
                } else {
                    CallOutcome::Connected
                },
                peer: (*name).into(),
                target: format!("sip:{extension}@example.com"),
                started_at: Some(ended - Duration::seconds(145 + i as i64 * 31)),
                connected_at: if missed {
                    None
                } else {
                    Some(ended - Duration::seconds(132 + i as i64 * 31))
                },
                ended_at: Some(ended),
            }
        })
        .collect::<Vec<_>>();
    store.save_history(&history)?;
    let incoming = std::env::args().any(|arg| arg == "--incoming");
    let session = Arc::new(SessionHandle::start_with_factory(
        Config::new(directory.path()),
        Box::new(move |_| Ok(Box::new(DemoBackend { incoming }))),
        Box::new(move || now),
    )?);
    let shutdown = session.clone();
    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, interrupted.clone())?;
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            input::init(cx);
            cx.set_global(OmarchyTheme::new(None));
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("GoSipTea Demo".into()),
                        ..Default::default()
                    }),
                    app_id: Some("gosiptea-demo".into()),
                    window_min_size: Some(size(px(540.), px(440.))),
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        let mut workspace = Workspace::new(session, interrupted, window, cx);
                        workspace.load_settings(store, cx);
                        workspace
                    })
                },
            )
            .expect("open demo window");
            cx.activate(true);
        });
    shutdown.request_shutdown();
    drop(directory);
    Ok(())
}
