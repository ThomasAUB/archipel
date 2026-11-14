//! Windows of the process (spec §2 "Several instances side by side").
//!
//! Every window shows one project through its own `archipel_app::App`
//! session ([`archipel_app::App::sibling`]): own selected repo, filters,
//! layout and background refresh, while ports, scheduler, caches and the
//! SQLite store are shared. Opening a project that a window already shows
//! focuses that window instead of opening a duplicate.

use std::path::Path;
use std::sync::Arc;

use archipel_app::App as Core;
use archipel_model::ProjectId;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Global, WindowBounds, WindowOptions, px, size,
};

use crate::workspace::Workspace;

/// Default size of a new window.
const WINDOW_WIDTH: f32 = 1280.0;
/// Default height of a new window.
const WINDOW_HEIGHT: f32 = 800.0;
/// Wayland `app_id` / X11 `WM_CLASS`: matches `assets/linux/archipel.desktop`,
/// so desktops group the windows under the Archipel icon.
const APP_ID: &str = "archipel";

/// The first update check waits this long, off the startup path.
const UPDATE_CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// The open windows and the core sessions behind them.
pub struct Windows {
    /// Session factory: the first window's core (kept alive even if that
    /// window closes, siblings are cloned from it).
    base: Arc<Core>,
    /// Whether `base` is still unused (the first window takes it).
    base_free: bool,
    /// In opening order (the order windows are cycled in).
    open: Vec<(AnyWindowHandle, Arc<Core>)>,
    /// Most recently activated first.
    recent: Vec<AnyWindowHandle>,
}

impl Global for Windows {}

impl Windows {
    /// A registry whose first window will use `base`.
    pub fn new(base: Arc<Core>) -> Self {
        Self {
            base,
            base_free: true,
            open: Vec::new(),
            recent: Vec::new(),
        }
    }

    fn take_core(&mut self) -> Arc<Core> {
        if std::mem::take(&mut self.base_free) {
            self.base.clone()
        } else {
            self.base.sibling()
        }
    }

    /// The window showing `project`, if any (other than `except`).
    fn showing(
        &self,
        project: &ProjectId,
        except: Option<AnyWindowHandle>,
    ) -> Option<AnyWindowHandle> {
        self.open
            .iter()
            .filter(|(h, _)| Some(*h) != except)
            .find(|(_, core)| core.project().is_some_and(|(id, _)| &id == project))
            .map(|(h, _)| *h)
    }
}

/// Opens a window on `dir` (a project directory) or an empty one; focuses
/// the window already showing that project instead of duplicating it.
pub fn open(cx: &mut App, dir: Option<&Path>) {
    if let Some(dir) = dir {
        let target = cx.global::<Windows>().base.directory_target(dir);
        match target {
            Ok(target) => {
                if focus_project(cx, &target.id, None) {
                    return;
                }
            }
            Err(e) => tracing::warn!(error = %e, dir = %dir.display(), "cannot open directory"),
        }
    }
    let core = cx.global_mut::<Windows>().take_core();
    let mut warnings = Vec::new();
    if let Some(dir) = dir {
        core.close_project();
        if let Err(e) = core.open_directory(dir) {
            warnings.push(format!("cannot open {}: {e}", dir.display()));
        }
    }
    open_with(cx, core, warnings, false);
}

/// Opens the first window, on the base core session prepared at startup,
/// in fullscreen.
pub fn open_first(cx: &mut App, core: Arc<Core>, warnings: Vec<String>) {
    cx.global_mut::<Windows>().base_free = false;
    open_with(cx, core, warnings, true);
}

/// Opens a window on `core`; `fullscreen` opens it in fullscreen, restoring
/// to the default windowed bounds.
fn open_with(cx: &mut App, core: Arc<Core>, warnings: Vec<String>, fullscreen: bool) {
    let offset =
        px(28.0 * f32::from(u8::try_from(cx.global::<Windows>().open.len() % 8).unwrap_or(0)));
    let mut bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    bounds.origin.x += offset;
    bounds.origin.y += offset;
    let options = WindowOptions {
        window_bounds: Some(if fullscreen {
            WindowBounds::Fullscreen(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        }),
        app_id: Some(APP_ID.to_owned()),
        ..WindowOptions::default()
    };
    core.start_auto_fetch();
    core.start_update_check(UPDATE_CHECK_DELAY);
    let window_core = core.clone();
    match gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| Workspace::new(window_core, warnings, window, cx))
    }) {
        Ok((handle, _)) => {
            let windows = cx.global_mut::<Windows>();
            windows.open.push((handle, core));
            windows.recent.insert(0, handle);
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
        Err(e) => {
            tracing::error!(error = %e, "cannot open a window");
            if cx.global::<Windows>().open.is_empty() {
                cx.quit();
            }
        }
    }
}

