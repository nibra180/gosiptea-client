use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::task::{Context as TaskContext, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, MessageIterator, Proxy, connection::Builder};
use zbus::proxy::MethodFlags;

const SERVICE: &str = "com.github.Baresip";
// Share the lock with pre-Sippy versions to prevent concurrent baresip startup.
const STARTUP_LOCK: &str = "com.github.GoSipTea.OwnedProcess";
const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(10);
const VOLUME_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_EVENT: usize = 64 * 1024;
const MAX_EVENTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Input,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub description: String,
    pub kind: NodeKind,
    pub is_default: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackendEvent {
    pub kind: String,
    pub id: String,
    pub peer_uri: String,
    pub peer_display_name: String,
    pub account_aor: String,
    pub detail: String,
}

/// Independent adapters for desktop work on bounded background workers.
pub trait DesktopEffects: Send {
    fn focus(&mut self) -> Result<()>;
    fn pause_media(&mut self) -> Result<()>;
    fn notify(&mut self, summary: &str, body: &str) -> Result<()>;
}

/// Called serially by the session worker. Polling never waits for an event.
pub trait Backend: Send {
    fn command(&mut self, command: &str, params: &str) -> Result<String>;
    fn poll_events(&mut self) -> Result<Vec<BackendEvent>>;
    fn discover_audio(&mut self) -> Result<Vec<Node>>;
    fn notify(&mut self, summary: &str, body: &str) -> Result<()>;
    fn pause_media(&mut self) -> Result<()>;
    fn focus(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;
    fn desktop_effects(&self) -> Option<Box<dyn DesktopEffects>> {
        None
    }
    /// Volume of the selected system output device, not the baresip stream.
    fn output_volume(&mut self, _output: &str) -> Result<Option<f32>> {
        Ok(None)
    }
    fn set_output_volume(&mut self, _output: &str, _volume: f32) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct BusState {
    owner: String,
    lost_owners: HashSet<String>,
    startup_events: VecDeque<(String, BackendEvent)>,
    events: VecDeque<BackendEvent>,
    fault: Option<String>,
    stopping: bool,
    stop_deadline: Option<Instant>,
    finished: bool,
}

type SharedState = Arc<Mutex<BusState>>;

fn state_lock(state: &SharedState) -> MutexGuard<'_, BusState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn fail(state: &SharedState, message: impl Into<String>) {
    let mut state = state_lock(state);
    if !state.stopping && state.fault.is_none() {
        state.fault = Some(message.into());
    }
}

/// Owns exactly the process it spawned, never a systemd service or a foreign bus owner.
/// Construct and use this on the session worker, not on the GPUI event thread.
pub struct RealBackend {
    connection: Connection,
    state: SharedState,
    stop: mpsc::Sender<StopRequest>,
    stop_timeout: Duration,
    supervisor: Option<JoinHandle<()>>,
    listener: Option<JoinHandle<()>>,
    owner: String,
}

pub struct RealDesktopEffects {
    connection: Connection,
}

struct StopRequest {
    deadline: Instant,
    graceful_until: Option<Instant>,
}

fn normalized_timeout(timeout: Duration, default: Duration) -> Duration {
    if timeout.is_zero() {
        default
    } else {
        timeout.max(Duration::from_millis(1))
    }
}

fn command_timeout(connection: &Connection) -> Duration {
    normalized_timeout(
        connection.method_timeout().unwrap_or(CALL_TIMEOUT),
        CALL_TIMEOUT,
    )
}

impl RealBackend {
    pub fn start(
        config_dir: &Path,
        baresip_path: &Path,
        log_path: Option<&Path>,
        sip_trace: bool,
    ) -> Result<Self> {
        Self::start_with_timeouts(
            config_dir,
            baresip_path,
            log_path,
            sip_trace,
            CALL_TIMEOUT,
            STOP_TIMEOUT,
        )
    }

