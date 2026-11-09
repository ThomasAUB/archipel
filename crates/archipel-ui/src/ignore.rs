//! Adding a working-copy file to the `.gitignore` from the staging panel:
//! a dialog proposes patterns built from the file (the file itself, its
//! name anywhere, its extension, its folders), any of which fills an
//! editable pattern field; Add appends that line to the `.gitignore` at
//! the repo root, created when missing.

use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, KeyDownEvent, SharedString, Subscription, Window, div, px,
};

use crate::staging::Staging;
use crate::theme::{color, mono_font, radius};
use crate::widgets::{button, modal, primary_button};

/// A pattern proposed for a file, with what it ignores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) pattern: String,
    pub(crate) what: String,
}

/// The open dialog, kept in the staging panel's state.
pub(crate) struct IgnoreDialog {
    /// The file (or untracked folder, ending with `/`) it was opened on.
    file: String,
    /// The file is tracked: ignoring it does not stop Git tracking it.
    tracked: bool,
    choices: Vec<Choice>,
    /// The pattern added, one of the choices or edited.
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// `segment` (a file or folder name) matched literally by a pattern:
/// the wildcard characters are escaped.
fn escape(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for c in segment.chars() {
        if matches!(c, '*' | '?' | '[' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `pattern` as a `.gitignore` line: a leading `#` or `!` and trailing
/// spaces would otherwise not be read literally.
fn line(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 2);
    if pattern.starts_with(['#', '!']) {
        out.push('\\');
    }
    let kept = pattern.trim_end_matches(' ');
    out.push_str(kept);
    for _ in kept.len()..pattern.len() {
        out.push_str("\\ ");
    }
    out
}

/// The patterns proposed for `path` (relative to the repo root, `/`
/// separated, ending with `/` for an untracked folder), the narrowest
/// first.
pub(crate) fn choices(path: &str) -> Vec<Choice> {
    let folder = path.ends_with('/');
    let path = path.trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let Some((name, parents)) = segments.split_last() else {
        return Vec::new();
    };
    let escaped: Vec<String> = segments.iter().map(|s| escape(s)).collect();
    let full = escaped.join("/");
    let parent = escaped[..parents.len()].join("/");
    let mut out = Vec::new();
    let mut push = |pattern: String, what: String| {
        let pattern = line(&pattern);
        if !out.iter().any(|c: &Choice| c.pattern == pattern) {
            out.push(Choice { pattern, what });
        }
    };
    if folder {
        push(format!("/{full}/"), format!("This folder: {path}/"));
        push(
            format!("{}/", escape(name)),
            format!("Every folder named {name}"),
        );
    } else {
        push(format!("/{full}"), format!("This file: {path}"));
        push(escape(name), format!("Every file named {name}"));
        // `.env` has no extension: the dot starts its name.
        if let Some((stem, ext)) = name.rsplit_once('.')
            && !stem.is_empty()
            && !ext.is_empty()
        {
            let ext_pattern = format!("*.{}", escape(ext));
            push(ext_pattern.clone(), format!("Every .{ext} file"));
            if !parents.is_empty() {
                push(
                    format!("/{parent}/{ext_pattern}"),
                    format!("Every .{ext} file directly in {}/", parents.join("/")),
                );
            }
        }
    }
    // The folders holding it, the closest first.
    for depth in (1..=parents.len()).rev() {
        push(
            format!("/{}/", escaped[..depth].join("/")),
            format!("The folder {}/", parents[..depth].join("/")),
        );
    }
    out
}

/// Opens the dialog on `file`.
pub(crate) fn open<V: Staging>(
    v: &mut V,
    file: String,
    tracked: bool,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let choices = choices(&file);
    let first = choices
        .first()
        .map(|c| c.pattern.clone())
        .unwrap_or_default();
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Pattern")
            .default_value(first)
    });
    input.update(cx, |i, cx| i.focus(window, cx));
    let subscription = cx.subscribe_in(&input, window, |v: &mut V, _, e: &InputEvent, _, cx| {
        if let InputEvent::PressEnter { .. } = e {
            submit(v, cx);
        }
    });
    v.panel().ignore = Some(IgnoreDialog {
        file,
        tracked,
        choices,
        input,
        _subscription: subscription,
    });
    cx.notify();
}

fn close<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    v.panel().ignore = None;
    cx.notify();
}

