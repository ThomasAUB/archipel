//! Archipel desktop application (GPUI).
//!
//! `main` is a composition root: it builds the core through
//! `archipel-compose` and hands the UI-agnostic `App` to the views. Views
//! only use `archipel-app` view-models and commands, and theme tokens.
//!
//! Startup (spec: < 1 s) overlaps the two slow parts: the core is composed
//! and the project opened on a background thread while GPUI initializes its
//! platform (graphics device, fonts) on the main thread.
//!
//! Command line: `archipel [<dir>]` opens the app on a directory (spec §1
//! "Launch from a directory"); without it, the last project reopens. When
//! Archipel is already running, the request goes to that process, which
//! opens a new window ([`instance`]); `--standalone` skips that hand-off.
//!
//! `archipel git-credential <op>` is not for people: the core hands it to
//! `git` as a credential helper serving the forge tokens to HTTPS fetch and
//! push. It answers on stdout and exits, before any window or hand-off.

// Release builds are GUI-subsystem executables on Windows: launching from
// Explorer opens no console window. Debug builds keep the console so logs
// and panics stay visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod armed;
mod branches_panel;
mod ci_tooltip;
mod commit_panel;
mod conventional;
mod diff_view;
mod file_edit;
mod fonts;
mod forecast_badge;
mod forge_panel;
mod icons;
mod ignore;
mod image_view;
mod instance;
mod jump_list;
mod keys;
mod latest;
mod markdown_view;
mod menu;
mod merge_view;
mod new_repo_dialog;
mod palette;
mod publish_dialog;
mod quick_edit;
mod repo_view;
mod staging;
mod startup;
mod stats_panel;
mod syntax;
mod terminal;
mod theme;
mod widgets;
mod windows;
mod working_copy;
mod workspace;
mod worktree_dialog;
mod worktree_open;
mod worktree_set_dialog;

use std::ops::ControlFlow;
use std::path::PathBuf;

use archipel_compose::{AppDirs, ComposeError, ComposeOptions, Composition};
use gpui_kit::{App, Global, KeyBinding};

/// Keeps the core's async runtime alive for the lifetime of the GPUI app.
struct CoreRuntime(Option<tokio::runtime::Runtime>);

/// The settings file (`settings.toml`), for the settings changed in the app
/// (the merge tool).
pub(crate) struct SettingsFile(pub std::path::PathBuf);

impl Global for SettingsFile {}

impl Global for CoreRuntime {}

impl Drop for CoreRuntime {
    /// Quitting must not wait for blocking jobs: a keychain read waiting on an
    /// unlock prompt never returns, and a plain drop of the runtime would then
    /// keep the windowless process alive (taking every later launch's request).
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// Command-line options.
struct Args {
    /// Directory to open.
    dir: Option<PathBuf>,
    /// Do not hand the request to a running Archipel.
    standalone: bool,
    /// Restarted after an update: take over from the quitting Archipel.
    after_update: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        dir: None,
        standalone: false,
        after_update: false,
    };
    let mut raw = std::env::args_os().skip(1);
    while let Some(arg) = raw.next() {
        match arg.to_str() {
            Some("--standalone") => args.standalone = true,
            Some(instance::AFTER_UPDATE) => args.after_update = true,
            Some(jump_list::DOCK_ACTION_FLAG) => {
                // A jump list task: its index follows (see `jump_list`).
                raw.next();
            }
            Some("-h" | "--help") => {
                return Err("usage: archipel [--standalone] [<directory>]".into());
            }
            Some(flag) if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ if args.dir.is_some() => return Err("only one directory can be opened".into()),
            _ => {
                let dir = std::path::absolute(PathBuf::from(&arg))
                    .map_err(|e| format!("{}: {e}", PathBuf::from(&arg).display()))?;
                args.dir = Some(dir);
            }
        }
    }
    Ok(args)
}

/// Composes the core and opens `dir`, or reopens the most recent project
/// (no GPUI needed). Also returns notices to show (a directory that could
/// not be opened).
fn compose_and_open(
    dirs: AppDirs,
    dir: Option<PathBuf>,
) -> Result<(Composition, Vec<String>), ComposeError> {
    let options = ComposeOptions {
        credential_helper: std::env::current_exe()
            .ok()
            .map(|exe| archipel_compose::credential_helper_command(&exe)),
    };
    let composition = archipel_compose::compose_with(dirs, options)?;
    for (step, d) in &composition.timings {
        startup::mark(&format!(
            "compose/{step} (+{:.1} ms)",
            d.as_secs_f64() * 1e3
        ));
    }
    startup::mark("compose");
    let app = &composition.app;
    let mut notices = Vec::new();
    if let Some(dir) = dir {
        match app.open_directory(&dir) {
            Ok(_) => {
                startup::mark("directory opened");
                return Ok((composition, notices));
            }
            Err(e) => notices.push(format!("cannot open {}: {e}", dir.display())),
        }
    }
    let recent = app
        .projects()
        .ok()
        .and_then(|p| p.into_iter().find(|e| e.last_opened.is_some()));
    if let Some(entry) = recent
        && let Err(e) = app.open_project(&entry.id)
    {
        tracing::warn!(error = %e, "cannot reopen the last project");
    }
    startup::mark("project reopened");
    Ok((composition, notices))
}

/// Runs as git's credential helper (`git-credential <op>`, request on
/// stdin). Failures stay silent on stdout: git then asks its next helper.
fn git_credential(op: &str) -> i32 {
    use std::io::{Read, Write};
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 1;
    }
    let answer = AppDirs::resolve()
        .map_err(|e| e.to_string())
        .and_then(|dirs| {
            archipel_compose::git_credential(&dirs, op, &input).map_err(|e| e.to_string())
        });
    match answer {
        Ok(text) => {
            let _ = std::io::stdout().write_all(text.as_bytes());
            0
        }
        Err(e) => {
            eprintln!("archipel git-credential: {e}");
            1
        }
    }
}

