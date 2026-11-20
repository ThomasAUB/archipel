//! Recent projects on the app icon's menu (spec §2 "Recent projects on the
//! app icon").
//!
//! Right-clicking the taskbar button (Windows) or the Dock icon (macOS)
//! lists the most recently opened projects, as editors do; picking one opens
//! it. Each entry is the project's folder, the one holding its
//! `archipel.toml`:
//!
//! - Windows: a "Recent Folders" category of the jump list. An entry
//!   relaunches `archipel <folder>`, which hands the request to the running
//!   process ([`crate::instance`]) or starts the app on that project.
//!   Entries the user removes from the list stay out of it for the session.
//!   Its "Tasks" hold "New Window" ([`DOCK_ACTION_FLAG`]).
//! - macOS: the Dock menu's recent documents. Picking one reaches the running
//!   app as a `file://` URL ([`on_open_urls`]). The list needs an app bundle
//!   declaring folders as documents; a bare binary shows none.
//! - Linux: nothing (desktop entries have no dynamic list).

use std::path::{Path, PathBuf};

use archipel_model::project::{ProjectLocation, RegistryEntry};
use gpui_kit::{App, Global};

/// What was last handed to the platform, so unchanged lists are not
/// rewritten on every project opening.
#[derive(Default)]
struct JumpList {
    /// Folders listed, most recent first.
    shown: Vec<PathBuf>,
    /// Folders the user removed from the Windows jump list this session.
    removed: Vec<PathBuf>,
}

impl Global for JumpList {}

/// Option a jump list task relaunches Archipel with, followed by the task's
/// index. The only task is "New Window", which is what a launch without a
/// directory asks of a running Archipel, so the option and its index are
/// read and dropped.
pub const DOCK_ACTION_FLAG: &str = "--dock-action";

/// Folders of the `limit` most recently opened projects, most recent first.
///
/// `projects` is the registry sorted most recently opened first
/// ([`archipel_app::App::projects`]); projects never opened are left out.
#[must_use]
pub fn recent_folders(projects: &[RegistryEntry], limit: usize) -> Vec<PathBuf> {
    projects
        .iter()
        .filter(|p| p.last_opened.is_some())
        .filter_map(|p| match &p.location {
            ProjectLocation::SharedFile(file) => file.parent().map(Path::to_path_buf),
        })
        .take(limit)
        .collect()
}

/// Updates the icon menu from the registry (after startup, a project
/// opening or a registry change).
pub fn refresh(cx: &mut App) {
    if !cfg!(any(target_os = "windows", target_os = "macos")) {
        return;
    }
    let core = crate::windows::base_core(cx);
    let projects = match core.projects() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list the projects for the icon menu");
            return;
        }
    };
    let limit = core.settings().recent_projects;
    let state = cx.default_global::<JumpList>();
    let folders: Vec<PathBuf> = recent_folders(&projects, limit)
        .into_iter()
        .filter(|f| !state.removed.contains(f))
        .collect();
    if folders == state.shown {
        return;
    }
    state.shown.clone_from(&folders);
    publish(cx, folders);
}

/// Hands `folders` (most recent first) to the Windows jump list.
#[cfg(target_os = "windows")]
fn publish(cx: &mut App, folders: Vec<PathBuf>) {
    let entries = folders
        .into_iter()
        .map(|f| smallvec::smallvec![f])
        .collect();
    // Windows rejects a jump list with an empty task list (E_INVALIDARG),
    // dropping the recent folders too: "New Window" is always there. It
    // relaunches `archipel --dock-action 0`, read as a plain `archipel`.
    let tasks = vec![gpui_kit::MenuItem::action(
        "New Window",
        crate::workspace::NewWindow,
    )];
    let task = cx.update_jump_list(tasks, entries);
    cx.spawn(async move |cx| {
        let removed = task.await;
        if removed.is_empty() {
            return;
        }
        cx.update(|cx| {
            let state = cx.default_global::<JumpList>();
            for folder in removed.into_iter().flatten() {
                if !state.removed.contains(&folder) {
                    state.removed.push(folder);
                }
            }
        });
    })
    .detach();
}

