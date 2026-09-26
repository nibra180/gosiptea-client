use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::{DateTime, Utc};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    domain::{self, Contact, HistoryEntry, State},
    platform::{Backend, BackendEvent, DesktopEffects, Node, NodeKind, RealBackend},
    storage::{Account, AccountCredentials, AudioConfig, Store},
};

#[derive(Clone, Debug)]
pub struct Config {
    pub config_dir: PathBuf,
    pub baresip_path: PathBuf,
    pub country_calling_code: String,
    pub log_path: Option<PathBuf>,
    pub sip_trace: bool,
    pub command_timeout: Duration,
    pub stop_timeout: Duration,
}

impl Config {
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
            baresip_path: "baresip".into(),
            country_calling_code: "49".into(),
            log_path: None,
            sip_trace: false,
            command_timeout: Duration::from_secs(10),
            stop_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioField {
    Output,
    Input,
    Ringtone,
}

pub enum Action {
    Dial(String),
    Answer,
    Reject,
    Hangup,
    ToggleMute,
    ToggleDnd,
    AddContact(Contact),
    RemoveContact(String),
    SetAudio { field: AudioField, name: String },
    RefreshAudio,
    WatchOutputVolume(bool),
    SetOutputVolume(f32),
    SaveAccount(AccountCredentials),
    Quit,
}

/// Owned copies only. Account contains a password-presence flag, never a secret.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: State,
    pub contacts: Vec<Contact>,
    pub audio_nodes: Vec<Node>,
    pub audio_config: AudioConfig,
    pub ringtone_restart_required: bool,
    /// None when the selected output is unavailable.
    pub output_volume: Option<f32>,
    pub account: Account,
    pub history: Vec<HistoryEntry>,
    pub running: bool,
    pub last_error: String,
    pub revision: u64,
    pub now: DateTime<Utc>,
}

fn real_backend(config: &Config) -> Result<Box<dyn Backend>> {
    Ok(Box::new(RealBackend::start_with_timeouts(
        &config.config_dir,
        &config.baresip_path,
        config.log_path.as_deref(),
        config.sip_trace,
        config.command_timeout,
        config.stop_timeout,
    )?))
}

pub type BackendFactory = Box<dyn FnMut(&Config) -> Result<Box<dyn Backend>> + Send>;
pub type Clock = Box<dyn Fn() -> DateTime<Utc> + Send>;

pub trait SessionStore: Send {
    fn contacts(&mut self) -> Result<Vec<Contact>>;
    fn add_contact(&mut self, contact: Contact) -> Result<()>;
    fn remove_contact(&mut self, uri: &str) -> Result<()>;
    fn audio(&mut self) -> Result<AudioConfig>;
    fn save_audio(&mut self, config: &AudioConfig) -> Result<()>;
    fn account(&mut self) -> Result<Account>;
    fn save_account(&mut self, credentials: AccountCredentials) -> Result<()>;
    fn history(&mut self) -> Result<Vec<HistoryEntry>>;
    fn save_history(&mut self, history: &[HistoryEntry]) -> Result<()>;
}

impl SessionStore for Store {
    fn contacts(&mut self) -> Result<Vec<Contact>> {
        self.load_contacts()
    }
    fn add_contact(&mut self, contact: Contact) -> Result<()> {
        Store::add_contact(self, &contact.into())
    }
    fn remove_contact(&mut self, uri: &str) -> Result<()> {
        Store::remove_contact(self, uri)
    }
    fn audio(&mut self) -> Result<AudioConfig> {
        self.load_audio()
    }
    fn save_audio(&mut self, config: &AudioConfig) -> Result<()> {
        Store::save_audio(self, config)
    }
    fn account(&mut self) -> Result<Account> {
        self.load_account()
    }
    fn save_account(&mut self, credentials: AccountCredentials) -> Result<()> {
        Store::save_account(self, &credentials)
    }
    fn history(&mut self) -> Result<Vec<HistoryEntry>> {
        self.load_history()
    }
    fn save_history(&mut self, history: &[HistoryEntry]) -> Result<()> {
        Store::save_history(self, history)
    }
}

const OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_CAPACITY: usize = 32;
const EFFECT_CAPACITY: usize = 8;
const WORKER_TICK: Duration = Duration::from_millis(20);
const VOLUME_POLL: Duration = Duration::from_secs(1);

struct Request {
    action: Action,
    deadline: Instant,
    reply: Option<mpsc::Sender<Result<()>>>,
}

impl Request {
    fn complete(self, result: Result<()>) {
        if let Some(reply) = self.reply {
            let _ = reply.send(result);
        }
    }
}

pub struct SessionHandle {
    requests: mpsc::SyncSender<Request>,
    stopping: Arc<AtomicBool>,
    snapshot: Arc<Mutex<Snapshot>>,
    worker: Option<JoinHandle<()>>,
}

impl SessionHandle {
    /// Startup runs on the worker; this waits for its startup result. Call before opening the window.
    pub fn start(config: Config) -> Result<Self> {
        Self::start_with_factory(config, Box::new(real_backend), Box::new(Utc::now))
    }

    /// Tests supply a backend factory and clock and use a temporary configuration directory.
    pub fn start_with_factory(config: Config, factory: BackendFactory, now: Clock) -> Result<Self> {
        let store = Box::new(Store::new(config.config_dir.clone()));
        Self::start_with_dependencies(config, factory, store, now)
    }

    pub fn start_with_dependencies(
        config: Config,
        factory: BackendFactory,
        store: Box<dyn SessionStore>,
        now: Clock,
    ) -> Result<Self> {
        if config.config_dir.as_os_str().is_empty() {
            bail!("session: config directory is required");
        }
        if config.sip_trace && config.log_path.is_none() {
            bail!("session: SIP trace requires a log path");
        }
        let snapshot = Arc::new(Mutex::new(Snapshot::empty(now())));
        let (requests, receiver) = mpsc::sync_channel(REQUEST_CAPACITY);
        let (started, startup) = mpsc::sync_channel(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = stopping.clone();
        let published = Arc::clone(&snapshot);
        let worker = thread::Builder::new()
            .name("sip-session".into())
            .spawn(
                move || match Worker::start(config, factory, store, now, published) {
                    Ok(mut worker) => {
                        if started.send(Ok(())).is_ok() {
                            worker.run(receiver, worker_stopping);
                        } else {
                            let _ = worker.stop(true);
                        }
                    }
                    Err(error) => {
                        let _ = started.send(Err(error));
                    }
                },
            )
            .context("session: spawn worker")?;
        match startup.recv_timeout(OPERATION_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                requests,
                stopping,
                snapshot,
                worker: Some(worker),
            }),
            result => {
                stopping.store(true, Ordering::Release);
                if worker.is_finished() {
                    let _ = worker.join();
                }
                match result {
                    Ok(Err(error)) => Err(error),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        Err(anyhow!("session: startup timed out"))
                    }
                    _ => Err(anyhow!("session: startup worker stopped unexpectedly")),
                }
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Enqueues without waiting for I/O. A full queue returns an error immediately.
    pub fn dispatch(&self, action: Action) -> Result<()> {
        if matches!(action, Action::Quit) {
            self.request_shutdown();
            return Ok(());
        }
        self.enqueue(Request {
            action,
            deadline: Instant::now() + OPERATION_TIMEOUT,
            reply: None,
        })
    }

    fn enqueue(&self, request: Request) -> Result<()> {
        if self.stopping.load(Ordering::Acquire) {
            bail!("session: stopping");
        }
        self.requests
            .try_send(request)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => anyhow!("session: request queue full"),
                mpsc::TrySendError::Disconnected(_) => anyhow!("session: not running"),
            })
    }

    /// Waits for serialized work, not asynchronous desktop effects, for at most 15 seconds.
    pub fn dispatch_wait(&self, action: Action) -> Result<()> {
        self.dispatch_until(action, Instant::now() + OPERATION_TIMEOUT)
    }

    fn dispatch_until(&self, action: Action, deadline: Instant) -> Result<()> {
        if matches!(action, Action::Quit) {
            self.request_shutdown();
            if let Some(worker) = &self.worker {
                wait_for_worker(worker, deadline)?;
            }
            return self.terminal_result();
        }
        let (reply, result) = mpsc::channel();
        self.enqueue(Request {
            action,
            deadline,
            reply: Some(reply),
        })?;
        result
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => anyhow!("session: action timed out"),
                mpsc::RecvTimeoutError::Disconnected => {
                    anyhow!("session: worker stopped before completing action")
                }
            })?
    }

    /// Requests stop even when the action queue is full; safe for panic cleanup.
    pub fn request_shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    fn terminal_result(&self) -> Result<()> {
        let terminal = self.snapshot();
        if !terminal.last_error.is_empty() {
            bail!(terminal.last_error);
        }
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.shutdown_until(Instant::now() + OPERATION_TIMEOUT)
    }

    fn shutdown_until(&mut self, deadline: Instant) -> Result<()> {
        self.request_shutdown();
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        // A backend that violates its timeout contract must not trap Drop in join.
        wait_for_worker(&worker, deadline)?;
        worker
            .join()
            .map_err(|_| anyhow!("session: worker panicked"))?;
        self.terminal_result()
    }
}