fn main() {
    let mut raw = std::env::args().skip(1);
    if raw.next().as_deref() == Some("git-credential") {
        let op = raw.next().unwrap_or_default();
        std::process::exit(git_credential(&op));
    }
    startup::begin();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("archipel: {e}");
            std::process::exit(2);
        }
    };
    let dirs = match AppDirs::resolve() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("archipel: {e}");
            std::process::exit(1);
        }
    };
    let _log = archipel_compose::init_logging(&dirs);
    archipel_compose::install_panic_hook(&dirs);
    startup::mark("logging");
    let ControlFlow::Continue(listener) = claim_instance(&args, &dirs) else {
        return;
    };
    startup::mark("instance");
    let themes_dir = dirs.themes_dir();
    let appearance_file = dirs.appearance_file();
    let dir = args.dir;
    let core = std::thread::Builder::new()
        .name("archipel-compose".into())
        .spawn(move || compose_and_open(dirs, dir));

    let application = gpui_kit::application();
    startup::mark("gpui platform created");
    let open_requests = jump_list::on_open_urls(&application);
    application
        .with_assets(icons::AppAssets)
        .run(move |cx: &mut App| {
            startup::mark("gpui running");
            gpui_kit::init(cx);
            let (ui_theme, theme_warning) =
                theme::UiTheme::load_remembered(&themes_dir, appearance_file, cx);
            cx.set_global(ui_theme);
            theme::sync_toolkit_theme(cx);
            startup::mark("gpui-kit init + theme");
            let (composition, notices) = match core.map_err(|e| e.to_string()).and_then(|h| {
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
                mut warnings,
                dirs,
                ..
            } = composition;
            cx.set_global(SettingsFile(dirs.settings_file()));
            warnings.extend(theme_warning);
            for w in &warnings {
                tracing::warn!("{w}");
            }
            warnings.extend(bind_keys(cx, &app));
            warnings.splice(0..0, notices);
            cx.set_global(CoreRuntime(Some(runtime)));
            cx.set_global(windows::Windows::new(app.clone()));
            windows::save_on_quit(cx);
            menu::install(cx);
            windows::open_first(cx, app, warnings);
            startup::mark("window opened");
            if let Some(listener) = listener {
                serve_other_launches(cx, listener);
            }
            jump_list::serve_open_urls(cx, open_requests);
            jump_list::refresh(cx);
            cx.activate(true);
        });
}

/// Becomes the primary instance (listening for later launches), or hands
/// the request to the running one: `Break` then means this process is done.
fn claim_instance(args: &Args, dirs: &AppDirs) -> ControlFlow<(), Option<instance::Listener>> {
    if args.standalone {
        return ControlFlow::Continue(None);
    }
    if args.after_update {
        return ControlFlow::Continue(match instance::take_over(&dirs.data) {
            Some(instance::Startup::Primary(listener)) => Some(listener),
            _ => None,
        });
    }
    let request = instance::Request {
        dir: args.dir.clone(),
    };
    match instance::forward_or_listen(&dirs.data, &request) {
        Some(instance::Startup::Forwarded) => {
            startup::mark("forwarded to the running instance");
            ControlFlow::Break(())
        }
        Some(instance::Startup::Primary(listener)) => ControlFlow::Continue(Some(listener)),
        None => ControlFlow::Continue(None),
    }
}

/// Opens a window for each later `archipel [<dir>]` launch.
fn serve_other_launches(cx: &mut App, listener: instance::Listener) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    listener.serve(move |request| {
        // The receiver lives as long as the app.
        let _ = tx.send(request);
    });
    cx.spawn(async move |cx| {
        while let Some(request) = rx.recv().await {
            cx.update(|cx| windows::open(cx, request.dir.as_deref()));
        }
    })
    .detach();
}

/// Installs the keymap (from `archipel-app`, rebindable) as GPUI bindings,
/// each running its action of the registry as the palette does. Returns a
/// warning per binding that cannot work, for the status bar.
///
/// The shortcuts a shell needs (Ctrl + a letter…, [`keys::goes_to_terminal`])
/// are left to the focused integrated terminal: bound to nothing in its
/// context, after the keymap, so they outrank it there.
fn bind_keys(cx: &mut App, app: &archipel_app::App) -> Vec<String> {
    let extensions = app.extensions();
    let mut warnings = Vec::new();
    let mut bindings = Vec::new();
    let mut to_terminal = Vec::new();
    for (stroke, id) in extensions.keymap.iter() {
        if extensions.actions.get(id).is_none() {
            warnings.push(format!(
                "keymap: {stroke} is bound to “{id}”, which is not an action"
            ));
            continue;
        }
        let action = Box::new(workspace::RunAction(id.to_owned()));
        let gpui = keys::gpui_keystroke(stroke);
        match KeyBinding::load(&gpui, action, None, false, None, cx) {
            Ok(b) => {
                bindings.push(b);
                if keys::goes_to_terminal(&gpui) {
                    to_terminal.push(gpui);
                }
            }
            Err(e) => warnings.push(format!("keymap: {stroke} cannot be bound: {e:?}")),
        }
    }
    let terminal = gpui_kit::KeyBindingContextPredicate::parse("Terminal")
        .ok()
        .map(std::rc::Rc::new);
    for stroke in to_terminal {
        let none = Box::new(gpui_kit::NoAction);
        if let Ok(b) = KeyBinding::load(&stroke, none, terminal.clone(), false, None, cx) {
            bindings.push(b);
        }
    }
    cx.bind_keys(bindings);
    warnings
}