    pub fn start_with_timeouts(
        config_dir: &Path,
        baresip_path: &Path,
        log_path: Option<&Path>,
        sip_trace: bool,
        command_timeout: Duration,
        stop_timeout: Duration,
    ) -> Result<Self> {
        ensure!(
            !config_dir.as_os_str().is_empty(),
            "baresip: config directory is required"
        );
        ensure!(
            !baresip_path.as_os_str().is_empty(),
            "baresip: executable is required"
        );
        ensure!(
            !sip_trace || log_path.is_some(),
            "baresip: SIP trace requires a log path"
        );
        let connection = Builder::session()?
            .method_timeout(normalized_timeout(command_timeout, CALL_TIMEOUT))
            .build()
            .context("baresip: connect to session bus")?;
        Self::start_connected_with_timeout(
            connection,
            config_dir,
            baresip_path,
            log_path,
            sip_trace,
            stop_timeout,
        )
    }

    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "used by the standalone platform integration tests"
    )]
    fn start_connected(
        connection: Connection,
        config_dir: &Path,
        baresip_path: &Path,
        log_path: Option<&Path>,
        sip_trace: bool,
    ) -> Result<Self> {
        Self::start_connected_with_timeout(
            connection,
            config_dir,
            baresip_path,
            log_path,
            sip_trace,
            STOP_TIMEOUT,
        )
    }

    fn start_connected_with_timeout(
        connection: Connection,
        config_dir: &Path,
        baresip_path: &Path,
        log_path: Option<&Path>,
        sip_trace: bool,
        stop_timeout: Duration,
    ) -> Result<Self> {
        let stop_timeout = normalized_timeout(stop_timeout, STOP_TIMEOUT);
        // Raw RequestName uses exactly Go's non-queued lock, without replacement rights.
        let bus = Proxy::new(&connection, BUS, BUS_PATH, BUS)?;
        let reply: u32 = bus.call("RequestName", &(STARTUP_LOCK, 4u32))?;
        ensure!(
            reply == 1,
            "baresip: another Sippy or GoSipTea instance owns the startup lock"
        );
        let setup = (|| -> Result<_> {
            let occupied: bool = bus.call("NameHasOwner", &(SERVICE,))?;
            ensure!(
                !occupied,
                "baresip: another process owns com.github.Baresip"
            );
            let iterator = MessageIterator::from(&connection);
            // Register before spawning so neither initial events nor a fast owner loss is missed.
            for name in [SERVICE, STARTUP_LOCK] {
                let rule = format!(
                    "type='signal',sender='org.freedesktop.DBus',path='/org/freedesktop/DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='{name}'"
                );
                let _: () = bus.call("AddMatch", &(rule,))?;
            }
            let _: () = bus.call(
                "AddMatch",
                &("type='signal',path='/baresip',interface='com.github.Baresip',member='event'",),
            )?;
            let mut command = Command::new(baresip_path);
            command.arg("-f").arg(config_dir).stdin(Stdio::null());
            if sip_trace {
                command.arg("-s");
            }
            match log_path {
                Some(path) => {
                    let file = open_log(path)?;
                    command.stdout(file.try_clone()?).stderr(file);
                }
                None => {
                    command.stdout(Stdio::null()).stderr(Stdio::null());
                }
            }
            configure_child(&mut command);
            let child = OwnedChild(Some(command.spawn().context("baresip: spawn owned child")?));
            Ok((iterator, child))
        })();
        drop(bus);
        let (iterator, child) = match setup {
            Ok(value) => value,
            Err(error) => {
                let _ = connection.close();
                return Err(error);
            }
        };
        let pid = child.pid();
        let state = Arc::new(Mutex::new(BusState::default()));
        let listener_state = state.clone();
        let listener = match thread::Builder::new()
            .name("baresip-events".into())
            .spawn(move || {
                for message in iterator {
                    match message {
                        Ok(message) => handle_message(&listener_state, &message),
                        Err(_) => break,
                    }
                }
                fail(&listener_state, "baresip: session bus disconnected");
            }) {
            Ok(listener) => listener,
            Err(error) => {
                drop(child);
                let _ = connection.close();
                return Err(error).context("baresip: start event listener");
            }
        };
        let (stop, stopped) = mpsc::channel();
        let supervisor_state = state.clone();
        let supervisor_bus = connection.clone();
        let supervisor =
            match thread::Builder::new()
                .name("baresip-process".into())
                .spawn(move || {
                    supervise(
                        child,
                        supervisor_bus,
                        supervisor_state,
                        stopped,
                        stop_timeout,
                    );
                }) {
                Ok(supervisor) => supervisor,
                Err(error) => {
                    let _ = connection.close();
                    let _ = listener.join();
                    return Err(error).context("baresip: start process supervisor");
                }
            };
        let mut backend = Self {
            connection,
            state,
            stop,
            stop_timeout,
            supervisor: Some(supervisor),
            listener: Some(listener),
            owner: String::new(),
        };
        let bus = Proxy::new(&backend.connection, BUS, BUS_PATH, BUS)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            backend.check_live()?;
            let has_owner: bool = bus.call("NameHasOwner", &(SERVICE,))?;
            if has_owner {
                let owner: String = bus.call("GetNameOwner", &(SERVICE,))?;
                let owner_pid: u32 = bus.call("GetConnectionUnixProcessID", &(&owner,))?;
                ensure!(
                    owner_pid == pid,
                    "baresip: bus owner PID does not match owned child"
                );
                let current: String = bus.call("GetNameOwner", &(SERVICE,))?;
                ensure!(
                    current == owner,
                    "baresip: service owner changed during startup"
                );
                pin_verified_owner(&backend.state, &owner)?;
                backend.check_live()?;
                backend.owner = owner;
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "baresip: timed out waiting for owned D-Bus service"
            );
            thread::sleep(TICK);
        }
        drop(bus);
        Ok(backend)
    }

    fn effects(&self) -> RealDesktopEffects {
        RealDesktopEffects {
            connection: self.connection.clone(),
        }
    }

    fn check_live(&self) -> Result<()> {
        let state = state_lock(&self.state);
        if let Some(error) = &state.fault {
            bail!("{error}");
        }
        ensure!(
            !state.stopping && !state.finished,
            "baresip: backend is closed"
        );
        ensure!(
            !self.connection.is_closed(),
            "baresip: session bus disconnected"
        );
        Ok(())
    }

    fn invoke(&self, line: &str) -> Result<String> {
        self.invoke_until(line, Instant::now() + command_timeout(&self.connection))
    }

    fn invoke_until(&self, line: &str, deadline: Instant) -> Result<String> {
        ensure!(!self.owner.is_empty(), "baresip: owner is unverified");
        let proxy = Proxy::new(&self.connection, self.owner.as_str(), "/baresip", SERVICE)?;
        let response: Option<String> = wait_until(
            proxy
                .inner()
                .call_with_flags("invoke", MethodFlags::NoAutoStart.into(), &(line,)),
            deadline.min(Instant::now() + command_timeout(&self.connection)),
        )?
        .context("baresip: invoke command")?;
        Ok(truncate_utf8(&response.unwrap_or_default(), MAX_EVENT).to_owned())
    }

    fn finish(&mut self, graceful: bool) -> Result<()> {
        if self.supervisor.is_none() {
            return Ok(());
        }
        let started = Instant::now();
        let can_quit = graceful && self.check_live().is_ok() && !self.owner.is_empty();
        let deadline = {
            let mut state = state_lock(&self.state);
            state.stopping = true;
            let deadline = started + self.stop_timeout;
            let deadline = state
                .stop_deadline
                .map_or(deadline, |previous| previous.min(deadline));
            state.stop_deadline = Some(deadline);
            deadline
        };
        // Reserve half the remaining budget for TERM, then KILL and reaping.
        let graceful_until = started + deadline.saturating_duration_since(started) / 2;
        let _ = self.stop.send(StopRequest {
            deadline,
            graceful_until: can_quit.then_some(graceful_until),
        });
        if can_quit {
            // A successful quit deregisters SIP. The child can exit before returning its reply.
            let _ = self.invoke_until("quit", graceful_until);
        }
        let supervisor_result = self.supervisor.take().unwrap().join();
        let _ = self.connection.clone().close();
        let listener_result = self.listener.take().map(JoinHandle::join);
        ensure!(
            supervisor_result.is_ok(),
            "baresip: process supervisor panicked"
        );
        ensure!(
            !matches!(listener_result, Some(Err(_))),
            "baresip: event listener panicked"
        );
        Ok(())
    }
}