fn wait_for_worker(worker: &JoinHandle<()>, deadline: Instant) -> Result<()> {
    while !worker.is_finished() {
        if Instant::now() >= deadline {
            bail!("session: shutdown timed out; worker cleanup is still pending");
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl Snapshot {
    fn empty(now: DateTime<Utc>) -> Self {
        Self {
            state: State::default(),
            contacts: Vec::new(),
            audio_nodes: Vec::new(),
            audio_config: AudioConfig::default(),
            ringtone_restart_required: false,
            output_volume: None,
            account: Account::default(),
            history: Vec::new(),
            running: false,
            last_error: String::new(),
            revision: 0,
            now,
        }
    }
}

enum DesktopTask {
    Focus,
    Pause,
    Notify { summary: &'static str, body: String },
}

impl DesktopTask {
    fn lane(&self) -> usize {
        match self {
            Self::Focus => 0,
            Self::Pause => 1,
            Self::Notify { .. } => 2,
        }
    }

    fn run(self, mut adapter: Box<dyn DesktopEffects>) -> Result<()> {
        match self {
            Self::Focus => adapter.focus().context("session: focus window"),
            Self::Pause => adapter.pause_media().context("session: pause media"),
            Self::Notify { summary, body } => adapter
                .notify(summary, &body)
                .with_context(|| format!("session: send {summary} notification")),
        }
    }
}

struct DesktopJob {
    adapter: Box<dyn DesktopEffects>,
    task: DesktopTask,
    deadline: Instant,
}

#[derive(Default)]
struct EffectLane {
    pending: VecDeque<DesktopJob>,
    active: Option<(u64, JoinHandle<Result<()>>)>,
}

impl EffectLane {
    fn submit(&mut self, adapter: Box<dyn DesktopEffects>, task: DesktopTask) -> Result<()> {
        if self.pending.len() == EFFECT_CAPACITY {
            bail!("session: desktop effect queue full");
        }
        self.pending.push_back(DesktopJob {
            adapter,
            task,
            deadline: Instant::now() + OPERATION_TIMEOUT,
        });
        Ok(())
    }

    fn poll(&mut self, generation: u64) -> Vec<anyhow::Error> {
        let mut errors = Vec::new();
        if self
            .active
            .as_ref()
            .is_some_and(|(_, job)| job.is_finished())
        {
            let (job_generation, job) = self.active.take().unwrap();
            let result = job
                .join()
                .unwrap_or_else(|_| Err(anyhow!("session: desktop effect worker panicked")));
            if job_generation == generation
                && let Err(error) = result
            {
                errors.push(error);
            }
        }
        while self.active.is_none() {
            let Some(job) = self.pending.pop_front() else {
                break;
            };
            if Instant::now() >= job.deadline {
                errors.push(anyhow!("session: desktop effect expired"));
                continue;
            }
            match thread::Builder::new()
                .name("sip-desktop-effect".into())
                .spawn(move || job.task.run(job.adapter))
            {
                Ok(handle) => self.active = Some((generation, handle)),
                Err(error) => errors.push(anyhow!(error).context("session: spawn desktop effect")),
            }
        }
        errors
    }
}

// One in-flight job per lane. Notifications stay FIFO without waiting for focus or media.
// Restart retains active handles so repeated restarts cannot create unbounded threads.
#[derive(Default)]
struct Effects {
    lanes: [EffectLane; 3],
    generation: u64,
}

impl Effects {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        for lane in &mut self.lanes {
            lane.pending.clear();
        }
    }

    fn poll(&mut self) -> Vec<anyhow::Error> {
        self.lanes
            .iter_mut()
            .flat_map(|lane| lane.poll(self.generation))
            .collect()
    }
}

impl Drop for Effects {
    fn drop(&mut self) {
        self.invalidate();
        // Running adapters own no session state and have platform timeouts. Never join
        // an unfinished adapter here: a broken fake must not prevent SIP cleanup.
        let _ = self.poll();
    }
}

struct Worker {
    config: Config,
    factory: BackendFactory,
    backend: Option<Box<dyn Backend>>,
    store: Box<dyn SessionStore>,
    now: Clock,
    current: Snapshot,
    published: Arc<Mutex<Snapshot>>,
    ringtone: String,
    effects: Effects,
    watch_output_volume: bool,
    volume_checked: Instant,
}

impl Worker {
    fn start(
        config: Config,
        mut factory: BackendFactory,
        mut store: Box<dyn SessionStore>,
        now: Clock,
        published: Arc<Mutex<Snapshot>>,
    ) -> Result<Self> {
        let mut current = Snapshot::empty(now());
        current.contacts = store.contacts().context("session: load contacts")?;
        current.audio_config = store.audio().context("session: load audio config")?;
        current.account = store.account().context("session: load account")?;
        current.history = store.history().context("session: load call history")?;
        let ringtone = ringtone_selection(&current.audio_config).to_owned();
        let backend = factory(&config).context("session: start baresip")?;
        current.running = true;
        let mut worker = Self {
            config,
            factory,
            backend: Some(backend),
            store,
            now,
            current,
            published,
            ringtone,
            effects: Effects::default(),
            watch_output_volume: false,
            volume_checked: Instant::now(),
        };
        if let Err(error) = worker.registration_state() {
            worker.record_error(error);
        }
        if let Err(error) = worker.refresh_audio() {
            worker.record_error(error);
        }
        worker.publish();
        Ok(worker)
    }

    fn publish(&mut self) {
        self.current.now = (self.now)();
        self.current.revision = self.current.revision.wrapping_add(1);
        self.current.ringtone_restart_required =
            self.ringtone != ringtone_selection(&self.current.audio_config);
        let mut snapshot = self.current.clone();
        snapshot.history = domain::resolve_history_peers(
            &snapshot.history,
            &snapshot.contacts,
            &self.config.country_calling_code,
        );
        for field in [
            &mut snapshot.account.server,
            &mut snapshot.account.username,
            &mut snapshot.account.domain,
            &mut snapshot.account.login,
        ] {
            *field = domain::clamp_text(field, crate::storage::MAX_FIELD_LENGTH);
        }
        *self
            .published
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = snapshot;
    }

    fn record_error(&mut self, error: anyhow::Error) {
        self.current.last_error = format!("{error:#}");
    }

    fn backend(&mut self) -> Result<&mut (dyn Backend + '_)> {
        match self.backend.as_mut() {
            Some(backend) => Ok(backend.as_mut()),
            None => Err(anyhow!("session: not running")),
        }
    }

    fn run(&mut self, requests: mpsc::Receiver<Request>, stopping: Arc<AtomicBool>) {
        let mut tick = Instant::now();
        loop {
            if stopping.load(Ordering::Acquire) {
                self.current.last_error.clear();
                break;
            }
            if let Err(error) = combine_errors(self.effects.poll()) {
                self.record_error(error);
                self.publish();
            }
            match self.backend().and_then(Backend::poll_events) {
                Ok(events) => {
                    for raw in events {
                        if stopping.load(Ordering::Acquire) {
                            break;
                        }
                        if let Err(error) = self.handle_event(raw) {
                            self.record_error(error);
                        }
                        self.publish();
                    }
                }
                Err(error) => {
                    let mut errors = vec![error.context("session: baresip stopped")];
                    if let Err(error) = self.stop(false) {
                        errors.push(error);
                    }
                    if let Err(error) = combine_errors(errors) {
                        self.record_error(error);
                    }
                    break;
                }
            }
            match requests.recv_timeout(WORKER_TICK) {
                Ok(request) => {
                    if stopping.load(Ordering::Acquire) {
                        request.complete(Err(anyhow!("session: stopping")));
                        self.current.last_error.clear();
                        break;
                    }
                    if Instant::now() >= request.deadline {
                        let message = "session: action expired before execution";
                        self.record_error(anyhow!(message));
                        self.publish();
                        request.complete(Err(anyhow!(message)));
                        continue;
                    }
                    self.current.last_error.clear();
                    let result = self.handle_action(request.action);
                    if let Err(error) = &result {
                        self.record_error(anyhow!("{error:#}"));
                    }
                    self.publish();
                    if let Some(reply) = request.reply {
                        let _ = reply.send(result);
                    }
                    if !self.current.running {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if self.watch_output_volume && self.volume_checked.elapsed() >= VOLUME_POLL {
                if let Err(error) = self.refresh_output_volume() {
                    self.record_error(error);
                }
                self.publish();
            }
            if tick.elapsed() >= Duration::from_millis(250) {
                self.publish();
                tick = Instant::now();
            }
        }
        stopping.store(true, Ordering::Release);
        for request in requests.try_iter() {
            request.complete(Err(anyhow!("session: stopped before completing action")));
        }
        drop(requests);
        if self.backend.is_some()
            && let Err(error) = self.stop(true)
        {
            self.record_error(error);
        }
        self.publish();
    }

    fn registration_state(&mut self) -> Result<()> {
        let response = self
            .backend()?
            .command("reginfo", "")
            .context("session: read registration state")?;
        let info = domain::parse_registration_output(&response);
        if !info.known {
            bail!("session: baresip returned an unrecognized reginfo response");
        }
        self.current.state = domain::reduce(
            &self.current.state,
            domain::Event::RegistrationSnapshot(info),
            &self.current.contacts,
            &self.config.country_calling_code,
            (self.now)(),
        )
        .state;
        Ok(())
    }

    fn handle_action(&mut self, action: Action) -> Result<()> {
        let domain_action = match action {
            Action::Dial(target) => domain::Action::Dial(target),
            Action::Answer => domain::Action::Answer,
            Action::Reject | Action::Hangup => domain::Action::Hangup,
            Action::ToggleMute => domain::Action::ToggleMute,
            Action::ToggleDnd => domain::Action::ToggleDnd,
            Action::AddContact(mut contact) => {
                let account = &self.current.account;
                let domain = if account.domain.is_empty() {
                    &account.server
                } else {
                    &account.domain
                };
                contact.uri = domain::normalize_contact_uri(&contact.uri, domain);
                if contact.uri.is_empty() {
                    bail!("session: enter a full SIP address or configure an account domain");
                }
                self.store
                    .add_contact(contact)
                    .context("session: add contact")?;
                self.current.contacts = self.store.contacts().context("session: read contacts")?;
                return Ok(());
            }
            Action::RemoveContact(uri) => {
                self.store
                    .remove_contact(&uri)
                    .context("session: remove contact")?;
                self.current.contacts = self.store.contacts().context("session: read contacts")?;
                return Ok(());
            }
            Action::SetAudio { field, name } => return self.select_audio_field(field, name),
            Action::RefreshAudio => return self.refresh_audio(),
            Action::WatchOutputVolume(watch) => {
                self.watch_output_volume = watch;
                if watch {
                    return self.refresh_output_volume();
                }
                return Ok(());
            }
            Action::SetOutputVolume(volume) => return self.set_output_volume(volume),
            Action::SaveAccount(credentials) => return self.save_account(credentials),
            Action::Quit => return self.stop(true),
        };
        self.apply_domain_event(domain::Event::Action(domain_action))
    }

    fn handle_event(&mut self, raw: BackendEvent) -> Result<()> {
        if let Some(event) = translate_event(raw) {
            self.apply_domain_event(event)?;
        }
        Ok(())
    }

    fn apply_domain_event(&mut self, event: domain::Event) -> Result<()> {
        let now = (self.now)();
        let failed_dial = matches!(&event, domain::Event::Action(domain::Action::Dial(_)));
        let transition = domain::reduce(
            &self.current.state,
            event,
            &self.current.contacts,
            &self.config.country_calling_code,
            now,
        );
        if let Some(error) = transition.error {
            return Err(error.into());
        }
        for command in &transition.commands {
            if let Err(error) = self.execute_command(command) {
                let mut errors = vec![error];
                if failed_dial {
                    let closed = domain::reduce(
                        &transition.state,
                        closed_event(&transition.state.call_id),
                        &self.current.contacts,
                        &self.config.country_calling_code,
                        now,
                    );
                    if let Err(error) = self.append_history(closed.history) {
                        errors.push(error);
                    }
                }
                return combine_errors(errors);
            }
        }
        self.current.state = transition.state;
        let mut errors = Vec::new();
        if let Err(error) = self.append_history(transition.history) {
            errors.push(error);
        }
        self.publish();
        for notification in transition.notifications {
            if notification.kind == domain::NotificationKind::Incoming {
                // Neither a focus failure nor a failed notification may skip media pause.
                if let Err(error) = self.desktop_effect(DesktopTask::Focus) {
                    errors.push(error.context("session: focus window"));
                }
                if let Err(error) = self.desktop_effect(DesktopTask::Pause) {
                    errors.push(error.context("session: pause media"));
                }
            }
            let summary = notification_summary(notification.kind);
            let body = domain::notification_text(&notification.body);
            if let Err(error) = self.desktop_effect(DesktopTask::Notify { summary, body }) {
                errors.push(error.context(format!("session: send {summary} notification")));
            }
        }
        errors.extend(self.effects.poll());
        combine_errors(errors)
    }

    fn desktop_effect(&mut self, task: DesktopTask) -> Result<()> {
        if let Some(adapter) = self.backend()?.desktop_effects() {
            self.effects.lanes[task.lane()].submit(adapter, task)
        } else {
            match task {
                DesktopTask::Focus => self.backend()?.focus(),
                DesktopTask::Pause => self.backend()?.pause_media(),
                DesktopTask::Notify { summary, body } => self.backend()?.notify(summary, &body),
            }
        }
    }

    fn execute_command(&mut self, command: &domain::Command) -> Result<()> {
        use domain::CommandKind;
        let (name, params) = match command.kind {
            CommandKind::Dial => ("dial", command.parameter.as_str()),
            CommandKind::Accept => ("accept", command.parameter.as_str()),
            CommandKind::Hangup => ("hangup", command.parameter.as_str()),
            CommandKind::Reject => ("hangup", "scode=603 reason=Decline"),
            CommandKind::Mute => ("mute", command.parameter.as_str()),
        };
        let response = self
            .backend()?
            .command(name, params)
            .with_context(|| format!("session: execute {name}"))?;
        check_call_response(&response).with_context(|| format!("session: execute {name}"))
    }

    fn append_history(&mut self, added: Vec<HistoryEntry>) -> Result<()> {
        let write = !added.is_empty();
        let (history, mut errors) = merge_history(&self.current.history, added);
        if (write || !errors.is_empty())
            && let Err(error) = self.store.save_history(&history)
        {
            errors.push(error.context("session: persist call history"));
        }
        self.current.history = history;
        combine_errors(errors)
    }

    fn finalize_call(&mut self, end_requested: bool) -> Result<()> {
        if self.current.state.call_state == domain::CallState::Idle {
            return Ok(());
        }
        let mut state = self.current.state.clone();
        state.end_requested |= end_requested;
        let transition = domain::reduce(
            &state,
            closed_event(&state.call_id),
            &self.current.contacts,
            &self.config.country_calling_code,
            (self.now)(),
        );
        self.current.state = transition.state;
        self.append_history(transition.history)
    }

    fn save_account(&mut self, credentials: AccountCredentials) -> Result<()> {
        if self.current.state.call_state != domain::CallState::Idle {
            bail!("session: account changes require an idle call");
        }
        self.store
            .save_account(credentials)
            .context("session: write account")?;
        self.current.account = self
            .store
            .account()
            .context("session: account was written but cannot be read")?;
        self.effects.invalidate();
        self.current.running = false;
        let dnd = self.current.state.dnd;
        self.current.state = State {
            dnd,
            ..State::default()
        };
        if let Some(mut backend) = self.backend.take() {
            backend
                .shutdown()
                .context("session: account was written but stopping baresip failed")?;
        }
        self.backend = Some(
            (self.factory)(&self.config)
                .context("session: account was written but baresip restart failed")?,
        );
        self.ringtone = ringtone_selection(&self.current.audio_config).to_owned();
        self.current.running = true;
        if let Err(error) = self.registration_state() {
            self.record_error(error);
        }
        Ok(())
    }

    fn stop(&mut self, hangup: bool) -> Result<()> {
        self.effects.invalidate();
        let mut errors = Vec::new();
        if hangup
            && self.backend.is_some()
            && self.current.state.call_state != domain::CallState::Idle
            && let Err(error) = self.execute_command(&domain::Command {
                kind: domain::CommandKind::Hangup,
                parameter: String::new(),
            })
        {
            errors.push(error.context("session: hang up before stop"));
        }
        if let Err(error) = self.finalize_call(hangup) {
            errors.push(error);
        }
        if let Some(mut backend) = self.backend.take()
            && let Err(error) = backend.shutdown()
        {
            errors.push(error.context("session: stop baresip"));
        }
        let dnd = self.current.state.dnd;
        self.current.state = State {
            dnd,
            ..State::default()
        };
        self.current.running = false;
        self.publish();
        combine_errors(errors)
    }

    fn refresh_audio(&mut self) -> Result<()> {
        self.current.audio_nodes = self
            .backend()?
            .discover_audio()
            .context("session: list audio")?;
        self.refresh_output_volume()
    }

    fn refresh_output_volume(&mut self) -> Result<()> {
        self.volume_checked = Instant::now();
        let output = self.current.audio_config.output.clone();
        let volume = self
            .backend()?
            .output_volume(&output)
            .context("session: read output volume");
        self.current.output_volume = volume.as_ref().ok().copied().flatten();
        volume.map(|_| ())
    }

    fn set_output_volume(&mut self, volume: f32) -> Result<()> {
        ensure!(volume.is_finite(), "session: invalid output volume");
        let volume = volume.clamp(0.0, 1.0);
        let output = self.current.audio_config.output.clone();
        self.backend()?
            .set_output_volume(&output, volume)
            .context("session: set output volume")?;
        self.current.output_volume = Some(volume);
        self.volume_checked = Instant::now();
        Ok(())
    }

    fn select_audio_field(&mut self, field: AudioField, name: String) -> Result<()> {
        let mut selected = self.current.audio_config.clone();
        match field {
            AudioField::Output => selected.output = name,
            AudioField::Input => selected.input = name,
            AudioField::Ringtone => selected.alert = name,
        }
        self.select_audio(selected)
    }

    fn select_audio(&mut self, mut selected: AudioConfig) -> Result<()> {
        selected.output = selected.output.trim().to_owned();
        selected.input = selected.input.trim().to_owned();
        selected.alert = selected.alert.trim().to_owned();
        let previous = self.current.audio_config.clone();
        if previous == selected {
            return Ok(());
        }
        self.refresh_audio()?;
        let nodes = &self.current.audio_nodes;
        if selected.alert != previous.alert {
            validate_audio_node(nodes, NodeKind::Output, &selected.alert)?;
        }
        // Resolve both directions before changing either one.
        let mut changes = Vec::new();
        for (command, kind, selection, old) in [
            (
                "auplay",
                NodeKind::Output,
                &selected.output,
                &previous.output,
            ),
            ("ausrc", NodeKind::Input, &selected.input, &previous.input),
        ] {
            if selection == old {
                continue;
            }
            validate_audio_node(nodes, kind, selection)?;
            changes.push(AudioChange {
                command,
                device: live_audio_device(nodes, kind, selection)?,
                previous: live_audio_device(nodes, kind, old).ok(),
                selection: selection.clone(),
                previous_selection: old.clone(),
            });
        }
        for (index, change) in changes.iter().enumerate() {
            if let Err(error) = self.audio_command(change.command, &change.device) {
                let mut errors = vec![error];
                for applied in &changes[..index] {
                    match &applied.previous {
                        None => errors.push(anyhow!(
                            "session: restore {}: no previous device",
                            applied.command
                        )),
                        Some(device) => match self.audio_command(applied.command, device) {
                            Ok(()) => self
                                .note_audio_command(applied.command, &applied.previous_selection),
                            Err(error) => {
                                errors.push(error.context("session: restore previous device"))
                            }
                        },
                    }
                }
                return combine_errors(errors);
            }
            self.note_audio_command(change.command, &change.selection);
        }
        self.store
            .save_audio(&selected)
            .context("session: audio changed for this process but was not saved")?;
        self.current.audio_config = selected;
        self.refresh_output_volume()
    }

    fn audio_command(&mut self, command: &str, device: &str) -> Result<()> {
        let response = self
            .backend()?
            .command(command, &format!("pipewire,{device}"))
            .with_context(|| format!("session: apply {command}"))?;
        check_audio_response(&response).with_context(|| format!("session: apply {command}"))
    }

    fn note_audio_command(&mut self, command: &str, selection: &str) {
        // auplay changes both call output and the live ringtone device.
        if command == "auplay" {
            self.ringtone = selection.to_owned();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.backend.is_some() {
            let _ = self.stop(true);
        }
    }
}

fn translate_event(raw: BackendEvent) -> Option<domain::Event> {
    use domain::{CallEventType, Event, RegisterEventType};
    let call_kind = match raw.kind.as_str() {
        "CALL_INCOMING" => Some(CallEventType::Incoming),
        "CALL_ESTABLISHED" => Some(CallEventType::Established),
        "CALL_CLOSED" => Some(CallEventType::Closed),
        _ => None,
    };
    if let Some(kind) = call_kind {
        return Some(Event::Call {
            kind,
            id: raw.id,
            peer_uri: raw.peer_uri,
            peer_display_name: raw.peer_display_name,
        });
    }
    let kind = match raw.kind.as_str() {
        "REGISTER_OK" => RegisterEventType::Ok,
        "REGISTER_FAIL" => RegisterEventType::Fail,
        "UNREGISTERING" => RegisterEventType::Unregistering,
        _ => return None,
    };
    Some(Event::Register {
        kind,
        account_aor: raw.account_aor,
        detail: raw.detail,
    })
}

fn closed_event(id: &str) -> domain::Event {
    domain::Event::Call {
        kind: domain::CallEventType::Closed,
        id: id.to_owned(),
        peer_uri: String::new(),
        peer_display_name: String::new(),
    }
}

fn notification_summary(kind: domain::NotificationKind) -> &'static str {
    match kind {
        domain::NotificationKind::Incoming => "Incoming call",
        domain::NotificationKind::Missed => "Missed call",
        domain::NotificationKind::RejectedDnd => "Call rejected by do not disturb",
        domain::NotificationKind::RejectedBusy => "Call rejected while busy",
    }
}

fn check_call_response(response: &str) -> Result<()> {
    let error = domain::parse_call_command_error(response);
    if error.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(error))
    }
}

fn check_audio_response(response: &str) -> Result<()> {
    let error = domain::parse_audio_command_error(response);
    if error.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(error))
    }
}

fn merge_history(
    current: &[HistoryEntry],
    added: Vec<HistoryEntry>,
) -> (Vec<HistoryEntry>, Vec<anyhow::Error>) {
    let mut history = Vec::new();
    let mut errors = Vec::new();
    for mut entry in added.into_iter().chain(current.iter().cloned()) {
        entry.peer = domain::clamp_text(&entry.peer, domain::MAX_PEER_DISPLAY_LENGTH);
        if let Err(error) = crate::storage::validate_history_entry(&entry) {
            errors.push(error.context("session: discarded invalid call history entry"));
            continue;
        }
        history.push(entry);
        if history.len() == crate::storage::MAX_CALL_HISTORY_ENTRIES {
            break;
        }
    }
    (history, errors)
}

struct AudioChange {
    command: &'static str,
    device: String,
    previous: Option<String>,
    selection: String,
    previous_selection: String,
}

fn ringtone_selection(config: &AudioConfig) -> &str {
    if config.alert.is_empty() {
        &config.output
    } else {
        &config.alert
    }
}

fn validate_audio_node(nodes: &[Node], kind: NodeKind, selected: &str) -> Result<()> {
    if selected.is_empty()
        || nodes
            .iter()
            .any(|node| node.kind == kind && node.name == selected)
    {
        Ok(())
    } else {
        bail!("session: selected audio node is not available: {kind:?} {selected:?}")
    }
}

fn live_audio_device(nodes: &[Node], kind: NodeKind, selected: &str) -> Result<String> {
    if !selected.is_empty() {
        return Ok(selected.to_owned());
    }
    nodes
        .iter()
        .find(|node| node.kind == kind && node.is_default)
        .map(|node| node.name.clone())
        .ok_or_else(|| {
            anyhow!("session: selected audio node is not available: no system default for {kind:?}")
        })
}

fn combine_errors(errors: Vec<anyhow::Error>) -> Result<()> {
    if errors.is_empty() {
        return Ok(());
    }
    Err(anyhow!(
        errors
            .iter()
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>()
            .join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Condvar};

    #[derive(Default)]
    struct Gate {
        entered: AtomicBool,
        released: Mutex<bool>,
        wake: Condvar,
    }

    impl Gate {
        fn block(&self) {
            self.entered.store(true, Ordering::Release);
            let (released, timeout) = self
                .wake
                .wait_timeout_while(
                    self.released.lock().unwrap(),
                    Duration::from_secs(5),
                    |released| !*released,
                )
                .unwrap();
            assert!(
                *released && !timeout.timed_out(),
                "test gate was not released"
            );
        }

        fn wait_entered(&self) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.entered.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "test gate was never entered");
                thread::sleep(Duration::from_millis(2));
            }
        }

        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.wake.notify_all();
        }
    }

    #[derive(Default)]
    struct BackendState {
        commands: Vec<String>,
        effects: Vec<String>,
        events: Vec<BackendEvent>,
        failures: HashMap<String, String>,
        responses: HashMap<String, String>,
        terminal: Option<String>,
        nodes: Vec<Node>,
        starts: usize,
        stops: usize,
        fail_start: bool,
        fail_notification: bool,
        fail_focus: bool,
        fail_pause: bool,
        async_effects: bool,
        focus_gate: Option<Arc<Gate>>,
        pause_gate: Option<Arc<Gate>>,
        notify_gate: Option<Arc<Gate>>,
        command_gate: Option<Arc<Gate>>,
        output_volume: Option<f32>,
        volume_reads: Vec<String>,
        volume_sets: Vec<(String, f32)>,
    }

    struct FakeBackend(Arc<Mutex<BackendState>>);

    impl Backend for FakeBackend {
        fn command(&mut self, command: &str, params: &str) -> Result<String> {
            let gate = self.0.lock().unwrap().command_gate.clone();
            if let Some(gate) = gate {
                gate.block();
            }
            let mut state = self.0.lock().unwrap();
            let line = if params.is_empty() {
                command.to_owned()
            } else {
                format!("{command} {params}")
            };
            state.commands.push(line.clone());
            state.effects.push(line);
            if let Some(error) = state.failures.get(command) {
                bail!("{error}");
            }
            if let Some(response) = state.responses.get(command) {
                return Ok(response.clone());
            }
            Ok(if command == "reginfo" {
                "User Agents (1)\n<sip:100@example.com> OK".into()
            } else {
                String::new()
            })
        }

        fn poll_events(&mut self) -> Result<Vec<BackendEvent>> {
            let mut state = self.0.lock().unwrap();
            if let Some(error) = &state.terminal {
                bail!("{error}");
            }
            Ok(std::mem::take(&mut state.events))
        }

        fn discover_audio(&mut self) -> Result<Vec<Node>> {
            Ok(self.0.lock().unwrap().nodes.clone())
        }

        fn notify(&mut self, summary: &str, body: &str) -> Result<()> {
            let gate = self.0.lock().unwrap().notify_gate.clone();
            if let Some(gate) = gate {
                gate.block();
            }
            let mut state = self.0.lock().unwrap();
            state.effects.push(format!("notify {summary}: {body}"));
            if state.fail_notification {
                bail!("notify-send failed");
            }
            Ok(())
        }

        fn pause_media(&mut self) -> Result<()> {
            let gate = self.0.lock().unwrap().pause_gate.clone();
            if let Some(gate) = gate {
                gate.block();
            }
            let mut state = self.0.lock().unwrap();
            state.effects.push("pause".into());
            if state.fail_pause {
                bail!("media failed");
            }
            Ok(())
        }

        fn focus(&mut self) -> Result<()> {
            let gate = self.0.lock().unwrap().focus_gate.clone();
            if let Some(gate) = gate {
                gate.block();
            }
            let mut state = self.0.lock().unwrap();
            state.effects.push("focus".into());
            if state.fail_focus {
                bail!("focus failed");
            }
            Ok(())
        }

        fn desktop_effects(&self) -> Option<Box<dyn DesktopEffects>> {
            self.0
                .lock()
                .unwrap()
                .async_effects
                .then(|| Box::new(FakeDesktop(self.0.clone())) as Box<dyn DesktopEffects>)
        }

        fn output_volume(&mut self, output: &str) -> Result<Option<f32>> {
            let mut state = self.0.lock().unwrap();
            state.volume_reads.push(output.to_owned());
            Ok(state.output_volume)
        }

        fn set_output_volume(&mut self, output: &str, volume: f32) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            ensure!(state.output_volume.is_some(), "test: output unavailable");
            state.volume_sets.push((output.to_owned(), volume));
            state.output_volume = Some(volume);
            Ok(())
        }

        fn shutdown(&mut self) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            state.stops += 1;
            state.effects.push("shutdown".into());
            state.events.clear();
            Ok(())
        }
    }

    struct FakeDesktop(Arc<Mutex<BackendState>>);

    impl DesktopEffects for FakeDesktop {
        fn focus(&mut self) -> Result<()> {
            FakeBackend(self.0.clone()).focus()
        }
        fn pause_media(&mut self) -> Result<()> {
            FakeBackend(self.0.clone()).pause_media()
        }
        fn notify(&mut self, summary: &str, body: &str) -> Result<()> {
            FakeBackend(self.0.clone()).notify(summary, body)
        }
    }

    #[derive(Default)]
    struct MemoryState {
        contacts: Vec<Contact>,
        audio: AudioConfig,
        account: Account,
        history: Vec<HistoryEntry>,
        account_writes: usize,
        history_writes: usize,
        fail_audio: bool,
        fail_history: bool,
        fail_load: bool,
    }

    struct MemoryStore(Arc<Mutex<MemoryState>>);

    impl SessionStore for MemoryStore {
        fn contacts(&mut self) -> Result<Vec<Contact>> {
            let state = self.0.lock().unwrap();
            if state.fail_load {
                bail!("contacts unreadable");
            }
            Ok(state.contacts.clone())
        }
        fn add_contact(&mut self, contact: Contact) -> Result<()> {
            self.0.lock().unwrap().contacts.push(contact);
            Ok(())
        }
        fn remove_contact(&mut self, uri: &str) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            let index = state
                .contacts
                .iter()
                .position(|contact| contact.uri == uri)
                .ok_or_else(|| anyhow!("contact not found"))?;
            state.contacts.remove(index);
            Ok(())
        }
        fn audio(&mut self) -> Result<AudioConfig> {
            Ok(self.0.lock().unwrap().audio.clone())
        }
        fn save_audio(&mut self, audio: &AudioConfig) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            if state.fail_audio {
                bail!("disk full");
            }
            state.audio = audio.clone();
            Ok(())
        }
        fn account(&mut self) -> Result<Account> {
            Ok(self.0.lock().unwrap().account.clone())
        }
        fn save_account(&mut self, credentials: AccountCredentials) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            state.account_writes += 1;
            state.account.username = credentials.username;
            state.account.server = credentials.server;
            state.account.domain = credentials.domain;
            state.account.login = credentials.login;
            state.account.has_password |= !credentials.password.is_empty();
            state.account.configured = true;
            Ok(())
        }
        fn history(&mut self) -> Result<Vec<HistoryEntry>> {
            Ok(self.0.lock().unwrap().history.clone())
        }
        fn save_history(&mut self, history: &[HistoryEntry]) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            state.history_writes += 1;
            if state.fail_history {
                bail!("history disk full");
            }
            state.history = history.to_vec();
            Ok(())
        }
    }

    fn at() -> DateTime<Utc> {
        "2026-02-03T04:05:06Z".parse().unwrap()
    }

    fn nodes() -> Vec<Node> {
        [
            ("sink.old", NodeKind::Output, true),
            ("sink.new", NodeKind::Output, false),
            ("sink.third", NodeKind::Output, false),
            ("source.old", NodeKind::Input, true),
            ("source.new", NodeKind::Input, false),
        ]
        .into_iter()
        .map(|(name, kind, is_default)| Node {
            name: name.into(),
            description: name.into(),
            kind,
            is_default,
        })
        .collect()
    }

    fn factory(state: Arc<Mutex<BackendState>>) -> BackendFactory {
        Box::new(move |_| {
            let mut inner = state.lock().unwrap();
            inner.starts += 1;
            if inner.fail_start {
                bail!("foreign owner");
            }
            drop(inner);
            Ok(Box::new(FakeBackend(state.clone())))
        })
    }

    struct Harness {
        session: SessionHandle,
        backend: Arc<Mutex<BackendState>>,
        store: Arc<Mutex<MemoryState>>,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_memory(MemoryState::default())
        }
        fn with_memory(memory: MemoryState) -> Self {
            Self::with_backend(memory, BackendState::default())
        }
        fn with_backend(memory: MemoryState, backend: BackendState) -> Self {
            let backend = Arc::new(Mutex::new(BackendState {
                nodes: nodes(),
                ..backend
            }));
            let store = Arc::new(Mutex::new(memory));
            let session = SessionHandle::start_with_dependencies(
                Config::new("/unused-session-test"),
                factory(backend.clone()),
                Box::new(MemoryStore(store.clone())),
                Box::new(at),
            )
            .unwrap();
            Self {
                session,
                backend,
                store,
            }
        }
        fn action(&self, action: Action) -> Result<()> {
            self.session.dispatch_wait(action)
        }
        fn event(&self, kind: &str, id: &str, peer: &str) {
            self.backend.lock().unwrap().events.push(BackendEvent {
                kind: kind.into(),
                id: id.into(),
                peer_uri: peer.into(),
                ..Default::default()
            });
        }
        fn wait(&self, predicate: impl Fn(&Snapshot) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !predicate(&self.session.snapshot()) {
                assert!(
                    Instant::now() < deadline,
                    "snapshot condition timed out: {:?}",
                    self.session.snapshot()
                );
                thread::sleep(Duration::from_millis(2));
            }
        }
        fn audio_commands(&self) -> Vec<String> {
            self.backend
                .lock()
                .unwrap()
                .commands
                .iter()
                .filter(|line| line.starts_with("auplay ") || line.starts_with("ausrc "))
                .cloned()
                .collect()
        }
    }

    #[test]
    fn ringtone_selection_saves_without_command_and_can_be_reverted() {
        let h = Harness::new();
        h.action(Action::SetAudio {
            field: AudioField::Ringtone,
            name: "sink.new".into(),
        })
        .unwrap();
        assert!(h.audio_commands().is_empty());
        assert!(h.session.snapshot().ringtone_restart_required);
        assert_eq!(h.store.lock().unwrap().audio.alert, "sink.new");
        h.action(Action::SetAudio {
            field: AudioField::Ringtone,
            name: String::new(),
        })
        .unwrap();
        assert!(!h.session.snapshot().ringtone_restart_required);
    }

    #[test]
    fn output_moves_live_ringtone_but_input_does_not() {
        let h = Harness::with_memory(MemoryState {
            audio: AudioConfig {
                output: "sink.old".into(),
                alert: "sink.new".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(!h.session.snapshot().ringtone_restart_required);
        h.action(Action::SetAudio {
            field: AudioField::Input,
            name: "source.new".into(),
        })
        .unwrap();
        assert!(!h.session.snapshot().ringtone_restart_required);
        h.action(Action::SetAudio {
            field: AudioField::Output,
            name: "sink.third".into(),
        })
        .unwrap();
        assert!(h.session.snapshot().ringtone_restart_required);
        h.action(Action::SetAudio {
            field: AudioField::Output,
            name: "sink.new".into(),
        })
        .unwrap();
        assert!(!h.session.snapshot().ringtone_restart_required);
    }

    #[test]
    fn default_output_and_input_selections_do_not_revert_each_other() {
        let h = Harness::new();
        thread::scope(|scope| {
            scope.spawn(|| {
                h.action(Action::SetAudio {
                    field: AudioField::Output,
                    name: "sink.new".into(),
                })
                .unwrap()
            });
            scope.spawn(|| {
                h.action(Action::SetAudio {
                    field: AudioField::Input,
                    name: "source.new".into(),
                })
                .unwrap()
            });
        });
        let snapshot = h.session.snapshot();
        assert_eq!(snapshot.audio_config.output, "sink.new");
        assert_eq!(snapshot.audio_config.input, "source.new");
        assert!(!snapshot.ringtone_restart_required);
        h.action(Action::SetAudio {
            field: AudioField::Output,
            name: String::new(),
        })
        .unwrap();
        assert_eq!(h.session.snapshot().audio_config.input, "source.new");
        assert!(
            h.audio_commands()
                .contains(&"auplay pipewire,sink.old".to_owned())
        );
    }

    #[test]
    fn missing_or_wrong_kind_audio_node_sends_no_command() {
        let h = Harness::new();
        for name in ["absent", "source.new"] {
            assert!(
                h.action(Action::SetAudio {
                    field: AudioField::Ringtone,
                    name: name.into()
                })
                .is_err()
            );
        }
        assert!(h.audio_commands().is_empty());
        assert_eq!(h.session.snapshot().audio_config, AudioConfig::default());
    }

    #[test]
    fn failed_audio_persistence_reports_live_ringtone_difference() {
        let h = Harness::new();
        h.store.lock().unwrap().fail_audio = true;
        let error = h
            .action(Action::SetAudio {
                field: AudioField::Output,
                name: "sink.new".into(),
            })
            .unwrap_err();
        assert!(format!("{error:#}").contains("was not saved"));
        assert_eq!(h.session.snapshot().audio_config, AudioConfig::default());
        assert!(h.session.snapshot().ringtone_restart_required);
    }

    #[test]
    fn startup_failure_does_not_issue_commands() {
        let backend = Arc::new(Mutex::new(BackendState {
            fail_start: true,
            ..Default::default()
        }));
        let result = SessionHandle::start_with_dependencies(
            Config::new("/unused-session-test"),
            factory(backend.clone()),
            Box::new(MemoryStore(Arc::new(Mutex::new(MemoryState::default())))),
            Box::new(at),
        );
        assert!(result.is_err());
        assert!(backend.lock().unwrap().commands.is_empty());
    }

    #[test]
    fn persisted_load_failure_prevents_backend_start() {
        let backend = Arc::new(Mutex::new(BackendState::default()));
        let result = SessionHandle::start_with_dependencies(
            Config::new("/unused-session-test"),
            factory(backend.clone()),
            Box::new(MemoryStore(Arc::new(Mutex::new(MemoryState {
                fail_load: true,
                ..Default::default()
            })))),
            Box::new(at),
        );
        assert!(result.is_err());
        assert_eq!(backend.lock().unwrap().starts, 0);
    }

    #[test]
    fn snapshots_are_copies_and_clock_updates_are_published() {
        let h = Harness::with_memory(MemoryState {
            contacts: vec![Contact {
                name: "Alice".into(),
                uri: "sip:alice@example.com".into(),
            }],
            ..Default::default()
        });
        let mut copy = h.session.snapshot();
        copy.contacts[0].name = "changed".into();
        assert_eq!(h.session.snapshot().contacts[0].name, "Alice");
        assert_eq!(copy.now, at());
        h.wait(|snapshot| snapshot.revision > copy.revision);
    }

    fn worker_harness(
        memory: MemoryState,
    ) -> (Worker, Arc<Mutex<BackendState>>, Arc<Mutex<MemoryState>>) {
        let backend = Arc::new(Mutex::new(BackendState {
            nodes: nodes(),
            ..Default::default()
        }));
        let store = Arc::new(Mutex::new(memory));
        let worker = Worker::start(
            Config::new("/unused-session-test"),
            factory(backend.clone()),
            Box::new(MemoryStore(store.clone())),
            Box::new(at),
            Arc::new(Mutex::new(Snapshot::empty(at()))),
        )
        .unwrap();
        (worker, backend, store)
    }

    fn credentials() -> AccountCredentials {
        AccountCredentials {
            server: "example.com".into(),
            username: "200".into(),
            password: "test-only-secret".into(),
            ..Default::default()
        }
    }

    #[test]
    fn audio_validates_all_changes_before_first_command() {
        let (mut worker, backend, _) = worker_harness(MemoryState::default());
        let result = worker.select_audio(AudioConfig {
            output: "sink.new".into(),
            input: "absent".into(),
            ..Default::default()
        });
        assert!(result.is_err());
        assert_eq!(backend.lock().unwrap().commands, ["reginfo"]);
        worker.stop(true).unwrap();
    }

    #[test]
    fn audio_rolls_back_output_when_input_command_fails() {
        let (mut worker, backend, store) = worker_harness(MemoryState::default());
        backend
            .lock()
            .unwrap()
            .failures
            .insert("ausrc".into(), "ausrc failed".into());
        assert!(
            worker
                .select_audio(AudioConfig {
                    output: "sink.new".into(),
                    input: "source.new".into(),
                    ..Default::default()
                })
                .is_err()
        );
        assert_eq!(
            backend.lock().unwrap().commands,
            [
                "reginfo",
                "auplay pipewire,sink.new",
                "ausrc pipewire,source.new",
                "auplay pipewire,sink.old",
            ]
        );
        assert_eq!(store.lock().unwrap().audio, AudioConfig::default());
        assert_eq!(worker.current.audio_config, AudioConfig::default());
        assert_eq!(worker.ringtone, "");
        worker.stop(true).unwrap();
    }

    #[test]
    fn rollback_auplay_also_moves_a_separate_ringtone() {
        let (mut worker, backend, _) = worker_harness(MemoryState {
            audio: AudioConfig {
                output: "sink.old".into(),
                alert: "sink.third".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        backend
            .lock()
            .unwrap()
            .failures
            .insert("ausrc".into(), "ausrc failed".into());
        assert!(
            worker
                .select_audio(AudioConfig {
                    output: "sink.new".into(),
                    input: "source.new".into(),
                    alert: "sink.third".into()
                })
                .is_err()
        );
        worker.publish();
        assert_eq!(worker.current.audio_config.output, "sink.old");
        assert!(worker.current.ringtone_restart_required);
        worker.stop(true).unwrap();
    }

    #[test]
    fn failed_dial_records_history_without_committing_state() {
        let h = Harness::with_memory(MemoryState {
            contacts: vec![Contact {
                name: "Alice".into(),
                uri: "sip:alice@example.com".into(),
            }],
            ..Default::default()
        });
        h.backend
            .lock()
            .unwrap()
            .failures
            .insert("dial".into(), "dial rejected".into());
        assert!(
            h.action(Action::Dial("sip:alice@example.com".into()))
                .is_err()
        );
        let snapshot = h.session.snapshot();
        assert_eq!(snapshot.state.call_state, domain::CallState::Idle);
        assert_eq!(snapshot.history.len(), 1);
        assert_eq!(snapshot.history[0].peer, "Alice");
        assert_eq!(
            snapshot.history[0].outcome,
            domain::CallOutcome::NotConnected
        );
        assert_eq!(snapshot.history[0].started_at, Some(at()));
        assert_eq!(snapshot.history[0].ended_at, Some(at()));
    }

    #[test]
    fn outgoing_call_keeps_original_redial_target_and_rolls_back_failed_mute() {
        let h = Harness::new();
        h.action(Action::Dial("123 45".into())).unwrap();
        assert_eq!(
            h.session.snapshot().state.call_state,
            domain::CallState::Outgoing
        );
        h.event("CALL_ESTABLISHED", "out-1", "sip:12345@example.com");
        h.wait(|snapshot| snapshot.state.call_state == domain::CallState::Active);
        h.backend
            .lock()
            .unwrap()
            .failures
            .insert("mute".into(), "mute refused".into());
        assert!(h.action(Action::ToggleMute).is_err());
        assert!(!h.session.snapshot().state.muted);
        h.backend.lock().unwrap().failures.remove("mute");
        h.action(Action::ToggleMute).unwrap();
        assert!(h.session.snapshot().state.muted);
        h.event("CALL_CLOSED", "out-1", "");
        h.wait(|snapshot| snapshot.history.len() == 1);
        assert_eq!(h.session.snapshot().history[0].target, "12345");
        assert_eq!(
            h.session.snapshot().history[0].outcome,
            domain::CallOutcome::Connected
        );
    }

    #[test]
    fn incoming_focus_and_pause_survive_notification_failure_without_resuming() {
        let h = Harness::new();
        h.backend.lock().unwrap().fail_notification = true;
        h.event("CALL_INCOMING", "in-1", "sip:alice@example.com");
        h.wait(|snapshot| {
            snapshot.state.call_state == domain::CallState::Incoming
                && snapshot.last_error.contains("notify-send failed")
        });
        let effects = h.backend.lock().unwrap().effects.clone();
        let focus = effects.iter().position(|effect| effect == "focus").unwrap();
        let pause = effects.iter().position(|effect| effect == "pause").unwrap();
        let notify = effects
            .iter()
            .position(|effect| effect.starts_with("notify Incoming call:"))
            .unwrap();
        assert!(focus < notify && pause < notify);
        h.event("CALL_CLOSED", "in-1", "");
        h.wait(|snapshot| snapshot.history.len() == 1);
        let effects = h.backend.lock().unwrap().effects.clone();
        assert_eq!(
            effects.iter().filter(|effect| *effect == "pause").count(),
            1
        );
        assert_eq!(
            effects.iter().filter(|effect| *effect == "focus").count(),
            1
        );
        assert_eq!(
            h.session.snapshot().history[0].outcome,
            domain::CallOutcome::Missed
        );
    }

    #[test]
    fn dnd_rejection_does_not_focus_or_pause_media() {
        let h = Harness::new();
        h.action(Action::ToggleDnd).unwrap();
        h.event("CALL_INCOMING", "dnd-1", "sip:alice@example.com");
        h.wait(|snapshot| snapshot.history.len() == 1);
        let backend = h.backend.lock().unwrap();
        assert!(backend.commands.contains(&"hangup".into()));
        assert!(!backend.effects.contains(&"focus".into()));
        assert!(!backend.effects.contains(&"pause".into()));
        assert_eq!(
            h.session.snapshot().state.call_state,
            domain::CallState::Idle
        );
        assert_eq!(
            h.session.snapshot().history[0].outcome,
            domain::CallOutcome::RejectedDnd
        );
    }

    #[test]
    fn busy_rejection_targets_only_second_call() {
        let h = Harness::new();
        h.event("CALL_INCOMING", "first", "sip:first@example.com");
        h.wait(|snapshot| snapshot.state.call_id == "first");
        h.event("CALL_INCOMING", "second", "sip:second@example.com");
        h.wait(|snapshot| snapshot.history.len() == 1);
        assert_eq!(h.session.snapshot().state.call_id, "first");
        assert_eq!(
            h.session.snapshot().history[0].outcome,
            domain::CallOutcome::RejectedBusy
        );
        assert!(
            h.backend
                .lock()
                .unwrap()
                .commands
                .contains(&"hangup second".into())
        );
    }

    #[test]
    fn reject_uses_global_decline_and_refusal_preserves_ringing_state() {
        let h = Harness::new();
        h.event("CALL_INCOMING", "reject-1", "sip:alice@example.com");
        h.wait(|snapshot| snapshot.state.call_state == domain::CallState::Incoming);
        h.backend
            .lock()
            .unwrap()
            .responses
            .insert("hangup".into(), "no active call\n".into());
        assert!(h.action(Action::Reject).is_err());
        assert_eq!(
            h.session.snapshot().state.call_state,
            domain::CallState::Incoming
        );
        assert!(!h.session.snapshot().state.end_requested);
        assert!(
            h.backend
                .lock()
                .unwrap()
                .commands
                .contains(&"hangup scode=603 reason=Decline".into())
        );
        h.backend.lock().unwrap().responses.remove("hangup");
        h.action(Action::Reject).unwrap();
        h.event("CALL_CLOSED", "reject-1", "");
        h.wait(|snapshot| snapshot.history.len() == 1);
        assert_eq!(
            h.session.snapshot().history[0].outcome,
            domain::CallOutcome::Rejected
        );
    }

    #[test]
    fn account_changes_require_idle_and_restart_preserves_dnd_and_resets_ringtone() {
        let h = Harness::new();
        h.event("CALL_INCOMING", "account-1", "sip:alice@example.com");
        h.wait(|snapshot| snapshot.state.call_state == domain::CallState::Incoming);
        assert!(h.action(Action::SaveAccount(credentials())).is_err());
        assert_eq!(h.store.lock().unwrap().account_writes, 0);
        h.event("CALL_CLOSED", "account-1", "");
        h.wait(|snapshot| snapshot.state.call_state == domain::CallState::Idle);
        h.action(Action::ToggleDnd).unwrap();
        h.action(Action::SetAudio {
            field: AudioField::Ringtone,
            name: "sink.new".into(),
        })
        .unwrap();
        assert!(h.session.snapshot().ringtone_restart_required);
        h.action(Action::SaveAccount(credentials())).unwrap();
        let snapshot = h.session.snapshot();
        assert_eq!(snapshot.account.username, "200");
        assert!(snapshot.state.dnd && snapshot.state.registered && snapshot.account.has_password);
        assert!(!snapshot.ringtone_restart_required);
        assert!(!format!("{snapshot:?}").contains("test-only-secret"));
        assert_eq!(h.backend.lock().unwrap().starts, 2);
        assert_eq!(h.backend.lock().unwrap().stops, 1);
    }

    #[test]
    fn account_restart_failure_is_terminal_and_cannot_keep_stale_registration() {
        let h = Harness::new();
        h.backend.lock().unwrap().fail_start = true;
        assert!(h.action(Action::SaveAccount(credentials())).is_err());
        let snapshot = h.session.snapshot();
        assert!(!snapshot.running && !snapshot.state.registered);
        assert!(
            snapshot
                .last_error
                .contains("account was written but baresip restart failed")
        );
        assert_eq!(h.backend.lock().unwrap().stops, 1);
    }

    #[test]
    fn real_store_keeps_password_when_account_form_submits_blank() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new(directory.path().to_path_buf());
        store.ensure_config().unwrap();
        Store::save_account(&store, &credentials()).unwrap();
        let backend = Arc::new(Mutex::new(BackendState::default()));
        let mut session = SessionHandle::start_with_factory(
            Config::new(directory.path()),
            factory(backend.clone()),
            Box::new(at),
        )
        .unwrap();
        let mut updated = credentials();
        updated.username = "300".into();
        updated.password.clear();
        session.dispatch_wait(Action::SaveAccount(updated)).unwrap();
        assert!(session.snapshot().account.has_password);
        let accounts = std::fs::read_to_string(directory.path().join("accounts")).unwrap();
        assert!(accounts.contains("auth_pass=test-only-secret"));
        assert!(accounts.contains("sip:300@example.com"));
        session.shutdown().unwrap();
        assert_eq!(backend.lock().unwrap().stops, 2);
    }

    #[test]
    fn shutdown_hangs_up_before_backend_stop_and_records_call_once() {
        let mut h = Harness::new();
        h.event("CALL_INCOMING", "stop-1", "sip:alice@example.com");
        h.wait(|snapshot| snapshot.state.call_state == domain::CallState::Incoming);
        h.session.shutdown().unwrap();
        h.session.shutdown().unwrap();
        let backend = h.backend.lock().unwrap();
        let hangup = backend
            .effects
            .iter()
            .position(|effect| effect == "hangup")
            .unwrap();
        let stop = backend
            .effects
            .iter()
            .position(|effect| effect == "shutdown")
            .unwrap();
        assert!(hangup < stop);
        assert_eq!(backend.stops, 1);
        let snapshot = h.session.snapshot();
        assert!(!snapshot.running);
        assert_eq!(snapshot.history.len(), 1);
        assert_eq!(snapshot.history[0].outcome, domain::CallOutcome::Rejected);
    }

    #[test]
    fn unexpected_backend_exit_finalizes_without_requesting_hangup() {
        let h = Harness::new();
        h.action(Action::Dial("123".into())).unwrap();
        h.backend.lock().unwrap().terminal = Some("child exited unexpectedly".into());
        h.wait(|snapshot| {
            !snapshot.running && snapshot.last_error.contains("child exited unexpectedly")
        });
        assert_eq!(
            h.session.snapshot().history[0].outcome,
            domain::CallOutcome::NotConnected
        );
        assert!(
            !h.backend
                .lock()
                .unwrap()
                .commands
                .contains(&"hangup".into())
        );
    }

    #[test]
    fn history_write_failure_does_not_rollback_signaling_state() {
        let h = Harness::new();
        h.store.lock().unwrap().fail_history = true;
        h.event("CALL_INCOMING", "history-1", "sip:alice@example.com");
        h.wait(|snapshot| snapshot.state.call_state == domain::CallState::Incoming);
        h.event("CALL_CLOSED", "history-1", "");
        h.wait(|snapshot| {
            snapshot.history.len() == 1 && snapshot.last_error.contains("history disk full")
        });
        assert_eq!(
            h.session.snapshot().state.call_state,
            domain::CallState::Idle
        );
    }

    #[test]
    fn oversized_incoming_target_never_poison_history_or_later_calls() {
        for mode in ["missed", "connected", "rejected", "dnd", "busy", "shutdown"] {
            let (mut worker, _, store) = worker_harness(MemoryState::default());
            if mode == "dnd" {
                worker.handle_action(Action::ToggleDnd).unwrap();
            }
            if mode == "busy" {
                worker
                    .handle_event(BackendEvent {
                        kind: "CALL_INCOMING".into(),
                        id: "first".into(),
                        peer_uri: "sip:first@example.com".into(),
                        ..Default::default()
                    })
                    .unwrap();
            }
            let oversized = format!("sip:{}@example.com", "a".repeat(4096));
            let mut result = worker.handle_event(BackendEvent {
                kind: "CALL_INCOMING".into(),
                id: "bad".into(),
                peer_uri: oversized.clone(),
                ..Default::default()
            });
            if !matches!(mode, "dnd" | "busy") {
                result.unwrap();
                assert_eq!(worker.current.state.call_target, oversized);
                assert!(
                    worker.current.state.peer.chars().count() <= domain::MAX_PEER_DISPLAY_LENGTH
                );
                if mode == "connected" {
                    worker
                        .handle_event(BackendEvent {
                            kind: "CALL_ESTABLISHED".into(),
                            id: "bad".into(),
                            ..Default::default()
                        })
                        .unwrap();
                }
                if mode == "rejected" {
                    worker.handle_action(Action::Reject).unwrap();
                }
                result = if mode == "shutdown" {
                    worker.finalize_call(true)
                } else {
                    worker.handle_event(BackendEvent {
                        kind: "CALL_CLOSED".into(),
                        id: "bad".into(),
                        ..Default::default()
                    })
                };
            }
            assert!(
                format!("{:#}", result.unwrap_err()).contains("discarded invalid call history"),
                "{mode}"
            );
            assert!(worker.current.history.is_empty());
            assert!(store.lock().unwrap().history.is_empty());
            if mode == "busy" {
                assert_eq!(worker.current.state.call_id, "first");
                worker
                    .handle_event(BackendEvent {
                        kind: "CALL_CLOSED".into(),
                        id: "first".into(),
                        ..Default::default()
                    })
                    .unwrap();
            }
            if mode == "dnd" {
                worker.handle_action(Action::ToggleDnd).unwrap();
            }
            let before = worker.current.history.len();
            worker
                .handle_event(BackendEvent {
                    kind: "CALL_INCOMING".into(),
                    id: "valid".into(),
                    peer_uri: "sip:valid@example.com".into(),
                    ..Default::default()
                })
                .unwrap();
            worker
                .handle_event(BackendEvent {
                    kind: "CALL_CLOSED".into(),
                    id: "valid".into(),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(worker.current.history.len(), before + 1);
            assert_eq!(worker.current.history[0].target, "sip:valid@example.com");
            worker.stop(true).unwrap();
        }
    }

    #[test]
    fn history_merge_drops_invalid_entries_and_preserves_unicode_labels() {
        let valid = HistoryEntry {
            direction: domain::CallDirection::Incoming,
            outcome: domain::CallOutcome::Missed,
            peer: "😀".repeat(domain::MAX_PEER_DISPLAY_LENGTH),
            target: "sip:alice@example.com".into(),
            started_at: Some(at()),
            ended_at: Some(at()),
            ..Default::default()
        };
        for field in ["target", "timestamp", "direction", "outcome", "connected"] {
            let mut invalid = valid.clone();
            match field {
                "target" => invalid.target = "a".repeat(4096),
                "timestamp" => invalid.started_at = None,
                "direction" => invalid.direction = domain::CallDirection::Unknown,
                "outcome" => invalid.outcome = domain::CallOutcome::Unknown,
                _ => invalid.outcome = domain::CallOutcome::Connected,
            }
            let (history, errors) = merge_history(&[invalid.clone()], vec![invalid, valid.clone()]);
            assert_eq!(errors.len(), 2, "{field}");
            assert_eq!(history.as_slice(), std::slice::from_ref(&valid));
        }
        let (history, _) = merge_history(
            &[],
            vec![valid; crate::storage::MAX_CALL_HISTORY_ENTRIES + 1],
        );
        assert_eq!(history.len(), crate::storage::MAX_CALL_HISTORY_ENTRIES);
    }

    #[test]
    fn registration_snapshot_failure_is_visible_but_not_fatal() {
        let backend = Arc::new(Mutex::new(BackendState {
            responses: HashMap::from([("reginfo".into(), "unknown format".into())]),
            ..Default::default()
        }));
        let session = SessionHandle::start_with_dependencies(
            Config::new("/unused-session-test"),
            factory(backend),
            Box::new(MemoryStore(Arc::new(Mutex::new(MemoryState::default())))),
            Box::new(at),
        )
        .unwrap();
        let snapshot = session.snapshot();
        assert!(snapshot.running && !snapshot.state.registered);
        assert!(snapshot.last_error.contains("unrecognized reginfo"));
    }

    #[test]
    fn adding_contact_normalizes_extension_using_account_domain() {
        let h = Harness::with_memory(MemoryState {
            account: Account {
                domain: "example.com".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        h.action(Action::AddContact(Contact {
            name: "Alice".into(),
            uri: "123".into(),
        }))
        .unwrap();
        assert_eq!(h.session.snapshot().contacts[0].uri, "sip:123@example.com");
        h.action(Action::RemoveContact("sip:123@example.com".into()))
            .unwrap();
        assert!(h.session.snapshot().contacts.is_empty());
    }

    #[test]
    fn answer_waits_for_established_event_and_registration_events_update_state() {
        let h = Harness::new();
        h.event("CALL_INCOMING", "answer-1", "sip:alice@example.com");
        h.wait(|snapshot| snapshot.state.call_state == domain::CallState::Incoming);
        h.action(Action::Answer).unwrap();
        assert!(
            h.backend
                .lock()
                .unwrap()
                .commands
                .contains(&"accept".into())
        );
        assert_eq!(
            h.session.snapshot().state.call_state,
            domain::CallState::Incoming
        );
        h.event("CALL_ESTABLISHED", "answer-1", "");
        h.wait(|snapshot| snapshot.state.call_state == domain::CallState::Active);
        h.backend.lock().unwrap().events.push(BackendEvent {
            kind: "REGISTER_FAIL".into(),
            detail: "403 Forbidden".into(),
            ..Default::default()
        });
        h.wait(|snapshot| snapshot.state.registration == domain::RegistrationState::Failed);
        assert!(
            h.session
                .snapshot()
                .state
                .registration_detail
                .contains("403 Forbidden")
        );
        h.event("UNREGISTERING", "", "");
        h.wait(|snapshot| snapshot.state.registration == domain::RegistrationState::Unregistered);
    }

    #[test]
    fn output_volume_is_read_at_start_and_set_without_a_call() {
        let h = Harness::with_backend(
            MemoryState::default(),
            BackendState {
                output_volume: Some(0.7),
                ..BackendState::default()
            },
        );
        assert_eq!(h.session.snapshot().output_volume, Some(0.7));
        h.action(Action::SetOutputVolume(1.4)).unwrap();
        assert_eq!(h.session.snapshot().output_volume, Some(1.0));
        assert_eq!(
            h.backend.lock().unwrap().volume_sets,
            [(String::new(), 1.0)]
        );
        assert!(h.action(Action::SetOutputVolume(f32::NAN)).is_err());
    }

    #[test]
    fn output_volume_targets_the_selected_device_not_the_ringtone_device() {
        let h = Harness::with_backend(
            MemoryState::default(),
            BackendState {
                output_volume: Some(0.8),
                ..BackendState::default()
            },
        );
        h.action(Action::SetAudio {
            field: AudioField::Output,
            name: "sink.new".into(),
        })
        .unwrap();
        h.action(Action::SetOutputVolume(0.5)).unwrap();
        h.action(Action::SetAudio {
            field: AudioField::Ringtone,
            name: "sink.old".into(),
        })
        .unwrap();
        h.action(Action::SetOutputVolume(0.4)).unwrap();
        let state = h.backend.lock().unwrap();
        assert_eq!(
            state.volume_sets,
            [("sink.new".into(), 0.5), ("sink.new".into(), 0.4)]
        );
    }

    #[test]
    fn output_volume_tracks_system_changes_only_while_watched() {
        let h = Harness::with_backend(
            MemoryState::default(),
            BackendState {
                output_volume: Some(0.8),
                ..BackendState::default()
            },
        );
        h.action(Action::WatchOutputVolume(true)).unwrap();
        h.backend.lock().unwrap().output_volume = Some(0.5);
        h.wait(|snapshot| snapshot.output_volume == Some(0.5));
        h.action(Action::WatchOutputVolume(false)).unwrap();
        h.backend.lock().unwrap().output_volume = Some(0.6);
        thread::sleep(VOLUME_POLL + Duration::from_millis(300));
        assert_eq!(h.session.snapshot().output_volume, Some(0.5));
        h.action(Action::WatchOutputVolume(true)).unwrap();
        assert_eq!(h.session.snapshot().output_volume, Some(0.6));
    }

    #[test]
    fn refresh_audio_replaces_discovered_nodes_without_changing_selection() {
        let h = Harness::new();
        h.action(Action::SetAudio {
            field: AudioField::Output,
            name: "sink.new".into(),
        })
        .unwrap();
        h.backend
            .lock()
            .unwrap()
            .nodes
            .retain(|node| node.name != "sink.new");
        h.action(Action::RefreshAudio).unwrap();
        let snapshot = h.session.snapshot();
        assert!(
            !snapshot
                .audio_nodes
                .iter()
                .any(|node| node.name == "sink.new")
        );
        assert_eq!(snapshot.audio_config.output, "sink.new");
    }

    #[test]
    fn published_history_resolves_contacts_without_rewriting_captured_labels() {
        let entry = HistoryEntry {
            direction: domain::CallDirection::Incoming,
            outcome: domain::CallOutcome::Missed,
            peer: "Original caller".into(),
            target: "sip:alice@example.com".into(),
            started_at: Some(at()),
            ended_at: Some(at()),
            ..Default::default()
        };
        let h = Harness::with_memory(MemoryState {
            history: vec![entry.clone()],
            ..Default::default()
        });
        assert_eq!(
            h.session.snapshot().history.as_slice(),
            std::slice::from_ref(&entry)
        );
        h.action(Action::AddContact(Contact {
            name: "Alice".into(),
            uri: entry.target.clone(),
        }))
        .unwrap();
        assert_eq!(h.session.snapshot().history[0].peer, "Alice");
        assert_eq!(h.store.lock().unwrap().history[0].peer, "Original caller");
        h.action(Action::RemoveContact(entry.target)).unwrap();
        assert_eq!(h.session.snapshot().history[0].peer, "Original caller");
    }

    #[test]
    fn shutdown_reports_worker_failure_even_after_it_has_finished() {
        let mut h = Harness::new();
        h.backend.lock().unwrap().terminal = Some("backend lost".into());
        h.wait(|snapshot| !snapshot.running && snapshot.last_error.contains("backend lost"));
        while !h.session.worker.as_ref().unwrap().is_finished() {
            thread::yield_now();
        }
        assert!(
            h.session
                .shutdown()
                .unwrap_err()
                .to_string()
                .contains("backend lost")
        );
    }

    #[test]
    fn factory_starts_and_restarts_on_the_worker_thread() {
        let main_thread = thread::current().id();
        let backend = Arc::new(Mutex::new(BackendState::default()));
        let inner = backend.clone();
        let factory: BackendFactory = Box::new(move |_| {
            assert_ne!(thread::current().id(), main_thread);
            assert_eq!(thread::current().name(), Some("sip-session"));
            Ok(Box::new(FakeBackend(inner.clone())))
        });
        let session = SessionHandle::start_with_dependencies(
            Config::new("/unused-session-test"),
            factory,
            Box::new(MemoryStore(Arc::new(Mutex::new(MemoryState::default())))),
            Box::new(at),
        )
        .unwrap();
        session
            .dispatch_wait(Action::SaveAccount(credentials()))
            .unwrap();
        drop(session);
        assert_eq!(backend.lock().unwrap().stops, 2);
    }

    #[test]
    fn failed_dial_drops_target_that_grows_past_storage_limit_during_normalization() {
        let h = Harness::new();
        h.backend
            .lock()
            .unwrap()
            .failures
            .insert("dial".into(), "dial failed".into());
        let target = format!(
            "{}@example.com",
            "a".repeat(crate::storage::MAX_FIELD_LENGTH - "@example.com".len())
        );
        let error = h.action(Action::Dial(target.clone())).unwrap_err();
        assert!(format!("{error:#}").contains("discarded invalid call history entry"));
        assert!(
            h.backend
                .lock()
                .unwrap()
                .commands
                .contains(&format!("dial sip:{target}"))
        );
        assert!(h.session.snapshot().history.is_empty());
        assert_eq!(
            h.session.snapshot().state.call_state,
            domain::CallState::Idle
        );
    }

    #[test]
    fn trace_requires_explicit_log_destination_before_factory_is_called() {
        let backend = Arc::new(Mutex::new(BackendState::default()));
        let mut config = Config::new("/unused-session-test");
        config.sip_trace = true;
        let result = SessionHandle::start_with_dependencies(
            config,
            factory(backend.clone()),
            Box::new(MemoryStore(Arc::new(Mutex::new(MemoryState::default())))),
            Box::new(at),
        );
        assert!(result.is_err());
        assert_eq!(backend.lock().unwrap().starts, 0);
    }

    #[test]
    fn blocked_desktop_effects_do_not_delay_answer_or_shutdown() {
        let focus = Arc::new(Gate::default());
        let pause = Arc::new(Gate::default());
        let mut h = Harness::with_backend(
            MemoryState::default(),
            BackendState {
                async_effects: true,
                focus_gate: Some(focus.clone()),
                pause_gate: Some(pause.clone()),
                fail_focus: true,
                ..Default::default()
            },
        );
        h.event("CALL_INCOMING", "async", "sip:alice@example.com");
        focus.wait_entered();
        pause.wait_entered();
        h.session
            .dispatch_until(Action::Answer, Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert!(
            h.backend
                .lock()
                .unwrap()
                .commands
                .contains(&"accept".into())
        );
        focus.release();
        h.wait(|snapshot| {
            snapshot
                .last_error
                .contains("session: focus window: focus failed")
        });
        h.session
            .shutdown_until(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let terminal = h.session.snapshot();
        assert_eq!(terminal.history.len(), 1);
        assert_eq!(h.backend.lock().unwrap().stops, 1);
        pause.release();
        h.wait(|_| h.backend.lock().unwrap().effects.contains(&"pause".into()));
        assert_eq!(h.session.snapshot().revision, terminal.revision);
    }

    #[test]
    fn notifications_remain_fifo_without_blocking_call_events() {
        let notify = Arc::new(Gate::default());
        let h = Harness::with_backend(
            MemoryState::default(),
            BackendState {
                async_effects: true,
                notify_gate: Some(notify.clone()),
                ..Default::default()
            },
        );
        h.event("CALL_INCOMING", "fifo", "sip:alice@example.com");
        notify.wait_entered();
        h.event("CALL_CLOSED", "fifo", "");
        h.wait(|snapshot| snapshot.history.len() == 1);
        assert!(
            !h.backend
                .lock()
                .unwrap()
                .effects
                .iter()
                .any(|e| e.starts_with("notify"))
        );
        notify.release();
        h.wait(|_| {
            h.backend
                .lock()
                .unwrap()
                .effects
                .iter()
                .filter(|e| e.starts_with("notify"))
                .count()
                == 2
        });
        let notifications: Vec<_> = h
            .backend
            .lock()
            .unwrap()
            .effects
            .iter()
            .filter(|e| e.starts_with("notify"))
            .cloned()
            .collect();
        assert!(notifications[0].starts_with("notify Incoming call:"));
        assert!(notifications[1].starts_with("notify Missed call:"));
    }

    #[test]
    fn async_notification_errors_are_published_by_session_worker() {
        let notify = Arc::new(Gate::default());
        let h = Harness::with_backend(
            MemoryState::default(),
            BackendState {
                async_effects: true,
                notify_gate: Some(notify.clone()),
                fail_notification: true,
                ..Default::default()
            },
        );
        h.event("CALL_INCOMING", "error", "sip:alice@example.com");
        notify.wait_entered();
        h.action(Action::Answer).unwrap();
        assert!(h.session.snapshot().last_error.is_empty());
        notify.release();
        h.wait(|snapshot| {
            snapshot
                .last_error
                .contains("send Incoming call notification: notify-send failed")
        });
    }

    #[test]
    fn effect_dispatch_is_bounded_and_restart_discards_pending_and_stale_errors() {
        let (mut worker, backend, _) = worker_harness(MemoryState::default());
        let gate = Arc::new(Gate::default());
        {
            let mut backend = backend.lock().unwrap();
            backend.async_effects = true;
            backend.pause_gate = Some(gate.clone());
            backend.fail_pause = true;
        }
        worker.desktop_effect(DesktopTask::Pause).unwrap();
        assert!(worker.effects.poll().is_empty());
        gate.wait_entered();
        for _ in 0..EFFECT_CAPACITY {
            worker.desktop_effect(DesktopTask::Pause).unwrap();
        }
        assert!(
            worker
                .desktop_effect(DesktopTask::Pause)
                .unwrap_err()
                .to_string()
                .contains("queue full")
        );
        let old_thread = worker.effects.lanes[1]
            .active
            .as_ref()
            .unwrap()
            .1
            .thread()
            .id();
        for _ in 0..3 {
            worker.save_account(credentials()).unwrap();
            assert!(worker.effects.lanes[1].pending.is_empty());
            assert_eq!(
                worker.effects.lanes[1]
                    .active
                    .as_ref()
                    .unwrap()
                    .1
                    .thread()
                    .id(),
                old_thread
            );
        }
        gate.release();
        let deadline = Instant::now() + Duration::from_secs(1);
        while worker.effects.lanes[1].active.is_some() {
            assert!(
                worker.effects.poll().is_empty(),
                "stale effect error escaped restart"
            );
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            backend
                .lock()
                .unwrap()
                .effects
                .iter()
                .filter(|e| *e == "pause")
                .count(),
            1
        );
        backend.lock().unwrap().fail_pause = false;
        worker.desktop_effect(DesktopTask::Pause).unwrap();
        worker.effects.poll();
        while worker.effects.lanes[1].active.is_some() {
            assert!(worker.effects.poll().is_empty());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            backend
                .lock()
                .unwrap()
                .effects
                .iter()
                .filter(|e| *e == "pause")
                .count(),
            2
        );
    }

    #[test]
    fn pending_effects_expire_without_touching_adapter() {
        let (mut worker, backend, _) = worker_harness(MemoryState::default());
        backend.lock().unwrap().async_effects = true;
        worker.desktop_effect(DesktopTask::Focus).unwrap();
        worker.effects.lanes[0]
            .pending
            .front_mut()
            .unwrap()
            .deadline = Instant::now();
        let errors = worker.effects.poll();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].to_string().contains("expired"));
        assert!(worker.effects.lanes[0].active.is_none());
        assert!(!backend.lock().unwrap().effects.contains(&"focus".into()));
    }

    #[test]
    fn drop_discards_pending_effects_without_joining_blocked_adapter() {
        let gate = Arc::new(Gate::default());
        let h = Harness::with_backend(
            MemoryState::default(),
            BackendState {
                async_effects: true,
                notify_gate: Some(gate.clone()),
                ..Default::default()
            },
        );
        h.event("CALL_INCOMING", "drop-effects", "sip:alice@example.com");
        gate.wait_entered();
        h.event("CALL_CLOSED", "drop-effects", "");
        h.wait(|snapshot| snapshot.history.len() == 1);
        // A barrier ensures the missed notification has reached the pending queue.
        h.action(Action::RefreshAudio).unwrap();
        let backend = h.backend.clone();
        let store = h.store.clone();
        let started = Instant::now();
        drop(h);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(backend.lock().unwrap().stops, 1);
        assert_eq!(store.lock().unwrap().history_writes, 1);
        gate.release();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let effects = backend.lock().unwrap().effects.clone();
            if effects
                .iter()
                .any(|e| e.starts_with("notify Incoming call:"))
            {
                assert!(!effects.iter().any(|e| e.starts_with("notify Missed call:")));
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn full_request_queue_cannot_delay_quit_or_execute_backlog() {
        for wait in [false, true] {
            let mut h = Harness::new();
            h.action(Action::Dial("123".into())).unwrap();
            h.event("CALL_ESTABLISHED", "backlog", "sip:123@example.com");
            h.wait(|snapshot| snapshot.state.call_state == domain::CallState::Active);
            let gate = Arc::new(Gate::default());
            h.backend.lock().unwrap().command_gate = Some(gate.clone());
            h.session.dispatch(Action::ToggleMute).unwrap();
            gate.wait_entered();
            let (reply, replies) = mpsc::channel();
            for _ in 0..REQUEST_CAPACITY {
                h.session
                    .enqueue(Request {
                        action: Action::ToggleDnd,
                        deadline: Instant::now() + OPERATION_TIMEOUT,
                        reply: Some(reply.clone()),
                    })
                    .unwrap();
            }
            assert!(
                h.session
                    .dispatch(Action::ToggleDnd)
                    .unwrap_err()
                    .to_string()
                    .contains("queue full")
            );
            assert!(
                h.action(Action::ToggleDnd)
                    .unwrap_err()
                    .to_string()
                    .contains("queue full")
            );
            thread::scope(|scope| {
                let quit = scope.spawn(|| {
                    if wait {
                        h.action(Action::Quit)
                    } else {
                        h.session.dispatch(Action::Quit)
                    }
                });
                let deadline = Instant::now() + Duration::from_secs(1);
                while !h.session.stopping.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline);
                    thread::yield_now();
                }
                assert!(h.session.dispatch(Action::ToggleDnd).is_err());
                gate.release();
                quit.join().unwrap().unwrap();
            });
            h.session.shutdown().unwrap();
            for _ in 0..REQUEST_CAPACITY {
                assert!(
                    replies
                        .recv_timeout(Duration::from_secs(1))
                        .unwrap()
                        .is_err()
                );
            }
            let snapshot = h.session.snapshot();
            assert!(!snapshot.state.dnd);
            assert_eq!(snapshot.history.len(), 1);
            assert_eq!(h.store.lock().unwrap().history_writes, 1);
            let backend = h.backend.lock().unwrap();
            assert_eq!(backend.stops, 1);
            assert_eq!(
                backend.commands.iter().filter(|c| *c == "hangup").count(),
                1
            );
        }
    }

    #[test]
    fn shutdown_and_drop_preempt_a_full_backlog() {
        for drop_handle in [false, true] {
            let mut h = Harness::new();
            let gate = Arc::new(Gate::default());
            h.backend.lock().unwrap().command_gate = Some(gate.clone());
            h.session.dispatch(Action::Dial("123".into())).unwrap();
            gate.wait_entered();
            for _ in 0..REQUEST_CAPACITY {
                h.session
                    .dispatch(Action::AddContact(Contact {
                        name: "Queued".into(),
                        uri: "sip:queued@example.com".into(),
                    }))
                    .unwrap();
            }
            let stopping = h.session.stopping.clone();
            let backend = h.backend.clone();
            let store = h.store.clone();
            let release = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(1);
                while !stopping.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(2));
                }
                gate.release();
            });
            if drop_handle {
                drop(h);
            } else {
                h.session.shutdown().unwrap();
            }
            release.join().unwrap();
            assert!(store.lock().unwrap().contacts.is_empty());
            assert_eq!(store.lock().unwrap().history_writes, 1);
            assert_eq!(backend.lock().unwrap().stops, 1);
        }
    }

    #[test]
    fn timed_out_requests_never_execute_after_backlog_clears() {
        let h = Harness::new();
        let gate = Arc::new(Gate::default());
        h.backend.lock().unwrap().command_gate = Some(gate.clone());
        h.session.dispatch(Action::Dial("123".into())).unwrap();
        gate.wait_entered();
        let error = h
            .session
            .dispatch_until(
                Action::AddContact(Contact {
                    name: "Expired".into(),
                    uri: "sip:expired@example.com".into(),
                }),
                Instant::now() + Duration::from_millis(30),
            )
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        let (reply, result) = mpsc::channel();
        h.session
            .enqueue(Request {
                action: Action::ToggleDnd,
                deadline: Instant::now(),
                reply: Some(reply),
            })
            .unwrap();
        gate.release();
        assert!(
            result
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("expired")
        );
        h.action(Action::RefreshAudio).unwrap();
        assert!(!h.session.snapshot().state.dnd);
        assert!(h.store.lock().unwrap().contacts.is_empty());
    }

    #[test]
    fn shutdown_timeout_does_not_join_an_uncooperative_backend() {
        let mut h = Harness::new();
        let gate = Arc::new(Gate::default());
        h.backend.lock().unwrap().command_gate = Some(gate.clone());
        h.session.dispatch(Action::Dial("123".into())).unwrap();
        gate.wait_entered();
        let started = Instant::now();
        let error = h
            .session
            .shutdown_until(started + Duration::from_millis(30))
            .unwrap_err();
        assert!(error.to_string().contains("shutdown timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(h.session.worker.is_none());
        gate.release();
        h.wait(|snapshot| !snapshot.running);
        assert_eq!(h.backend.lock().unwrap().stops, 1);
        assert_eq!(h.store.lock().unwrap().history_writes, 1);
    }

    #[test]
    fn dropping_handle_joins_and_stops_backend_once() {
        let h = Harness::new();
        let backend = h.backend.clone();
        drop(h);
        assert_eq!(backend.lock().unwrap().stops, 1);
    }
}
