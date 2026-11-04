//! Archipel desktop application (GPUI).
//!
//! `main` is a composition root: it builds the core through
//! `archipel-compose` and hands the UI-agnostic `App` to the views. Views
//! only use `archipel-app` view-models and commands, and theme tokens.
//!
//! Startup (spec: < 1 s) overlaps the two slow parts: the core is composed
//! and the last project reopened on a background thread while GPUI
//! initializes its platform (graphics device, fonts) on the main thread.

// Render functions are declarative element trees: splitting them by line
// count hurts readability more than it helps.
#![allow(clippy::too_many_lines)]

mod commit_panel;
mod palette;
mod repo_view;
mod startup;
mod theme;
mod widgets;
mod workspace;

use std::sync::Arc;

use archipel_compose::{AppDirs, ComposeError, Composition};
use gpui_kit::{
    App, AppContext, Bounds, Global, KeyBinding, WindowBounds, WindowOptions, px, size,
};

use crate::workspace::Workspace;

/// Keeps the core's async runtime alive for the lifetime of the GPUI app.
struct CoreRuntime(#[allow(dead_code)] tokio::runtime::Runtime);

impl Global for CoreRuntime {}

/// Composes the core and reopens the most recent project (no GPUI needed).
fn compose_and_reopen(dirs: AppDirs) -> Result<Composition, ComposeError> {
    let composition = archipel_compose::compose(dirs)?;
    for (step, d) in &composition.timings {
        startup::mark(&format!(
            "compose/{step} (+{:.1} ms)",
            d.as_secs_f64() * 1e3
        ));
    }
    startup::mark("compose");
    let app = &composition.app;
    let recent = app
        .projects()
        .ok()
        .and_then(|p| p.into_iter().find(|e| e.last_opened.is_some()));
    if let Some(entry) = recent {
        if let Err(e) = app.open_project(&entry.id) {
            tracing::warn!(error = %e, "cannot reopen the last project");
        }
    }
    startup::mark("project reopened");
    Ok(composition)
}

fn main() {
    startup::begin();
    let dirs = match AppDirs::resolve() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("archipel: {e}");
            std::process::exit(1);
        }
    };
    let _log = archipel_compose::init_logging(&dirs);
    startup::mark("logging");
    let themes_dir = dirs.themes_dir();
    let core = std::thread::Builder::new()
        .name("archipel-compose".into())
        .spawn(move || compose_and_reopen(dirs));

    let application = gpui_kit::application();
    startup::mark("gpui platform created");
    application
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            startup::mark("gpui running");
            gpui_kit::init(cx);
            let ui_theme =
                theme::UiTheme::load(&themes_dir, archipel_theme::builtin::DEFAULT_ID, cx);
            cx.set_global(ui_theme);
            startup::mark("gpui-kit init + theme");
            let composition = match core.map_err(|e| e.to_string()).and_then(|h| {
                h.join()
                    .map_err(|_| "the startup thread panicked".to_owned())?
                    .map_err(|e| e.to_string())
            }) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "startup failed");
                    eprintln!("archipel: {e}");
                    cx.quit();
                    return;
                }
            };
            startup::mark("core joined");
            let Composition {
                app,
                runtime,
                warnings,
                ..
            } = composition;
            for w in &warnings {
                tracing::warn!("{w}");
            }
            cx.set_global(CoreRuntime(runtime));
            bind_keys(cx, &app);
            open_main_window(cx, app, warnings);
            startup::mark("window opened");
            cx.activate(true);
        });
}

fn open_main_window(cx: &mut App, app: Arc<archipel_app::App>, warnings: Vec<String>) {
    let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    };
    let opened = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| Workspace::new(app, warnings, window, cx))
    });
    if let Err(e) = opened {
        tracing::error!(error = %e, "cannot open the window");
        cx.quit();
    }
}

/// Installs the keymap (from `archipel-app`, rebindable) as GPUI bindings.
fn bind_keys(cx: &mut App, app: &archipel_app::App) {
    let bindings: Vec<KeyBinding> = app
        .extensions()
        .keymap
        .iter()
        .filter_map(|(stroke, action)| {
            let action = workspace::action_for(action)?;
            match KeyBinding::load(&stroke.to_string(), action, None, false, None, cx) {
                Ok(b) => Some(b),
                Err(e) => {
                    tracing::warn!(key = %stroke, error = ?e, "unusable key binding");
                    None
                }
            }
        })
        .collect();
    cx.bind_keys(bindings);
}