impl Backend for RealBackend {
    fn command(&mut self, command: &str, params: &str) -> Result<String> {
        let line = build_command(command, params)?;
        self.check_live()?;
        self.invoke(&line)
    }

    fn poll_events(&mut self) -> Result<Vec<BackendEvent>> {
        self.check_live()?;
        Ok(state_lock(&self.state).events.drain(..).collect())
    }

    fn discover_audio(&mut self) -> Result<Vec<Node>> {
        let deadline =
            Instant::now() + Duration::from_secs(5).min(command_timeout(&self.connection));
        discover_audio_with(|args| run_command("wpctl", args, deadline, 256 * 1024))
    }

    fn notify(&mut self, summary: &str, body: &str) -> Result<()> {
        self.effects().notify(summary, body)
    }

    fn pause_media(&mut self) -> Result<()> {
        self.effects().pause_media()
    }

    fn focus(&mut self) -> Result<()> {
        self.effects().focus()
    }

    fn shutdown(&mut self) -> Result<()> {
        self.finish(true)
    }

    fn desktop_effects(&self) -> Option<Box<dyn DesktopEffects>> {
        Some(Box::new(self.effects()))
    }

    fn output_volume(&mut self, output: &str) -> Result<Option<f32>> {
        let Some(id) = self.output_sink_id(output)? else {
            return Ok(None);
        };
        let deadline = Instant::now() + VOLUME_TIMEOUT.min(command_timeout(&self.connection));
        let text = run_command(
            "wpctl",
            &["get-volume", &id.to_string()],
            deadline,
            16 * 1024,
        )
        .context("audio: read output volume")?;
        Ok(Some(parse_output_volume(&text)?))
    }

    fn set_output_volume(&mut self, output: &str, volume: f32) -> Result<()> {
        ensure!(
            volume.is_finite() && (0.0..=1.0).contains(&volume),
            "audio: volume out of range"
        );
        let id = self
            .output_sink_id(output)?
            .ok_or_else(|| anyhow!("audio: selected output is unavailable"))?;
        let deadline = Instant::now() + VOLUME_TIMEOUT.min(command_timeout(&self.connection));
        run_command(
            "wpctl",
            &["set-volume", &id.to_string(), &format!("{volume:.2}")],
            deadline,
            16 * 1024,
        )
        .context("audio: set output volume")?;
        Ok(())
    }
}

impl RealBackend {
    fn output_sink_id(&self, output: &str) -> Result<Option<u32>> {
        self.check_live()?;
        let deadline = Instant::now() + VOLUME_TIMEOUT.min(command_timeout(&self.connection));
        let text = run_command("wpctl", &["list", "audio", "sinks"], deadline, 256 * 1024)
            .context("audio: list output devices")?;
        output_sink_id(&text, output)
    }
}

