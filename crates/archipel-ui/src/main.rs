//! Archipel desktop application (GPUI).
//!
//! `main` is a composition root: it builds the core through
//! `archipel-compose` and hands the UI-agnostic `App` to the views. Views
//! only use `archipel-app` view-models and commands, and theme tokens.

// Render functions are declarative element trees: splitting them by line
// count hurts readability more than it helps.
#![allow(clippy::too_many_lines)]

mod commit_panel;
mod palette;
mod repo_view;
mod theme;
mod widgets;
mod workspace;

use gpui_kit::{App, AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size};

use crate::workspace::Workspace;

fn main() {
    let dirs = match archipel_compose::AppDirs::resolve() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("archipel: {e}");
            std::process::exit(1);
        }
    };
    let _log = archipel_compose::init_logging(&dirs);
    let composition = match archipel_compose::compose(dirs) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "startup failed");
            eprintln!("archipel: {e}");
            std::process::exit(1);
        }
    };
    let archipel_compose::Composition {
        app,
        runtime,
        dirs,
        warnings,
    } = composition;
    for w in &warnings {
        tracing::warn!("{w}");
    }

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            cx.set_global(theme::UiTheme::load(
                &dirs.themes_dir(),
                archipel_theme::builtin::DEFAULT_ID,
            ));
            bind_keys(cx, &app);
            let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            };
            let app = app.clone();
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| Workspace::new(app, warnings, window, cx))
            });
            if let Err(e) = opened {
                tracing::error!(error = %e, "cannot open the window");
                cx.quit();
            }
            cx.activate(true);
        });
    drop(runtime);
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