/// Focuses the window showing `project` (not `except`); `false` when none does.
pub fn focus_project(cx: &mut App, project: &ProjectId, except: Option<AnyWindowHandle>) -> bool {
    let Some(handle) = cx.global::<Windows>().showing(project, except) else {
        return false;
    };
    handle
        .update(cx, |_, window, _| window.activate_window())
        .is_ok()
}

/// Records that `handle` became the active window (most recent first).
pub fn activated(cx: &mut App, handle: AnyWindowHandle) {
    let recent = &mut cx.global_mut::<Windows>().recent;
    recent.retain(|h| *h != handle);
    recent.insert(0, handle);
}

/// Activates the window after `from` in opening order (`forward`) or the one
/// before it, wrapping around; `false` when `from` is the only window.
pub fn cycle(cx: &mut App, from: AnyWindowHandle, forward: bool) -> bool {
    let open: Vec<AnyWindowHandle> = cx
        .global::<Windows>()
        .open
        .iter()
        .map(|(h, _)| *h)
        .collect();
    let Some(target) = neighbour(&open, from, forward) else {
        return false;
    };
    target
        .update(cx, |_, window, _| window.activate_window())
        .is_ok()
}

/// The item after (or before) `from` in `items`, wrapping around; `None`
/// when there is no other item. An unknown `from` starts at either end.
fn neighbour<T: Copy + PartialEq>(items: &[T], from: T, forward: bool) -> Option<T> {
    let others = items.iter().filter(|h| **h != from).count();
    if others == 0 {
        return None;
    }
    let n = items.len();
    let next = match items.iter().position(|h| *h == from) {
        Some(i) if forward => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None if forward => 0,
        None => n - 1,
    };
    Some(items[next])
}

/// Projects shown by windows other than `except`, most recently active first.
pub fn open_projects_by_recency(cx: &App, except: AnyWindowHandle) -> Vec<ProjectId> {
    let windows = cx.global::<Windows>();
    windows
        .recent
        .iter()
        .filter(|h| **h != except)
        .filter_map(|h| windows.open.iter().find(|(o, _)| o == h))
        .filter_map(|(_, core)| core.project().map(|(id, _)| id))
        .collect()
}

/// Forgets a closing window: its project is unloaded (UI state and
/// summaries saved); the app quits with its last window.
pub fn closed(cx: &mut App, handle: AnyWindowHandle) {
    let windows = cx.global_mut::<Windows>();
    if let Some(i) = windows.open.iter().position(|(h, _)| *h == handle) {
        let (_, core) = windows.open.remove(i);
        core.close_project();
    }
    windows.recent.retain(|h| *h != handle);
    if windows.open.is_empty() {
        cx.quit();
    }
}

/// Unloads every open project when the app quits other than by closing its
/// last window (Cmd+Q, Dock menu, logout), so their state is saved too.
pub fn save_on_quit(cx: &mut App) {
    cx.on_app_quit(|cx| {
        for (_, core) in cx.global_mut::<Windows>().open.drain(..) {
            core.close_project();
        }
        async {}
    })
    .detach();
}

/// Redraws every window (after a theme change).
pub fn refresh_all(cx: &mut App) {
    for handle in cx.windows() {
        let _ = handle.update(cx, |_, window, _| window.refresh());
    }
}

/// Whether a window other than `window` shows `project`.
pub fn is_open(window: &gpui_kit::Window, cx: &App, project: &ProjectId) -> bool {
    cx.global::<Windows>()
        .showing(project, Some(window.window_handle()))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::neighbour;

    #[test]
    fn neighbour_wraps_both_ways() {
        let items = [1, 2, 3];
        assert_eq!(neighbour(&items, 1, true), Some(2));
        assert_eq!(neighbour(&items, 3, true), Some(1));
        assert_eq!(neighbour(&items, 1, false), Some(3));
        assert_eq!(neighbour(&items, 2, false), Some(1));
    }

    #[test]
    fn neighbour_needs_another_item() {
        assert_eq!(neighbour(&[1], 1, true), None);
        assert_eq!(neighbour::<i32>(&[], 1, false), None);
        assert_eq!(neighbour(&[1, 2], 9, true), Some(1));
        assert_eq!(neighbour(&[1, 2], 9, false), Some(2));
    }
}