fn output_sink_id(list: &str, output: &str) -> Result<Option<u32>> {
    Ok(parse_audio_list(list)?
        .into_iter()
        .find(|item| {
            item.node.kind == NodeKind::Output
                && if output.is_empty() {
                    item.node.is_default
                } else {
                    item.node.name == output
                }
        })
        .map(|item| item.id))
}

fn parse_output_volume(text: &str) -> Result<f32> {
    let value: f32 = text
        .trim()
        .strip_prefix("Volume: ")
        .and_then(|text| text.split_whitespace().next())
        .ok_or_else(|| anyhow!("audio: invalid output volume"))?
        .parse()
        .context("audio: invalid output volume")?;
    ensure!(
        value.is_finite() && value >= 0.0,
        "audio: invalid output volume"
    );
    Ok(value.min(1.0))
}

impl DesktopEffects for RealDesktopEffects {
    fn notify(&mut self, summary: &str, body: &str) -> Result<()> {
        let summary = notification_text(summary, 256);
        let body = notification_text(body, 4096);
        run_command(
            "notify-send",
            &[
                "--app-name",
                "Sippy",
                "--icon",
                "sippy",
                "--",
                &summary,
                &body,
            ],
            Instant::now() + Duration::from_secs(3).min(command_timeout(&self.connection)),
            16 * 1024,
        )?;
        Ok(())
    }

    fn pause_media(&mut self) -> Result<()> {
        let deadline = Instant::now() + command_timeout(&self.connection);
        let bus = Proxy::new(&self.connection, BUS, BUS_PATH, BUS)?;
        let names: Vec<String> = wait_until(bus.inner().call("ListNames", &()), deadline)??;
        let mut errors = Vec::new();
        for name in player_names(names).into_iter().take(64) {
            if Instant::now() >= deadline {
                errors.push("media: pause deadline exceeded".into());
                break;
            }
            let result = (|| -> Result<()> {
                // Pin the discovered owner too, so a disappearing player is never activated.
                let owner: String =
                    wait_until(bus.inner().call("GetNameOwner", &(&name,)), deadline)??;
                let proxy = Proxy::new(
                    &self.connection,
                    owner.as_str(),
                    "/org/mpris/MediaPlayer2",
                    "org.mpris.MediaPlayer2.Player",
                )?;
                let _: Option<()> = wait_until(
                    proxy
                        .inner()
                        .call_with_flags("Pause", MethodFlags::NoAutoStart.into(), &()),
                    deadline,
                )??;
                Ok(())
            })();
            if let Err(error) = result {
                errors.push(format!("media: pause {name}: {error}"));
            }
        }
        ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }

    fn focus(&mut self) -> Result<()> {
        if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none_or(|value| value.is_empty()) {
            return Ok(());
        }
        let deadline =
            Instant::now() + Duration::from_secs(2).min(command_timeout(&self.connection));
        focus_with(std::process::id(), parent_pid, |args| {
            run_command("hyprctl", args, deadline, 1024 * 1024)
        })
    }
}

impl Drop for RealBackend {
    fn drop(&mut self) {
        let _ = self.finish(false);
    }
}

fn pin_verified_owner(shared: &SharedState, owner: &str) -> Result<()> {
    let mut state = state_lock(shared);
    ensure!(
        state.owner.is_empty(),
        "baresip: service owner is already pinned"
    );
    ensure!(
        owner.starts_with(':'),
        "baresip: expected unique service owner"
    );
    ensure!(
        !state.lost_owners.contains(owner),
        "baresip: service owner disappeared during startup"
    );
    if let Some(error) = &state.fault {
        bail!("{error}");
    }
    ensure!(
        !state.stopping && !state.finished,
        "baresip: backend is closed"
    );
    // PID and current ownership were checked by the caller. Pin and drain under
    // one lock so new signals cannot overtake the verified startup events.
    state.owner = owner.to_owned();
    while let Some((sender, event)) = state.startup_events.pop_front() {
        if sender == owner {
            state.events.push_back(event);
        }
    }
    Ok(())
}