/// Notes `folders` (most recent first) as the Dock's recent documents: the
/// last one noted is listed first.
#[cfg(target_os = "macos")]
fn publish(cx: &mut App, folders: Vec<PathBuf>) {
    for folder in folders.into_iter().rev() {
        cx.add_recent_document(&folder);
    }
}

/// No icon menu on other platforms.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn publish(_cx: &mut App, _folders: Vec<PathBuf>) {}

/// The local path of a `file://` URL (percent-decoded), `None` for any
/// other URL or a malformed escape.
#[must_use]
pub fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file:///Users/…`: the host is empty (or `localhost`).
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    let path = path
        .strip_suffix('/')
        .filter(|p| !p.is_empty())
        .unwrap_or(&path);
    Some(PathBuf::from(path))
}

/// Folders the platform asked to open, waiting for the app to run.
pub type OpenRequests = tokio::sync::mpsc::UnboundedReceiver<PathBuf>;

/// Collects the folders the platform asks to open (a Dock recent item on
/// macOS). Call before running the application, which may be launched by
/// such a request; then hand the result to [`serve_open_urls`].
pub fn on_open_urls(application: &gpui_kit::Application) -> OpenRequests {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    application.on_open_urls(move |urls| {
        for url in urls {
            if let Some(path) = file_url_path(&url) {
                // The receiver lives as long as the app.
                let _ = tx.send(path);
            } else {
                tracing::warn!(%url, "ignoring a URL that is not a local file");
            }
        }
    });
    rx
}

/// Opens a window (or focuses the one showing that project) for each folder
/// in `requests`.
pub fn serve_open_urls(cx: &mut App, mut requests: OpenRequests) {
    cx.spawn(async move |cx| {
        while let Some(dir) = requests.recv().await {
            cx.update(|cx| crate::windows::open(cx, Some(&dir)));
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use archipel_model::ProjectId;
    use archipel_model::project::{ProjectLocation, RegistryEntry};
    use archipel_model::time::Timestamp;

    use super::{file_url_path, recent_folders};

    fn entry(file: &str, opened: Option<i64>) -> RegistryEntry {
        RegistryEntry {
            id: ProjectId(format!("file:{file}")),
            name: file.to_owned(),
            location: ProjectLocation::SharedFile(PathBuf::from(file)),
            last_opened: opened.map(Timestamp),
        }
    }

    #[test]
    fn lists_the_folders_of_opened_projects_up_to_the_limit() {
        let projects = [
            entry("/w/a/archipel.toml", Some(3)),
            entry("/w/b/archipel.toml", Some(2)),
            entry("/w/c/archipel.toml", Some(1)),
            entry("/w/never/archipel.toml", None),
        ];
        assert_eq!(
            recent_folders(&projects, 2),
            [PathBuf::from("/w/a"), PathBuf::from("/w/b")]
        );
        assert_eq!(recent_folders(&projects, 10).len(), 3);
    }

    #[test]
    fn decodes_local_file_urls() {
        assert_eq!(
            file_url_path("file:///Users/me/My%20Project/"),
            Some(PathBuf::from("/Users/me/My Project"))
        );
        assert_eq!(
            file_url_path("file://localhost/srv/repo"),
            Some(PathBuf::from("/srv/repo"))
        );
        assert_eq!(
            file_url_path("file:///caf%C3%A9"),
            Some(PathBuf::from("/café"))
        );
        assert_eq!(file_url_path("file:///"), Some(PathBuf::from("/")));
    }

    #[test]
    fn rejects_other_urls() {
        assert_eq!(file_url_path("https://example.com/x"), None);
        assert_eq!(file_url_path("file://server/share"), None);
        assert_eq!(file_url_path("file:///bad%2"), None);
        assert_eq!(file_url_path("file:///bad%zz"), None);
    }
}
