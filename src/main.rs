use std::{
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
use sippy::{
    assets::Assets,
    baresip, input,
    session::{Config, SessionHandle, Snapshot},
    settings::OmarchyTheme,
    storage,
    ui::Workspace,
};

#[derive(Parser)]
#[command(about = "Sippy SIP softphone")]
struct Options {
    #[arg(long, help = "baresip configuration directory, defaults to ~/.baresip")]
    config_dir: Option<PathBuf>,
    #[arg(long, default_value = "baresip")]
    baresip: PathBuf,
    #[arg(long, help = "baresip module directory for a newly created config")]
    baresip_modules: Option<PathBuf>,
    #[arg(
        long,
        default_value = "49",
        help = "Country calling code used for contact matching"
    )]
    country_code: String,
    #[arg(long, help = "Append sensitive baresip debugging output to this file")]
    baresip_log: Option<PathBuf>,
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        default_value = "false",
        action = clap::ArgAction::Set,
        requires_if("true", "baresip_log"),
        help = "Trace SIP from startup; includes authentication and call data"
    )]
    sip_trace: bool,
}

fn main() {
    env_logger::init();
    if let Err(error) = run() {
        eprintln!("sippy: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // Accept both single- and double-dash option spellings.
    let args = std::env::args_os().map(|arg| {
        let text = arg.to_string_lossy();
        let name = text.split('=').next().unwrap_or_default();
        if [
            "-config-dir",
            "-baresip",
            "-baresip-modules",
            "-country-code",
            "-baresip-log",
            "-sip-trace",
        ]
        .contains(&name)
        {
            format!("-{text}").into()
        } else {
            arg
        }
    });
    let options = Options::parse_from(args);
    let directory = match options.config_dir {
        Some(directory) => directory,
        None => PathBuf::from(
            std::env::var_os("HOME").context("cannot find home directory; pass --config-dir")?,
        )
        .join(".baresip"),
    };
    let baresip_path = baresip::locate(&options.baresip)?;
    if let Some(warning) = baresip::check_version(baresip::probe_version(&baresip_path)?)? {
        eprintln!("sippy: warning: {warning}");
    }
    let module_dir = options.baresip_modules.as_deref();
    let created = storage::ensure_config(&directory, || {
        baresip::config_defaults(&baresip_path, module_dir)
    })?;
    if !created && module_dir.is_some() {
        eprintln!(
            "sippy: warning: --baresip-modules only applies to a new config; the existing config keeps its module_path"
        );
    }
    let settings_store = storage::Store::new(directory.clone());
    let warnings = baresip::check_config(&settings_store.paths().config, || {
        let candidates = baresip::module_dir_candidates(&baresip_path);
        baresip::find_module_dir(&candidates, &storage::default_modules()).ok()
    })?;
    for warning in warnings {
        eprintln!("sippy: warning: {warning}");
    }
    let mut config = Config::new(directory);
    config.baresip_path = baresip_path;
    config.country_calling_code = options.country_code;
    config.log_path = options.baresip_log;
    config.sip_trace = options.sip_trace;
    let interrupted = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, interrupted.clone())
            .context("register shutdown signal")?;
    }
    let session = Arc::new(SessionHandle::start(config).context("start phone session")?);
    let signals = InterruptRelay::new(session.clone(), interrupted.clone())?;
    let startup_error = Arc::new(Mutex::new(None));
    let ui_error = startup_error.clone();
    let terminal = run_guarded(session, move |ui_session| {
        let app = Application::new().with_assets(Assets);
        app.run(move |cx: &mut App| {
            input::init(cx);
            cx.set_global(OmarchyTheme::new(OmarchyTheme::default_path()));
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1000.), px(700.)), cx);
            let result = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Sippy".into()),
                        ..Default::default()
                    }),
                    app_id: Some("sippy".into()),
                    window_min_size: Some(size(px(540.), px(440.))),
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("Sippy");
                    cx.new(|cx| {
                        let mut workspace = Workspace::new(ui_session, interrupted, window, cx);
                        workspace.load_settings(settings_store, cx);
                        workspace
                    })
                },
            );
            match result {
                Ok(_) => cx.activate(true),
                Err(error) => {
                    *ui_error.lock().unwrap() = Some(format!("open window: {error:#}"));
                    cx.quit();
                }
            }
        });
    })?;
    drop(signals);
    if let Some(error) = startup_error.lock().unwrap().take() {
        bail!(error);
    }
    if !terminal.running && !terminal.last_error.is_empty() {
        bail!("{}", terminal.last_error);
    }
    Ok(())
}

struct InterruptRelay {
    done: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl InterruptRelay {
    fn new(session: Arc<SessionHandle>, interrupted: Arc<AtomicBool>) -> Result<Self> {
        let done = Arc::new(AtomicBool::new(false));
        let finished = done.clone();
        let worker = thread::Builder::new()
            .name("sip-signals".into())
            .spawn(move || {
                while !finished.load(Ordering::Acquire) {
                    if interrupted.load(Ordering::Relaxed) {
                        session.request_shutdown();
                        break;
                    }
                    thread::park_timeout(Duration::from_millis(20));
                }
            })
            .context("start shutdown signal relay")?;
        Ok(Self {
            done,
            worker: Some(worker),
        })
    }
}

impl Drop for InterruptRelay {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;

fn run_guarded(
    session: Arc<SessionHandle>,
    ui: impl FnOnce(Arc<SessionHandle>),
) -> Result<Snapshot> {
    let outcome = catch_unwind(AssertUnwindSafe(|| ui(session.clone())));
    let terminal = session.snapshot();
    // GPUI may retain entities on unwind, so cleanup cannot depend on the last Arc dropping.
    let stopped = match Arc::try_unwrap(session) {
        Ok(mut session) => session.shutdown(),
        Err(session) => session.dispatch_wait(sippy::session::Action::Quit),
    };
    if let Err(panic) = outcome {
        resume_unwind(panic);
    }
    stopped?;
    Ok(terminal)
}