fn handle_message(shared: &SharedState, message: &zbus::Message) {
    let header = message.header();
    if header.message_type() != zbus::message::Type::Signal {
        return;
    }
    let sender = header.sender().map(|name| name.as_str()).unwrap_or("");
    let member = header.member().map(|name| name.as_str()).unwrap_or("");
    let interface = header.interface().map(|name| name.as_str()).unwrap_or("");
    let path = header.path().map(|name| name.as_str()).unwrap_or("");
    if sender == BUS && interface == BUS && path == BUS_PATH && member == "NameOwnerChanged" {
        if let Ok((name, old, new)) = message.body().deserialize::<(String, String, String)>() {
            let mut state = state_lock(shared);
            if name == SERVICE && !old.is_empty() && old != new {
                if state.lost_owners.len() < MAX_EVENTS {
                    state.lost_owners.insert(old.clone());
                } else if !state.stopping {
                    state
                        .fault
                        .get_or_insert("baresip: too many owner changes".into());
                }
                if state.owner == old && !state.stopping {
                    state
                        .fault
                        .get_or_insert("baresip: service owner disappeared or changed".into());
                }
            }
            if name == STARTUP_LOCK && !old.is_empty() && old != new && !state.stopping {
                state
                    .fault
                    .get_or_insert("baresip: startup lock was lost".into());
            }
        }
        return;
    }
    if interface != SERVICE || path != "/baresip" || member != "event" {
        return;
    }
    let mut state = state_lock(shared);
    if state.stopping
        || state.fault.is_some()
        || !sender.starts_with(':')
        || (!state.owner.is_empty() && sender != state.owner)
    {
        return;
    }
    let body = message.body();
    if body.len() > MAX_EVENT + 4096 {
        return;
    }
    let payload = if let Ok((_, _, payload)) = body.deserialize::<(String, String, String)>() {
        payload
    } else if let Ok((_, _, value)) =
        body.deserialize::<(String, String, zbus::zvariant::OwnedValue)>()
    {
        match String::try_from(value) {
            Ok(payload) => payload,
            Err(_) => return,
        }
    } else {
        return;
    };
    if let Ok(event) = parse_event(&payload) {
        if state.events.len() + state.startup_events.len() >= MAX_EVENTS {
            // Dropping CALL_CLOSED could leave the UI in a live call indefinitely.
            state.fault = Some("baresip: event queue overflow".into());
        } else if state.owner.is_empty() {
            state.startup_events.push_back((sender.to_owned(), event));
        } else {
            state.events.push_back(event);
        }
    }
}

fn supervise(
    mut child: OwnedChild,
    connection: Connection,
    state: SharedState,
    stop: mpsc::Receiver<StopRequest>,
    stop_timeout: Duration,
) {
    let request = loop {
        match child.exited() {
            Ok(true) => {
                fail(&state, "baresip: owned child exited");
                break None;
            }
            Err(error) => {
                fail(&state, format!("baresip: inspect child: {error}"));
                break None;
            }
            Ok(false) => {}
        }
        if connection.is_closed() {
            fail(&state, "baresip: session bus disconnected");
        }
        if state_lock(&state).fault.is_some() {
            break None;
        }
        match stop.recv_timeout(TICK) {
            Ok(request) => break Some(request),
            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    let deadline = *state_lock(&state)
        .stop_deadline
        .get_or_insert_with(|| Instant::now() + stop_timeout);
    let deadline = request
        .as_ref()
        .map_or(deadline, |request| deadline.min(request.deadline));
    if let Some(graceful_until) = request.and_then(|request| request.graceful_until) {
        child.wait_until(graceful_until.min(deadline)).ok();
    }
    if let Err(error) = child.terminate_until(deadline) {
        fail(&state, format!("baresip: stop child: {error}"));
    }
    drop(child);
    // Keep the bus-wide lock until the entire owned process group has been stopped and reaped.
    state_lock(&state).finished = true;
    let _ = connection.close();
}

struct OwnedChild(Option<Child>);

impl OwnedChild {
    fn pid(&self) -> u32 {
        self.0.as_ref().expect("owned child is present").id()
    }

    fn signal(&self, signal: i32) -> io::Result<()> {
        if let Some(child) = &self.0 {
            // The unreaped group leader reserves its PID, so this cannot target a reused group.
            if unsafe { libc::kill(-(child.id() as i32), signal) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn exited(&self) -> io::Result<bool> {
        let Some(child) = &self.0 else {
            return Ok(true);
        };
        // WNOWAIT keeps the PID reserved until descendants have received SIGKILL.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        loop {
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(unsafe { info.si_pid() } != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    fn reap(&mut self) -> io::Result<ExitStatus> {
        self.signal(libc::SIGKILL)?;
        let result = self.0.as_mut().expect("owned child is present").wait();
        if result.is_ok() {
            self.0.take();
        }
        result
    }

    fn wait_until(&self, deadline: Instant) -> io::Result<()> {
        while Instant::now() < deadline && !self.exited()? {
            thread::sleep(TICK.min(deadline.saturating_duration_since(Instant::now())));
        }
        Ok(())
    }

    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "used by the standalone platform integration tests"
    )]
    fn terminate(&mut self, timeout: Duration) -> io::Result<()> {
        self.terminate_until(Instant::now() + timeout)
    }

    fn terminate_until(&mut self, deadline: Instant) -> io::Result<()> {
        if self.0.is_none() {
            return Ok(());
        }
        self.signal(libc::SIGTERM)?;
        // Leave time for KILL/reaping and bus cleanup instead of spending it all on TERM.
        let cleanup_budget = TICK.min(deadline.saturating_duration_since(Instant::now()) / 10);
        self.wait_until(deadline - cleanup_budget)?;
        self.reap()?;
        Ok(())
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.is_some() {
            let _ = self.reap();
        }
    }
}

fn configure_child(command: &mut Command) {
    let parent = unsafe { libc::getpid() };
    // Only async-signal-safe syscalls may run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            // Close the race where the parent died before PR_SET_PDEATHSIG was installed.
            if libc::getppid() != parent {
                libc::_exit(127);
            }
            Ok(())
        });
    }
}

fn open_log(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .context("baresip: open private log")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == unsafe { libc::geteuid() } && metadata.nlink() == 1,
        "baresip: log must be a regular, singly linked file owned by this user"
    );
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

pub fn build_command(command: &str, params: &str) -> Result<String> {
    ensure!(
        !command.is_empty()
            && command.len() <= 32
            && command
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
        "baresip: invalid command name"
    );
    ensure!(
        params.len() <= 2048 && !params.chars().any(|ch| ch < ' '),
        "baresip: invalid command parameters"
    );
    Ok(if params.is_empty() {
        command.to_owned()
    } else {
        format!("{command} {params}")
    })
}

pub fn parse_event(payload: &str) -> Result<BackendEvent> {
    ensure!(payload.len() <= MAX_EVENT, "baresip: event is too large");
    let raw: Value = serde_json::from_str(payload).context("baresip: invalid event JSON")?;
    let object = raw
        .as_object()
        .ok_or_else(|| anyhow!("baresip: event is not an object"))?;
    let text = |key: &str| -> Result<String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(value)) => Ok(value.clone()),
            _ => bail!("baresip: invalid event field {key}"),
        }
    };
    let kind = text("type")?;
    ensure!(!kind.trim().is_empty(), "baresip: event type is missing");
    let mut event = BackendEvent {
        kind,
        ..BackendEvent::default()
    };
    match event.kind.as_str() {
        "CALL_INCOMING" | "CALL_ESTABLISHED" | "CALL_CLOSED" => {
            event.id = text("id")?;
            event.peer_uri = text("peeruri")?;
            event.peer_display_name = text("peerdisplayname")?;
            if event.peer_display_name.is_empty() {
                event.peer_display_name = text("peerdisplay")?;
            }
        }
        "REGISTER_OK" | "REGISTER_FAIL" | "UNREGISTERING" => {
            event.account_aor = text("accountaor")?;
            event.detail = text("param")?;
        }
        _ => bail!("baresip: unsupported event type"),
    }
    Ok(event)
}

