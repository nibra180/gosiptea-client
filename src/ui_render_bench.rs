use super::*;
use gpui::{Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, size};
use std::{cell::RefCell, rc::Rc, time::Instant};

const WARMUP: usize = 25;
const SAMPLES: usize = 250;

#[derive(Default)]
struct Measurements {
    index: usize,
    rows: Vec<(View, f64, f64, f64, f64)>,
    pending: Option<(View, Instant, f64, f64, f64)>,
}

fn next_sample(
    workspace: Entity<Workspace>,
    state: Rc<RefCell<Measurements>>,
    window: &mut Window,
) {
    window.on_next_frame(move |window, cx| {
        let mut measurements = state.borrow_mut();
        if measurements.index == 0 {
            println!(
                "BENCH window viewport={:?} scale_factor={}",
                window.viewport_size(),
                window.scale_factor()
            );
        }
        if let Some((view, start, change, draw, total)) = measurements.pending.take()
            && measurements.index > WARMUP
        {
            measurements.rows.push((
                view,
                change,
                draw,
                total,
                start.elapsed().as_secs_f64() * 1000.,
            ));
        }
        if measurements.index == WARMUP + SAMPLES {
            println!("BENCH view,change_us,scene_draw_us,change_to_scene_us,next_callback_ms");
            for (view, change, draw, total, next) in &measurements.rows {
                println!("BENCH {view:?},{change:.3},{draw:.3},{total:.3},{next:.3}");
            }
            for view in View::ALL {
                for (name, column) in [
                    ("change_us", 0),
                    ("scene_draw_us", 1),
                    ("change_to_scene_us", 2),
                    ("next_callback_ms", 3),
                ] {
                    let mut values: Vec<f64> = measurements
                        .rows
                        .iter()
                        .filter(|row| row.0 == view)
                        .map(|row| match column {
                            0 => row.1,
                            1 => row.2,
                            2 => row.3,
                            _ => row.4,
                        })
                        .collect();
                    values.sort_by(f64::total_cmp);
                    println!(
                        "SUMMARY {view:?} {name} n={} median={:.3} p95={:.3} max={:.3}",
                        values.len(),
                        values[values.len() / 2],
                        values[(values.len() * 95).div_ceil(100) - 1],
                        values[values.len() - 1]
                    );
                }
            }
            cx.quit();
            return;
        }
        // Start at Contacts so every sample changes the active view.
        let view = View::ALL[(measurements.index + 1) % View::ALL.len()];
        let start = Instant::now();
        workspace.update(cx, |this, cx| this.change_view(view, window, cx));
        let change = start.elapsed().as_secs_f64() * 1_000_000.;
        // notify effects are deferred until this callback returns. Force dirty
        // state here so the synchronous draw cannot reuse the previous view.
        window.refresh();
        let draw_start = Instant::now();
        let arena = window.draw(cx);
        let draw = draw_start.elapsed().as_secs_f64() * 1_000_000.;
        let total = start.elapsed().as_secs_f64() * 1_000_000.;
        arena.clear();
        measurements.pending = Some((view, start, change, draw, total));
        measurements.index += 1;
        drop(measurements);
        next_sample(workspace, state, window);
    });
}

/// Requires a real graphics session. Opens only its own fake-backend window.
/// Times synchronous full scene construction, not GPU submission or display.
/// GPUI runs next-frame callbacks before draw/present, so callback spacing is
/// only a scheduling observation. The forced draw bypasses normal invalidation.
#[test]
#[ignore = "real-window rendering benchmark; run explicitly with --nocapture --test-threads=1"]
fn real_window_view_switch_latency() {
    let directory = Arc::new(tempfile::tempdir().unwrap());
    Store::new(directory.path()).ensure_config().unwrap();
    let backend = Arc::new(Mutex::new(BackendState::default()));
    let keep_directory = directory.clone();
    let session = Arc::new(
        SessionHandle::start_with_factory(
            Config::new(directory.path()),
            Box::new(move |_| {
                let _ = &keep_directory;
                Ok(Box::new(FakeBackend(backend.clone())))
            }),
            Box::new(|| DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
        )
        .unwrap(),
    );
    println!(
        "BENCH metadata debug_assertions={} warmup={WARMUP} samples={SAMPLES} requested_size=1000x700 empty_contacts=true fake_backend=true",
        cfg!(debug_assertions)
    );
    Application::new().run(move |cx| {
        crate::input::init(cx);
        let bounds = Bounds::centered(None, size(px(1000.), px(700.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Sippy isolated rendering benchmark".into()),
                    ..Default::default()
                }),
                app_id: Some("sippy-render-bench".into()),
                focus: false,
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| {
                    Workspace::new(session, Arc::new(AtomicBool::new(false)), window, cx)
                });
                next_sample(
                    workspace.clone(),
                    Rc::new(RefCell::new(Measurements::default())),
                    window,
                );
                workspace
            },
        )
        .unwrap();
    });
}
