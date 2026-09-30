use super::*;
use gpui::TestAppContext;
use sippy::{
    platform::{Backend, BackendEvent, Node},
    session::Action,
};

#[test]
fn trace_requires_log_only_when_enabled() {
    assert!(!Options::try_parse_from(["sippy"]).unwrap().sip_trace);
    assert!(Options::try_parse_from(["sippy", "--sip-trace"]).is_err());
    assert!(Options::try_parse_from(["sippy", "--sip-trace=false"]).is_ok());
    let enabled =
        Options::try_parse_from(["sippy", "--sip-trace", "--baresip-log", "test.log"]).unwrap();
    assert!(enabled.sip_trace);
}

#[derive(Default)]
struct Calls {
    commands: Vec<String>,
    shutdowns: usize,
}
struct FakeBackend(Arc<Mutex<Calls>>);
impl Backend for FakeBackend {
    fn command(&mut self, name: &str, _: &str) -> Result<String> {
        self.0.lock().unwrap().commands.push(name.to_owned());
        Ok(if name == "reginfo" {
            "User Agents (1)\n<sip:test@example.invalid> OK".into()
        } else {
            String::new()
        })
    }
    fn poll_events(&mut self) -> Result<Vec<BackendEvent>> {
        Ok(vec![])
    }
    fn discover_audio(&mut self) -> Result<Vec<Node>> {
        Ok(vec![])
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
        self.0.lock().unwrap().shutdowns += 1;
        Ok(())
    }
}

#[test]
fn termination_signal_stops_sip_without_a_running_gui_event_loop() {
    let directory = tempfile::tempdir().unwrap();
    storage::Store::new(directory.path())
        .ensure_config()
        .unwrap();
    let calls = Arc::new(Mutex::new(Calls::default()));
    let backend_calls = calls.clone();
    let session = Arc::new(
        SessionHandle::start_with_factory(
            Config::new(directory.path()),
            Box::new(move |_| Ok(Box::new(FakeBackend(backend_calls.clone())))),
            Box::new(chrono::Utc::now),
        )
        .unwrap(),
    );
    let signal = Arc::new(AtomicBool::new(false));
    let relay = InterruptRelay::new(session.clone(), signal.clone()).unwrap();
    signal.store(true, Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while session.snapshot().running {
        assert!(std::time::Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    drop(relay);
    assert_eq!(calls.lock().unwrap().shutdowns, 1);
}

#[gpui::test]
fn panic_after_workspace_creation_stops_session_even_if_gpui_keeps_an_arc(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    storage::Store::new(directory.path())
        .ensure_config()
        .unwrap();
    let calls = Arc::new(Mutex::new(Calls::default()));
    let backend_calls = calls.clone();
    let session = Arc::new(
        SessionHandle::start_with_factory(
            Config::new(directory.path()),
            Box::new(move |_| Ok(Box::new(FakeBackend(backend_calls.clone())))),
            Box::new(chrono::Utc::now),
        )
        .unwrap(),
    );
    session.dispatch_wait(Action::Dial("100".into())).unwrap();
    let retained = session.clone();
    let panic = catch_unwind(AssertUnwindSafe(|| {
        run_guarded(session, |ui_session| {
            cx.update(input::init);
            let (_, window) = cx.add_window_view(move |window, cx| {
                Workspace::new(ui_session, Arc::new(AtomicBool::new(false)), window, cx)
            });
            window.run_until_parked();
            panic!("deliberate UI failure after constructing the workspace");
        })
        .unwrap();
    }));
    assert!(panic.is_err());
    assert!(!retained.snapshot().running);
    assert_eq!(retained.snapshot().history.len(), 1);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.shutdowns, 1);
    assert!(calls.commands.iter().any(|name| name == "hangup"));
}