struct ThreadWake(thread::Thread);

impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn wait_until<F: Future>(future: F, deadline: Instant) -> Result<F::Output> {
    // zbus 5's call_with_flags bypasses the connection method timeout. Poll its
    // future with a deadline instead of leaving a detached blocking call behind.
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = TaskContext::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        ensure!(
            Instant::now() < deadline,
            "platform: D-Bus operation timed out"
        );
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return Ok(value),
            Poll::Pending => {
                thread::park_timeout(deadline.saturating_duration_since(Instant::now()))
            }
        }
    }
}

fn truncate_utf8(value: &str, limit: usize) -> &str {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn notification_text(value: &str, limit: usize) -> String {
    let value = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut result = String::new();
    for ch in value.chars() {
        if ch.is_control() && ch != '\n' && ch != '\t' {
            continue;
        }
        let escaped = match ch {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '\'' => "&#39;".to_owned(),
            '"' => "&#34;".to_owned(),
            _ => ch.to_string(),
        };
        if result.len() + escaped.len() > limit {
            break;
        }
        result.push_str(&escaped);
    }
    result
}

fn player_names(names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| name.starts_with("org.mpris.MediaPlayer2.") && name.len() <= 255)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_available(reader: &mut impl Read, output: &mut Vec<u8>, limit: usize) -> Result<bool> {
    let mut buffer = [0u8; 8192];
    // Bound each drain too, so a noisy child cannot starve deadline checks.
    for _ in 0..32 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                ensure!(
                    output.len() + count <= limit,
                    "platform: command output exceeded {limit} bytes"
                );
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn run_command(program: &str, args: &[&str], deadline: Instant, limit: usize) -> Result<String> {
    let (status, out, err) = capture_command(Path::new(program), args, deadline, limit)?;
    ensure!(
        status.success(),
        "platform: {program} failed: {}",
        String::from_utf8_lossy(&err).trim()
    );
    String::from_utf8(out).with_context(|| format!("platform: invalid UTF-8 from {program}"))
}

/// Runs `program` to completion and returns its exit status, stdout and stderr,
/// each capped at `limit` bytes.
pub(crate) fn capture_command(
    path: &Path,
    args: &[&str],
    deadline: Instant,
    limit: usize,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let program = path.display();
    ensure!(Instant::now() < deadline, "platform: {program} timed out");
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_child(&mut command);
    let mut child = OwnedChild(Some(
        command
            .spawn()
            .with_context(|| format!("platform: run {program}"))?,
    ));
    let mut stdout = child.0.as_mut().unwrap().stdout.take().unwrap();
    let mut stderr = child.0.as_mut().unwrap().stderr.take().unwrap();
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        let out_done = read_available(&mut stdout, &mut out, limit)?;
        let err_done = read_available(&mut stderr, &mut err, limit)?;
        if child.exited()? && out_done && err_done {
            return Ok((child.reap()?, out, err));
        }
        ensure!(Instant::now() < deadline, "platform: {program} timed out");
        thread::sleep(TICK);
    }
}

#[derive(Debug)]
struct ListedNode {
    id: u32,
    node: Node,
}

fn audio_kind(class: &str) -> Option<NodeKind> {
    match class.to_ascii_lowercase().as_str() {
        "audio/sink" => Some(NodeKind::Output),
        "audio/source" => Some(NodeKind::Input),
        _ => None,
    }
}

fn bounded_lines(text: &str) -> Result<Vec<&str>> {
    let lines: Vec<_> = text.lines().collect();
    ensure!(
        lines.iter().all(|line| line.len() < 64 * 1024),
        "audio: parser line exceeds limit"
    );
    Ok(lines)
}

fn parse_audio_list(text: &str) -> Result<Vec<ListedNode>> {
    let mut nodes = Vec::new();
    let mut ids = HashSet::new();
    for line in bounded_lines(text)? {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        ensure!(
            fields.len() >= 3,
            "audio: expected tab-separated wpctl fields"
        );
        let Some(kind) = audio_kind(fields[2].trim()) else {
            continue;
        };
        ensure!(fields.len() == 4, "audio: expected four fields");
        let id: u32 = fields[0].trim().parse().context("audio: invalid node ID")?;
        ensure!(ids.insert(id), "audio: duplicate node ID");
        let name = fields[1].trim();
        ensure!(!name.is_empty(), "audio: empty node name");
        ensure!(
            matches!(fields[3].trim(), "" | "*"),
            "audio: invalid default marker"
        );
        nodes.push(ListedNode {
            id,
            node: Node {
                name: name.into(),
                description: String::new(),
                kind,
                is_default: fields[3].trim() == "*",
            },
        });
        ensure!(nodes.len() <= 256, "audio: too many nodes");
    }
    Ok(nodes)
}

fn parse_audio_filters(text: &str) -> Result<Vec<ListedNode>> {
    let (mut audio, mut filters, mut found_audio) = (false, false, false);
    let mut records = String::new();
    for line in bounded_lines(text)? {
        let line = line.trim();
        match line {
            "Audio" => {
                audio = true;
                found_audio = true;
                filters = false;
                continue;
            }
            "Video" | "Settings" => {
                audio = false;
                filters = false;
                continue;
            }
            _ => {}
        }
        if !audio {
            continue;
        }
        let line = line.trim_start_matches([' ', '\t', '│', '├', '└', '─']);
        if line.ends_with(':') {
            filters = line == "Filters:";
            continue;
        }
        if !filters || !line.ends_with(']') {
            continue;
        }
        let Some(start) = line.rfind('[') else {
            continue;
        };
        let class = &line[start + 1..line.len() - 1];
        if audio_kind(class).is_none() {
            continue;
        }
        let entry = line[..start].trim();
        let default = if entry.starts_with('*') { "*" } else { "" };
        let entry = entry.trim_start_matches('*').trim();
        let (id, name) = entry
            .split_once('.')
            .ok_or_else(|| anyhow!("audio: filter node lacks ID separator"))?;
        records.push_str(&format!(
            "{}\t{}\t{class}\t{default}\n",
            id.trim(),
            name.trim()
        ));
    }
    ensure!(found_audio, "audio: missing Audio section");
    parse_audio_list(&records)
}

fn inspect_node(text: &str, listed: Node) -> Result<Option<Node>> {
    let mut props = HashMap::new();
    for line in bounded_lines(text)? {
        let line = line.trim().trim_start_matches('*').trim();
        let Some((key, value)) = line.split_once(" = ") else {
            continue;
        };
        if !matches!(
            key.trim(),
            "node.name" | "node.description" | "node.nick" | "media.class"
        ) {
            continue;
        }
        let value = value.trim();
        let value = if value.starts_with('"') {
            decode_property(value)?
        } else {
            value.to_owned()
        };
        props.insert(key.trim(), value);
    }
    let class = props
        .get("media.class")
        .ok_or_else(|| anyhow!("audio: missing media.class"))?
        .to_ascii_lowercase();
    if matches!(
        class.as_str(),
        "audio/source/internal" | "audio/sink/internal"
    ) {
        return Ok(None);
    }
    let kind = audio_kind(&class).ok_or_else(|| anyhow!("audio: unsupported media.class"))?;
    let name = props
        .remove("node.name")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("audio: missing node.name"))?;
    ensure!(
        name == listed.name && kind == listed.kind,
        "audio: node changed during discovery"
    );
    let description = props
        .remove("node.description")
        .filter(|s| !s.is_empty())
        .or_else(|| props.remove("node.nick").filter(|s| !s.is_empty()))
        .unwrap_or_else(|| name.clone());
    Ok(Some(Node {
        name,
        description,
        kind,
        is_default: listed.is_default,
    }))
}