/// Adds the pattern of the field to the `.gitignore`, closing the dialog.
fn submit<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    let Some(dialog) = &v.panel().ignore else {
        return;
    };
    let pattern = dialog
        .input
        .read(cx)
        .value()
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    if pattern.trim().is_empty() {
        v.report("Write a pattern first".into(), cx);
        return;
    }
    v.panel().ignore = None;
    v.run_noted_job(
        archipel_ops::OpClass::Write,
        format!("Ignore {pattern}"),
        cx,
        Box::new(move |a, repo, _| {
            Ok(
                (!a.add_ignore_pattern(repo, &pattern)?)
                    .then(|| "already in .gitignore".to_owned()),
            )
        }),
    );
}

/// Picks a proposed pattern: it fills the field.
fn pick<V: Staging>(v: &mut V, pattern: String, window: &mut Window, cx: &mut Context<V>) {
    if let Some(dialog) = &v.panel().ignore {
        dialog.input.clone().update(cx, |i, cx| {
            i.set_value(pattern, window, cx);
            i.focus(window, cx);
        });
    }
    cx.notify();
}

/// The dialog, over the view, when open.
pub(crate) fn render<V: Staging>(v: &mut V, cx: &mut Context<V>) -> Option<AnyElement> {
    let dialog = v.panel().ignore.as_ref()?;
    let current = dialog.input.read(cx).value().to_string();
    let (hover, selected) = (color(cx, T::BgHover), color(cx, T::BgSelected));
    let rows: Vec<AnyElement> = dialog
        .choices
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let pattern = c.pattern.clone();
            div()
                .id(SharedString::from(format!("ignore-choice-{i}")))
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .py_1()
                .rounded(radius(cx))
                .cursor_pointer()
                .when(current == c.pattern, |d| d.bg(selected))
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |v: &mut V, _, window, cx| {
                    pick(v, pattern.clone(), window, cx);
                }))
                .child(
                    div()
                        .w(px(190.0))
                        .flex_none()
                        .truncate()
                        .font_family(mono_font(cx))
                        .child(c.pattern.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(color(cx, T::FgMuted))
                        .child(c.what.clone()),
                )
                .into_any_element()
        })
        .collect();
    let note = dialog.tracked.then(|| {
        div()
            .text_xs()
            .text_color(color(cx, T::StatusWarning))
            .child(format!(
                "{} is tracked: Git keeps tracking it once ignored. Ignoring applies to \
                 untracked files only.",
                dialog.file
            ))
    });
    let body = div()
        .id("ignore-dialog")
        .occlude()
        .on_key_down(cx.listener(|v: &mut V, e: &KeyDownEvent, _, cx| {
            if e.keystroke.key == "escape" {
                cx.stop_propagation();
                close(v, cx);
            }
        }))
        .w(px(560.0))
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .bg(color(cx, T::BgElevated))
        .border_1()
        .border_color(color(cx, T::Border))
        .rounded(radius(cx))
        .shadow_lg()
        .child(format!("Add {} to .gitignore", dialog.file))
        .child(div().flex().flex_col().children(rows))
        .child(Input::new(&dialog.input))
        .children(note)
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    button("ignore-cancel", "Cancel", cx)
                        .on_click(cx.listener(|v: &mut V, _, _, cx| close(v, cx))),
                )
                .child(
                    primary_button("ignore-ok", "Add to .gitignore", cx)
                        .on_click(cx.listener(|v: &mut V, _, _, cx| submit(v, cx))),
                ),
        );
    Some(modal(body, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(path: &str) -> Vec<String> {
        choices(path).into_iter().map(|c| c.pattern).collect()
    }

    #[test]
    fn a_file_offers_itself_its_name_its_extension_and_its_folders() {
        assert_eq!(
            patterns("src/gen/out.log"),
            [
                "/src/gen/out.log",
                "out.log",
                "*.log",
                "/src/gen/*.log",
                "/src/gen/",
                "/src/",
            ]
        );
        // At the root: no folder.
        assert_eq!(patterns("notes.tmp"), ["/notes.tmp", "notes.tmp", "*.tmp"]);
        // A dot file has no extension.
        assert_eq!(patterns("app/.env"), ["/app/.env", ".env", "/app/"]);
    }

    #[test]
    fn an_untracked_folder_offers_itself_its_name_and_its_parents() {
        assert_eq!(
            patterns("web/node_modules/"),
            ["/web/node_modules/", "node_modules/", "/web/"]
        );
    }

    #[test]
    fn special_characters_are_matched_literally() {
        assert_eq!(
            patterns("a[1]/#b*.txt"),
            [
                "/a\\[1]/#b\\*.txt",
                "\\#b\\*.txt",
                "*.txt",
                "/a\\[1]/*.txt",
                "/a\\[1]/",
            ]
        );
        assert_eq!(line("!x  "), "\\!x\\ \\ ");
    }
}