fn decode_property(value: &str) -> Result<String> {
    // wpctl uses C/Go-style quoted strings, including hexadecimal and octal escapes.
    ensure!(
        value.len() >= 2 && value.ends_with('"'),
        "audio: unterminated property"
    );
    let mut chars = value[1..value.len() - 1].chars();
    let mut result = Vec::new();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            ensure!(
                ch != '"' && ch != '\n' && ch != '\r',
                "audio: invalid quoted property"
            );
            result.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
            continue;
        }
        let ch = chars
            .next()
            .ok_or_else(|| anyhow!("audio: incomplete escape"))?;
        match ch {
            'a' => result.push(b'\x07'),
            'b' => result.push(b'\x08'),
            'f' => result.push(b'\x0c'),
            'n' => result.push(b'\n'),
            'r' => result.push(b'\r'),
            't' => result.push(b'\t'),
            'v' => result.push(b'\x0b'),
            '\\' => result.push(b'\\'),
            '"' => result.push(b'"'),
            'x' | 'u' | 'U' | '0'..='7' => {
                let (radix, count, mut digits) = match ch {
                    'x' => (16, 2, String::new()),
                    'u' => (16, 4, String::new()),
                    'U' => (16, 8, String::new()),
                    _ => (8, 2, ch.to_string()),
                };
                for _ in 0..count {
                    digits.push(
                        chars
                            .next()
                            .ok_or_else(|| anyhow!("audio: incomplete escape"))?,
                    );
                }
                let code = u32::from_str_radix(&digits, radix).context("audio: invalid escape")?;
                if radix == 8 || ch == 'x' {
                    ensure!(code <= 255, "audio: invalid byte escape");
                    result.push(code as u8);
                } else {
                    let ch =
                        char::from_u32(code).ok_or_else(|| anyhow!("audio: invalid character"))?;
                    result.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                }
            }
            _ => bail!("audio: unknown escape"),
        }
    }
    String::from_utf8(result).context("audio: property is not UTF-8")
}

fn discover_audio_with(mut run: impl FnMut(&[&str]) -> Result<String>) -> Result<Vec<Node>> {
    let mut nodes = parse_audio_list(&run(&["list", "audio"])?)?;
    for filter in parse_audio_filters(&run(&["status", "-n"])?)? {
        if let Some(existing) = nodes.iter_mut().find(|node| node.id == filter.id) {
            ensure!(
                existing.node.name == filter.node.name && existing.node.kind == filter.node.kind,
                "audio: node changed between list and status"
            );
            existing.node.is_default = filter.node.is_default;
        } else {
            nodes.push(filter);
        }
    }
    ensure!(nodes.len() <= 256, "audio: too many nodes");
    let mut available = Vec::new();
    for node in nodes {
        let output = run(&["inspect", &node.id.to_string()])?;
        if let Some(node) = inspect_node(&output, node.node)? {
            available.push(node);
        }
    }
    Ok(available)
}

fn parent_pid(pid: u32) -> Result<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, suffix) = stat
        .rsplit_once(')')
        .ok_or_else(|| anyhow!("desktop: malformed proc stat"))?;
    suffix
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("desktop: missing parent PID"))?
        .parse()
        .context("desktop: invalid parent PID")
}

fn valid_address(address: &str) -> bool {
    address.strip_prefix("0x").is_some_and(|hex| {
        !hex.is_empty()
            && address.len() <= 32
            && hex.bytes().all(|ch| ch.is_ascii_hexdigit())
            && u64::from_str_radix(hex, 16).is_ok()
    })
}

fn focus_with(
    mut pid: u32,
    mut parent: impl FnMut(u32) -> Result<u32>,
    mut run: impl FnMut(&[&str]) -> Result<String>,
) -> Result<()> {
    let clients: Value = serde_json::from_str(&run(&["-j", "clients"])?)?;
    let clients = clients
        .as_array()
        .ok_or_else(|| anyhow!("desktop: clients is not an array"))?;
    for _ in 0..16 {
        if pid <= 1 {
            break;
        }
        // Start with the GPUI process itself. A terminal ancestor is only a fallback.
        if let Some(address) = clients.iter().find_map(|client| {
            let address = client.get("address")?.as_str()?;
            (client.get("pid")?.as_u64()? == u64::from(pid) && valid_address(address))
                .then_some(address)
        }) {
            let lua = format!("hl.dsp.focus({{ window = \"address:{address}\" }})");
            if run(&["dispatch", &lua]).is_err() {
                run(&["dispatch", "focuswindow", &format!("address:{address}")])?;
            }
            return Ok(());
        }
        let next = parent(pid)?;
        if next == pid {
            break;
        }
        pid = next;
    }
    Ok(())
}

#[cfg(test)]
mod volume_tests {
    use super::{output_sink_id, parse_output_volume};

    #[test]
    fn selects_only_the_configured_sink_or_the_current_default() {
        let list = "34\tsink.hyperx\taudio/sink\t\n59\tsink.hdmi\taudio/sink\t*\n88\tinput\taudio/source\t\n";
        assert_eq!(output_sink_id(list, "sink.hyperx").unwrap(), Some(34));
        assert_eq!(output_sink_id(list, "").unwrap(), Some(59));
        assert_eq!(output_sink_id(list, "missing").unwrap(), None);
        assert!(output_sink_id("malformed", "").is_err());
    }

    #[test]
    fn reads_wpctl_output_volume_and_rejects_invalid_values() {
        assert_eq!(parse_output_volume("Volume: 0.80\n").unwrap(), 0.8);
        assert_eq!(parse_output_volume("Volume: 0.55 [MUTED]\n").unwrap(), 0.55);
        assert_eq!(parse_output_volume("Volume: 1.40\n").unwrap(), 1.0);
        assert!(parse_output_volume("Volume: NaN").is_err());
        assert!(parse_output_volume("oops").is_err());
    }
}
