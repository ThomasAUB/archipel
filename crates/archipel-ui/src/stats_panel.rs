//! The statistics panel (issue #93): contribution per author, commit size
//! distribution, churn against age, tag cadence, lead time to merge,
//! repository weight and (issue #94) the language breakdown, for the project
//! or one of its repos, as charts.
//!
//! Opened from the dashboard and the tree view (toolbar **Statistics…**, the
//! row menu and the tree's hover buttons). Everything is read in the
//! background when it opens; closing it cancels the reads. The language
//! breakdown can be narrowed to some folders, ticked in a folder tree.

use std::collections::{HashMap, HashSet};
use std::f32::consts::{FRAC_PI_2, TAU};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use archipel_app::dashboard::exact_time;
use archipel_app::insights::{
    AdviceLevel, FILE_BUCKETS, FolderFilter, FolderNode, FolderState, HUGE_LINES, Insights,
    LINE_BUCKETS, LanguageShare, RECENT_DAYS, TINY_LINES, human_bytes,
};
use archipel_app::{App as Core, InsightsReport};
use archipel_model::{CancelToken, RepoId, Timestamp};
use archipel_theme::ColorToken as T;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, Context, Div, EventEmitter, FontWeight, Hsla, PathBuilder, Pixels,
    SharedString, Window, canvas, div, point, px, relative,
};

use crate::theme::{color, lane, mono_font, radius, rgb_color};
use crate::widgets::{SHORT_OID, TextTooltip, button, chip};

/// What the panel reports to the workspace.
pub enum StatsPanelEvent {
    /// The panel was closed.
    Closed,
}

/// Series colors: the theme's graph lanes, in a fixed order.
const SERIES: u32 = 8;
/// Contributors shown by name; the rest fold into "Others".
const TOP_PEOPLE: usize = 7;
/// Languages shown by name; the rest fold into "Other".
const TOP_LANGUAGES: usize = 10;
/// Weeks of the activity chart.
const WEEKS: i64 = 52;
const DAY: f64 = 86_400.0;

/// Reads done so far and to do.
#[derive(Default)]
struct Progress {
    done: AtomicUsize,
    total: AtomicUsize,
}

/// The panel.
pub struct StatsPanel {
    /// Repos looked at (all shown when empty).
    scope: Vec<RepoId>,
    cancel: CancelToken,
    progress: Arc<Progress>,
    report: Option<InsightsReport>,
    /// The repo shown (index in the report), the project when `None`.
    shown: Option<usize>,
    /// The repo to show once read.
    focus: Option<RepoId>,
    /// Series index per contributor, from the project order, so a
    /// person keeps their color from one repo to another.
    people: HashMap<String, u32>,
    /// The source folders of each repo, by repo name.
    folders: HashMap<String, FolderNode>,
    /// Whether the folder tree is shown.
    picking: bool,
    /// Folders (repo, path) opened or closed against their default (a
    /// repo root is open when it is the only repo shown).
    toggled: HashSet<(String, String)>,
    /// The folders the languages count.
    filter: FolderFilter,
    /// The statistics shown, languages filtered, for (shown, filter).
    filtered: Option<((Option<usize>, FolderFilter), Insights)>,
}

impl EventEmitter<StatsPanelEvent> for StatsPanel {}

impl Drop for StatsPanel {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

// Declarative element trees; the renderers are methods for symmetry.
#[allow(clippy::unused_self, clippy::too_many_lines)]
impl StatsPanel {
    /// Opens the panel on `scope` (every shown repo when empty), showing
    /// `focus` first when given, and reads the statistics in the background.
    pub fn new(
        core: Arc<Core>,
        scope: Vec<RepoId>,
        focus: Option<RepoId>,
        cx: &mut Context<Self>,
    ) -> Self {
        let panel = Self {
            scope,
            cancel: CancelToken::new(),
            progress: Arc::new(Progress::default()),
            report: None,
            shown: None,
            focus,
            people: HashMap::new(),
            folders: HashMap::new(),
            picking: false,
            toggled: HashSet::new(),
            filter: FolderFilter::default(),
            filtered: None,
        };
        panel.load(core, cx);
        panel
    }

    fn load(&self, core: Arc<Core>, cx: &mut Context<Self>) {
        let (scope, cancel, progress) = (
            self.scope.clone(),
            self.cancel.clone(),
            self.progress.clone(),
        );
        let task = cx.background_spawn(async move {
            core.insights(&scope, &cancel, &|done, total| {
                progress.done.store(done, Ordering::Relaxed);
                progress.total.store(total, Ordering::Relaxed);
            })
        });
        // Redraws the progress while reading.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let reading = this
                    .update(cx, |p, cx| {
                        cx.notify();
                        p.report.is_none()
                    })
                    .unwrap_or(false);
                if !reading {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let report = task.await;
            let _ = this.update(cx, |p, cx| {
                p.people = report
                    .project
                    .contributors
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (c.key.clone(), u32::try_from(i).unwrap_or(0) % SERIES))
                    .collect();
                p.folders = report
                    .project
                    .repo_languages
                    .iter()
                    .map(|r| (r.repo.clone(), r.folders()))
                    .collect();
                p.shown = match (p.focus, report.repos.len()) {
                    (_, 1) => Some(0),
                    (Some(id), _) => report.repos.iter().position(|r| r.id == id),
                    _ => None,
                };
                p.report = Some(report);
                cx.notify();
            });
        })
        .detach();
    }

    fn insights(&self) -> Option<&Insights> {
        let report = self.report.as_ref()?;
        Some(match self.shown {
            Some(i) => &report.repos.get(i)?.insights,
            None => &report.project,
        })
    }

    /// The statistics shown, their languages narrowed by the path filter.
    fn filtered_insights(&mut self) -> Option<Insights> {
        let key = (self.shown, self.filter.clone());
        if self.filtered.as_ref().is_none_or(|(k, _)| *k != key) {
            let mut s = self.insights()?.clone();
            s.filter_languages(&self.filter);
            self.filtered = Some((key, s));
        }
        self.filtered.as_ref().map(|(_, s)| s.clone())
    }

    fn person_color(&self, key: &str, cx: &App) -> Hsla {
        lane(cx, self.people.get(key).copied().unwrap_or(SERIES - 1))
    }

    fn render_scope_chips(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let report = self.report.as_ref()?;
        if report.repos.len() < 2 {
            return None;
        }
        let mut chips = vec![
            chip("st-scope-all", "All repositories", self.shown.is_none(), cx)
                .on_click(cx.listener(|p, _, _, cx| {
                    p.shown = None;
                    cx.notify();
                }))
                .into_any_element(),
        ];
        for (i, repo) in report.repos.iter().enumerate() {
            chips.push(
                chip(
                    SharedString::from(format!("st-scope-{i}")),
                    repo.name.clone(),
                    self.shown == Some(i),
                    cx,
                )
                .on_click(cx.listener(move |p, _, _, cx| {
                    p.shown = Some(i);
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        Some(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(chips)
                .into_any_element(),
        )
    }

    fn render_kpis(&self, s: &Insights, cx: &App) -> Div {
        let lead = s
            .lead_time()
            .map_or_else(|| "—".to_owned(), |(median, _)| duration(median));
        let lead_hint = s.lead_time().map_or_else(
            || "no merge into the main line".to_owned(),
            |(_, p90)| format!("p90 {} · {} merges", duration(p90), s.merges_lead.len()),
        );
        let span = s.span.map_or_else(String::new, |(a, b)| {
            format!("{} → {}", short_date(a), short_date(b))
        });
        let released = s.tags.iter().filter(|t| t.last_tag.is_some()).count();
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .child(kpi("Commits", thousands(s.commits), span, cx))
            .child(kpi(
                "Contributors",
                s.contributors.len().to_string(),
                format!("{} merges besides", thousands(s.merges)),
                cx,
            ))
            .child(kpi(
                "Median commit",
                format!("{} lines", s.sizes.median_lines),
                format!("p90 {} lines", thousands(s.sizes.p90_lines)),
                cx,
            ))
            .child(kpi("Lead time to merge", lead, lead_hint, cx))
            .child(kpi(
                "Unreleased commits",
                thousands(s.unreleased()),
                if released == 0 {
                    "no tag yet".to_owned()
                } else {
                    "after the latest tag".to_owned()
                },
                cx,
            ))
            .child(kpi(
                "Git size",
                human_bytes(s.git_bytes()),
                format!("{} repo(s)", s.weights.len()),
                cx,
            ))
    }

    #[allow(clippy::cast_precision_loss)] // shares
    fn render_contribution(&self, s: &Insights, cx: &App) -> Div {
        let total_commits = s.commits.max(1) as f32;
        let total_lines = s
            .contributors
            .iter()
            .map(archipel_app::insights::Contributor::lines)
            .sum::<u64>()
            .max(1) as f32;
        let mut segments: Vec<(f32, Hsla)> = s
            .contributors
            .iter()
            .take(TOP_PEOPLE)
            .map(|c| {
                (
                    c.commits as f32 / total_commits,
                    self.person_color(&c.key, cx),
                )
            })
            .collect();
        let others: Vec<_> = s.contributors.iter().skip(TOP_PEOPLE).collect();
        let muted = color(cx, T::FgSubtle);
        if !others.is_empty() {
            let n: u64 = others.iter().map(|c| c.commits).sum();
            segments.push((n as f32 / total_commits, muted));
        }
        let top_share = segments.first().map_or(0.0, |s| s.0);
        let mut rows: Vec<AnyElement> = vec![
            div()
                .flex()
                .gap_3()
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .child(div().flex_1().child("AUTHOR"))
                .child(div().w(px(150.0)).child("SHARE OF COMMITS"))
                .child(div().w(px(150.0)).child("SHARE OF CHANGED LINES"))
                .into_any_element(),
        ];
        for (i, c) in s.contributors.iter().take(TOP_PEOPLE).enumerate() {
            let fill = self.person_color(&c.key, cx);
            let commit_share = c.commits as f32 / total_commits;
            let line_share = c.lines() as f32 / total_lines;
            let tip = vec![
                format!("{} <{}>", c.name, c.email),
                format!(
                    "{} commits ({})",
                    thousands(c.commits),
                    percent(commit_share)
                ),
                format!(
                    "+{} −{} lines ({})",
                    thousands(c.added),
                    thousands(c.removed),
                    percent(line_share)
                ),
            ];
            rows.push(
                div()
                    .id(SharedString::from(format!("st-person-{i}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .py(px(3.0))
                    .rounded(radius(cx))
                    .hover(|s| s.bg(color(cx, T::BgHover)))
                    .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(swatch(fill))
                            .child(div().truncate().child(c.name.clone())),
                    )
                    .child(share_bar(commit_share, fill, cx))
                    .child(share_bar(line_share, fill.opacity(0.55), cx))
                    .into_any_element(),
            );
        }
        if !others.is_empty() {
            let commits: u64 = others.iter().map(|c| c.commits).sum();
            let lines: u64 = others.iter().map(|c| c.lines()).sum();
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py(px(3.0))
                    .text_color(color(cx, T::FgMuted))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(swatch(muted))
                            .child(match others.len() {
                                1 => "1 other".to_owned(),
                                n => format!("{n} others"),
                            }),
                    )
                    .child(share_bar(commits as f32 / total_commits, muted, cx))
                    .child(share_bar(
                        lines as f32 / total_lines,
                        muted.opacity(0.55),
                        cx,
                    ))
                    .into_any_element(),
            );
        }
        let center = s.contributors.first().map_or_else(String::new, |c| {
            format!("{}\n{}", percent(top_share), first_name(&c.name))
        });
        card(
            "Contribution",
            "Share of commits and of changed lines per author (merges excluded).",
            cx,
        )
        .child(
            div()
                .flex()
                .gap_6()
                .items_center()
                .child(donut(segments, 150.0, &center, cx))
                .child(div().flex_1().min_w_0().flex().flex_col().children(rows)),
        )
    }

    fn render_activity(&self, s: &Insights, cx: &App) -> Option<Div> {
        let last = s.weeks.last()?.0.max(Timestamp::now().week_index());
        let counts: HashMap<i64, u32> = s.weeks.iter().copied().collect();
        let accent = color(cx, T::Accent);
        let bars: Vec<Bar> = (last - WEEKS + 1..=last)
            .map(|w| {
                let n = counts.get(&w).copied().unwrap_or(0);
                Bar {
                    value: u64::from(n),
                    color: accent,
                    label: String::new(),
                    tip: vec![
                        format!(
                            "Week of {}",
                            short_date(Timestamp(w * 7 * 86_400 - 3 * 86_400))
                        ),
                        format!("{n} commit(s)"),
                    ],
                }
            })
            .collect();
        let total: u64 = bars.iter().map(|b| b.value).sum();
        Some(
            card(
                "Activity",
                &format!("Commits per week over the last year: {}.", thousands(total)),
                cx,
            )
            .child(columns("st-week", bars, 80.0, 2.0, cx)),
        )
    }

    /// The rows of a folder and, when open, of its subfolders.
    #[allow(clippy::cast_precision_loss)] // indent
    fn folder_rows(
        &self,
        repo: &str,
        node: &FolderNode,
        depth: usize,
        single: bool,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let key = (repo.to_owned(), node.path.clone());
        let open = (node.path.is_empty() && single) != self.toggled.contains(&key);
        let state = self.filter.state(repo, &node.path);
        let label = if node.path.is_empty() {
            repo.to_owned()
        } else {
            node.name.clone()
        };
        let id = format!("{repo}/{}", node.path);
        let has_children = !node.children.is_empty();
        let toggle_open = key.clone();
        let (tick_repo, tick_path) = key;
        rows.push(
            div()
                .flex()
                .items_center()
                .gap_1()
                .h(px(22.0))
                .pl(px(depth as f32 * 16.0))
                .pr_2()
                .rounded(radius(cx))
                .hover(|s| s.bg(color(cx, T::BgHover)))
                .child(
                    div()
                        .id(SharedString::from(format!("st-fold-open-{id}")))
                        .flex_none()
                        .w(px(14.0))
                        .text_color(color(cx, T::FgMuted))
                        .when(has_children, |d| {
                            d.cursor_pointer()
                                .child(if open { "▾" } else { "▸" })
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    if !p.toggled.remove(&toggle_open) {
                                        p.toggled.insert(toggle_open.clone());
                                    }
                                    cx.notify();
                                }))
                        }),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("st-fold-tick-{id}")))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .when(state == FolderState::None, |d| {
                            d.text_color(color(cx, T::FgMuted))
                        })
                        .child(
                            div()
                                .flex_none()
                                .text_color(color(
                                    cx,
                                    if state == FolderState::None {
                                        T::FgMuted
                                    } else {
                                        T::AccentText
                                    },
                                ))
                                .child(match state {
                                    FolderState::All => "☑",
                                    FolderState::Some => "▣",
                                    FolderState::None => "☐",
                                }),
                        )
                        .child(div().truncate().child(label))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            p.filter.toggle(&tick_repo, &tick_path);
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(format!(
                            "{} lines · {} files",
                            thousands(node.lines),
                            thousands(node.files)
                        )),
                )
                .into_any_element(),
        );
        if open {
            for child in &node.children {
                self.folder_rows(repo, child, depth + 1, single, rows, cx);
            }
        }
    }

    /// The folder picker of the languages: a button showing the folder
    /// tree, what is left out, and a reset.
    fn render_folders(&self, s: &Insights, cx: &mut Context<Self>) -> Div {
        let repos: Vec<String> = s
            .repos
            .iter()
            .map(|(r, _)| r.clone())
            .filter(|r| self.folders.contains_key(r))
            .collect();
        let narrowed = repos
            .iter()
            .any(|r| self.filter.state(r, "") != FolderState::All);
        let mut bar = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                button(
                    "st-folders",
                    if self.picking {
                        "Hide folders"
                    } else {
                        "Choose folders…"
                    },
                    cx,
                )
                .on_click(cx.listener(|p, _, _, cx| {
                    p.picking = !p.picking;
                    cx.notify();
                })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(if narrowed {
                        "Counting the folders ticked only."
                    } else {
                        "Counting every folder."
                    }),
            );
        if narrowed {
            let clear = repos.clone();
            bar = bar.child(button("st-folders-all", "Count every folder", cx).on_click(
                cx.listener(move |p, _, _, cx| {
                    for r in &clear {
                        p.filter.clear(r);
                    }
                    cx.notify();
                }),
            ));
        }
        let mut out = div().flex().flex_col().gap_2().child(bar);
        if self.picking {
            let single = repos.len() == 1;
            let mut rows = Vec::new();
            for r in &repos {
                if let Some(node) = self.folders.get(r) {
                    self.folder_rows(r, node, 0, single, &mut rows, cx);
                }
            }
            out = out.child(
                div()
                    .id("st-folder-tree")
                    .max_h(px(320.0))
                    .overflow_y_scrollbar()
                    .p_1()
                    .border_1()
                    .border_color(color(cx, T::Border))
                    .rounded(radius(cx))
                    .flex()
                    .flex_col()
                    .children(rows),
            );
        }
        out
    }

    #[allow(clippy::cast_precision_loss)] // shares
    fn render_languages(&self, s: &Insights, cx: &mut Context<Self>) -> Div {
        let card = card(
            "Languages",
            "Share of each language in the code of the walked branch, by lines and by files \
             (from file extensions; vendored, generated and binary files left out).",
            cx,
        );
        if s.repo_languages.is_empty() {
            return card.child(hint("The files could not be read.", cx));
        }
        let card = card.child(self.render_folders(s, cx));
        let (filtered_files, filtered_lines) = s.repo_languages.iter().fold((0, 0), |(f, l), r| {
            (f + r.filtered_files, l + r.filtered_lines)
        });
        if s.languages.is_empty() {
            return card.child(hint(
                if filtered_files > 0 {
                    "No source file in the folders ticked."
                } else {
                    "No source file in a known language."
                },
                cx,
            ));
        }
        let muted = color(cx, T::FgSubtle);
        let (shown, other) = fold_languages(&s.languages);
        let lines = s.languages.iter().map(|l| l.lines).sum::<u64>().max(1) as f32;
        let files = s.languages.iter().map(|l| l.files).sum::<u64>().max(1) as f32;
        let fill = |l: &LanguageShare| {
            if l.name.is_empty() {
                muted
            } else {
                rgb_color(l.color)
            }
        };
        let mut rows: Vec<AnyElement> = vec![
            div()
                .flex()
                .gap_3()
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .child(div().flex_1().child("LANGUAGE"))
                .child(div().w(px(90.0)).flex().justify_end().child("LINES"))
                .child(div().w(px(150.0)).child("SHARE OF LINES"))
                .child(div().w(px(150.0)).child("SHARE OF FILES"))
                .into_any_element(),
        ];
        for (i, l) in shown.iter().chain(other.as_ref()).enumerate() {
            let tip = language_tip(l, lines, files);
            let label = if l.name.is_empty() {
                "Other".to_owned()
            } else {
                l.name.clone()
            };
            rows.push(
                div()
                    .id(SharedString::from(format!("st-lang-{i}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .py(px(3.0))
                    .rounded(radius(cx))
                    .hover(|s| s.bg(color(cx, T::BgHover)))
                    .when(l.name.is_empty(), |d| d.text_color(color(cx, T::FgMuted)))
                    .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(swatch(fill(l)))
                            .child(div().truncate().child(label)),
                    )
                    .child(
                        div()
                            .w(px(90.0))
                            .flex()
                            .justify_end()
                            .text_color(color(cx, T::FgMuted))
                            .child(thousands(l.lines)),
                    )
                    .child(share_bar(l.lines as f32 / lines, fill(l), cx))
                    .child(share_bar(l.files as f32 / files, fill(l).opacity(0.55), cx))
                    .into_any_element(),
            );
        }
        let mut card = card
            .child(language_bar(
                "st-lang-bar",
                &s.languages,
                px(10.0),
                muted,
                cx,
            ))
            .child(div().flex().flex_col().children(rows));
        if s.repo_languages.len() > 1 {
            let max = s
                .repo_languages
                .iter()
                .map(archipel_app::insights::RepoLanguages::lines)
                .max()
                .unwrap_or(1)
                .max(1) as f32;
            card = card.child(sub_title("Per repository", cx)).children(
                s.repo_languages.iter().enumerate().map(|(i, r)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py(px(2.0))
                        .child(div().w(px(130.0)).truncate().child(r.repo.clone()))
                        .child(div().flex_1().min_w_0().child(
                            div().w(relative((r.lines() as f32 / max).max(0.02))).child(
                                language_bar(
                                    &format!("st-lang-repo-{i}"),
                                    &r.languages,
                                    px(8.0),
                                    muted,
                                    cx,
                                ),
                            ),
                        ))
                        .child(
                            div()
                                .w(px(110.0))
                                .text_xs()
                                .text_color(color(cx, T::FgMuted))
                                .child(format!("{} lines", thousands(r.lines()))),
                        )
                }),
            );
        }
        let (excluded_files, excluded_lines) = s.repo_languages.iter().fold((0, 0), |(f, l), r| {
            (f + r.excluded_files, l + r.excluded_lines)
        });
        card.child(hint(
            &format!(
                "{} lines in {} files.{}{}",
                thousands(s.languages.iter().map(|l| l.lines).sum()),
                thousands(s.languages.iter().map(|l| l.files).sum()),
                match filtered_files {
                    0 => String::new(),
                    n => format!(
                        " {} files in folders left out ({} lines).",
                        thousands(n),
                        thousands(filtered_lines)
                    ),
                },
                match excluded_files {
                    0 => String::new(),
                    n => format!(
                        " {} vendored or generated files left out ({} lines).",
                        thousands(n),
                        thousands(excluded_lines)
                    ),
                }
            ),
            cx,
        ))
    }

    fn render_sizes(&self, s: &Insights, cx: &App) -> Div {
        let accent = color(cx, T::Accent);
        let warn = color(cx, T::StatusWarning);
        let line_bars: Vec<Bar> = LINE_BUCKETS
            .iter()
            .zip(s.sizes.lines)
            .enumerate()
            .map(|(i, ((_, label), n))| Bar {
                value: n,
                // Typo-sized and huge commits stand out.
                color: if i <= 1 || i >= LINE_BUCKETS.len() - 2 {
                    warn
                } else {
                    accent
                },
                label: (*label).to_owned(),
                tip: vec![
                    format!("{label} lines changed"),
                    format!("{} commit(s)", thousands(n)),
                ],
            })
            .collect();
        let file_bars: Vec<Bar> = FILE_BUCKETS
            .iter()
            .zip(s.sizes.files)
            .map(|((_, label), n)| Bar {
                value: n,
                color: accent,
                label: (*label).to_owned(),
                tip: vec![
                    format!("{label} file(s) changed"),
                    format!("{} commit(s)", thousands(n)),
                ],
            })
            .collect();
        let max_p90 = s
            .sizes
            .authors
            .iter()
            .take(TOP_PEOPLE)
            .map(|a| a.p90_lines)
            .max()
            .unwrap_or(1)
            .max(1);
        let mut authors: Vec<AnyElement> = vec![
            div()
                .flex()
                .gap_3()
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .child(div().w(px(150.0)).child("AUTHOR"))
                .child(div().flex_1().child("LINES PER COMMIT: MEDIAN ● AND P90 ▬"))
                .child(div().w(px(110.0)).child("TINY / HUGE"))
                .into_any_element(),
        ];
        for (i, a) in s.sizes.authors.iter().take(TOP_PEOPLE).enumerate() {
            let fill = self.person_color(&a.key, cx);
            let tip = vec![
                a.name.clone(),
                format!(
                    "{} commits · median {} lines in {} file(s) · p90 {} lines",
                    a.commits, a.median_lines, a.median_files, a.p90_lines
                ),
                format!(
                    "{} tiny (≤ {TINY_LINES} lines), {} huge (> {HUGE_LINES} lines)",
                    a.tiny, a.huge
                ),
            ];
            authors.push(
                div()
                    .id(SharedString::from(format!("st-size-{i}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .py(px(3.0))
                    .rounded(radius(cx))
                    .hover(|s| s.bg(color(cx, T::BgHover)))
                    .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
                    .child(
                        div()
                            .w(px(150.0))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(swatch(fill))
                            .child(div().truncate().child(a.name.clone())),
                    )
                    .child(range_bar(
                        log_frac(a.median_lines, max_p90),
                        log_frac(a.p90_lines, max_p90),
                        fill,
                        cx,
                    ))
                    .child(
                        div()
                            .w(px(110.0))
                            .text_xs()
                            .text_color(color(cx, T::FgMuted))
                            .child(format!("{} / {}", a.tiny, a.huge)),
                    )
                    .into_any_element(),
            );
        }
        let mut lists = div().flex().gap_6();
        if !s.sizes.biggest.is_empty() {
            let rows = s.sizes.biggest.iter().take(6).map(|b| {
                list_line(
                    format!("{} lines · {} files", thousands(b.lines), b.files),
                    format!(
                        "{} {} · {} · {}",
                        b.oid.short(SHORT_OID),
                        b.subject,
                        b.author,
                        b.repo
                    ),
                    cx,
                )
            });
            lists = lists.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(sub_title("Largest commits", cx))
                    .children(rows),
            );
        }
        let streaks: Vec<Div> = s
            .sizes
            .streaks
            .iter()
            .take(6)
            .map(|t| {
                list_line(
                    format!("{} in a row", t.commits),
                    format!(
                        "{} · {} → {} · {}",
                        t.author,
                        short_date(t.from),
                        short_date(t.to),
                        t.repo
                    ),
                    cx,
                )
            })
            .collect();
        lists = lists.child(
            div()
                .flex_1()
                .min_w_0()
                .child(sub_title(
                    &format!("Streaks of tiny commits (≤ {TINY_LINES} lines)"),
                    cx,
                ))
                .when(streaks.is_empty(), |d| {
                    d.child(hint("No streak: small fixes come alone.", cx))
                })
                .children(streaks),
        );
        card(
            "Commit size",
            "How big commits are: huge ones and streaks of “fix typo” commits are worth a second look, \
             especially from agents.",
            cx,
        )
        .child(
            div()
                .flex()
                .gap_6()
                .child(
                    div()
                        .flex_1()
                        .child(sub_title("Lines changed per commit", cx))
                        .child(columns("st-lines", line_bars, 110.0, 6.0, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .child(sub_title("Files changed per commit", cx))
                        .child(columns("st-files", file_bars, 110.0, 6.0, cx)),
                ),
        )
        .child(div().flex().flex_col().children(authors))
        .child(lists)
    }

    #[allow(clippy::cast_precision_loss)] // chart positions
    fn render_aging(&self, s: &Insights, cx: &App) -> Div {
        let card = card(
            "Churn against age",
            &format!(
                "Old files still changing often: a sharper hotspot signal than raw churn. \
                 Age since the first commit (→), commits in the last {RECENT_DAYS} days (↑)."
            ),
            cx,
        );
        if s.aging.is_empty() {
            return card.child(hint(
                "No file older than six months changed twice in the last 90 days.",
                cx,
            ));
        }
        let top_age = s.aging.iter().map(|a| a.age_days).max().unwrap_or(1);
        let low_age = s.aging.iter().map(|a| a.age_days).min().unwrap_or(0);
        let (min_age, age_span) = (low_age as f32, (top_age - low_age).max(1) as f32);
        let top_recent = s
            .aging
            .iter()
            .map(|a| a.recent_commits)
            .max()
            .unwrap_or(1)
            .max(1);
        let max_recent = top_recent as f32;
        let points: Vec<Dot> = s
            .aging
            .iter()
            .enumerate()
            .map(|(i, a)| Dot {
                x: (a.age_days as f32 - min_age) / age_span,
                y: a.recent_commits as f32 / max_recent,
                size: if i == 0 { 14.0 } else { 9.0 },
                color: lane(cx, 3),
                tip: vec![
                    format!("{} · {}", a.path, a.repo),
                    format!("{} days old, {} commits in all", a.age_days, a.commits),
                    format!(
                        "{} commits, {} lines in the last {RECENT_DAYS} days",
                        a.recent_commits,
                        thousands(a.recent_churn)
                    ),
                ],
            })
            .collect();
        let rows = s.aging.iter().take(8).map(|a| {
            list_line(
                format!("{}×", a.recent_commits),
                format!("{} · {} · {} d old", a.path, a.repo, a.age_days),
                cx,
            )
        });
        card.child(
            div()
                .flex()
                .gap_6()
                .child(scatter(
                    "st-aging",
                    points,
                    (format!("{top_age} d old"), format!("{low_age} d old")),
                    top_recent.to_string(),
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(sub_title("Top aging hotspots", cx))
                        .children(rows),
                ),
        )
    }

    #[allow(clippy::cast_precision_loss)] // chart positions
    fn render_tags(&self, s: &Insights, cx: &App) -> Div {
        let card = card(
            "Tags and releases",
            "Tags on a time line per repo, the time between them, and the commits after the latest \
             one (unreleased work).",
            cx,
        );
        let (start, end) = s
            .tags
            .iter()
            .flat_map(|t| t.tags.iter().map(|p| p.time.0))
            .chain(s.span.map(|(a, _)| a.0))
            .fold((i64::MAX, Timestamp::now().0), |(a, b), t| {
                (a.min(t), b.max(t))
            });
        let width = (end - start).max(1) as f32;
        let accent = color(cx, T::Accent);
        let warn = color(cx, T::StatusWarning);
        let rows = s.tags.iter().enumerate().map(|(r, t)| {
            let dots = t.tags.iter().enumerate().map(|(i, p)| {
                let x = (p.time.0 - start) as f32 / width;
                let tip = vec![p.name.clone(), short_date(p.time)];
                div()
                    .id(SharedString::from(format!("st-tag-{r}-{i}")))
                    .absolute()
                    .top(px(3.0))
                    .left(relative(x))
                    .ml(px(-5.0))
                    .size(px(10.0))
                    .rounded_full()
                    .bg(accent)
                    .border_2()
                    .border_color(color(cx, T::BgPanel))
                    .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
            });
            let unreleased = match (&t.last_tag, t.unreleased) {
                (_, 0) => "all released".to_owned(),
                (Some(tag), n) => format!("{} commits after {}", thousands(n), tag.name),
                (None, n) => format!("{} commits, no tag", thousands(n)),
            };
            let cadence = t
                .median_interval_days
                .map_or_else(|| "—".to_owned(), |d| format!("every {d} d"));
            div()
                .flex()
                .items_center()
                .gap_3()
                .py_1()
                .child(div().w(px(130.0)).truncate().child(t.repo.clone()))
                .child(
                    div()
                        .flex_1()
                        .relative()
                        .h(px(16.0))
                        .child(
                            div()
                                .absolute()
                                .top(px(7.0))
                                .left_0()
                                .right_0()
                                .h(px(2.0))
                                .rounded_full()
                                .bg(color(cx, T::BorderSubtle)),
                        )
                        .children(dots),
                )
                .child(
                    div()
                        .w(px(80.0))
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(cadence),
                )
                .child(
                    div()
                        .w(px(190.0))
                        .text_xs()
                        .text_color(if t.unreleased > 0 && t.last_tag.is_some() {
                            warn
                        } else {
                            color(cx, T::FgMuted)
                        })
                        .child(unreleased),
                )
        });
        card.child(
            div()
                .flex()
                .justify_between()
                .pl(px(142.0))
                .pr(px(282.0))
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .child(short_date(Timestamp(start)))
                .child(short_date(Timestamp(end))),
        )
        .children(rows)
    }

    #[allow(clippy::cast_precision_loss)] // chart positions
    fn render_lead(&self, s: &Insights, cx: &App) -> Div {
        let card = card(
            "Lead time to merge",
            "From a branch's first commit to its merge into the main line, from local history.",
            cx,
        );
        let Some((median, p90)) = s.lead_time() else {
            return card.child(hint(
                "No merge commit on the main line: branches are rebased or fast-forwarded.",
                cx,
            ));
        };
        let first = s.merges_lead.first().map_or(0, |m| m.merged_at.0);
        let last = s.merges_lead.last().map_or(1, |m| m.merged_at.0);
        let span = (last - first).max(1) as f32;
        let top = s
            .merges_lead
            .iter()
            .map(|m| m.lead_secs)
            .max()
            .unwrap_or(1)
            .max(1);
        let accent = color(cx, T::Accent);
        let warn = color(cx, T::StatusWarning);
        let points: Vec<Dot> = s
            .merges_lead
            .iter()
            .map(|m| Dot {
                x: (m.merged_at.0 - first) as f32 / span,
                y: log_frac(m.lead_secs, top),
                size: 8.0,
                color: if m.lead_secs > p90 { warn } else { accent },
                tip: vec![
                    format!("{} · {}", m.branch, m.repo),
                    format!("merged {}", short_date(m.merged_at)),
                    format!(
                        "{} from its first commit, {} commit(s)",
                        duration(m.lead_secs),
                        m.commits
                    ),
                ],
            })
            .collect();
        let mut slowest: Vec<_> = s.merges_lead.iter().collect();
        slowest.sort_by_key(|m| std::cmp::Reverse(m.lead_secs));
        let rows = slowest.iter().take(8).map(|m| {
            list_line(
                duration(m.lead_secs),
                format!("{} · {} · {}", m.branch, m.repo, short_date(m.merged_at)),
                cx,
            )
        });
        card.child(
            div()
                .flex()
                .gap_6()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(scatter(
                            "st-lead",
                            points,
                            (short_date(Timestamp(last)), short_date(Timestamp(first))),
                            duration(top),
                            cx,
                        ))
                        .child(hint(
                            &format!(
                                "Median {} · p90 {} · log scale; above p90 in warning color.",
                                duration(median),
                                duration(p90)
                            ),
                            cx,
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(sub_title("Slowest merges", cx))
                        .children(rows),
                ),
        )
    }

    #[allow(clippy::cast_precision_loss)] // shares
    fn render_weight(&self, s: &Insights, cx: &App) -> Div {
        let card = card(
            "Repository weight",
            "Size of each Git directory, packed and loose objects, and the largest blobs in history.",
            cx,
        );
        if s.weights.is_empty() {
            return card.child(hint("The weight could not be measured.", cx));
        }
        let max = s
            .weights
            .iter()
            .map(|w| w.storage.git_dir_bytes)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let packed = color(cx, T::Accent);
        let loose = color(cx, T::StatusWarning);
        let rows =
            s.weights.iter().enumerate().map(|(i, w)| {
                let st = &w.storage;
                let other = st
                    .git_dir_bytes
                    .saturating_sub(st.pack_bytes + st.loose_bytes);
                let frac = |b: u64| b as f32 / max;
                let tip = vec![
                    format!("{}: {}", w.repo, human_bytes(st.git_dir_bytes)),
                    format!(
                        "packs: {} in {} pack(s), {} objects",
                        human_bytes(st.pack_bytes),
                        st.packs,
                        thousands(st.packed_objects)
                    ),
                    format!(
                        "loose: {} in {} objects",
                        human_bytes(st.loose_bytes),
                        thousands(st.loose_objects)
                    ),
                    format!("other (index, logs, LFS…): {}", human_bytes(other)),
                ];
                let advice = w.advice.iter().map(|a| {
                    let (glyph, tone) = match a.level {
                        AdviceLevel::Good => ("✓", T::StatusSuccess),
                        AdviceLevel::Hint => ("ⓘ", T::Accent),
                        AdviceLevel::Warning => ("⚠", T::StatusWarning),
                    };
                    div()
                        .flex()
                        .gap_1()
                        .text_xs()
                        .child(div().text_color(color(cx, tone)).child(glyph))
                        .child(
                            div()
                                .text_color(color(cx, T::FgMuted))
                                .child(a.text.clone()),
                        )
                });
                let blobs =
                    st.largest_blobs.iter().take(4).map(|b| {
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .text_color(color(cx, T::FgMuted))
                            .child(div().w(px(70.0)).child(human_bytes(b.bytes)))
                            .child(div().truncate().font_family(mono_font(cx)).child(
                                if b.reachable {
                                    b.path.clone()
                                } else {
                                    format!("{} (unreachable)", b.oid.short(SHORT_OID))
                                },
                            ))
                    });
                let flags = [st.shallow.then_some("shallow"), st.lfs.then_some("LFS")]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                div()
                    .flex()
                    .gap_3()
                    .py_1()
                    .child(
                        div()
                            .w(px(130.0))
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(w.repo.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(color(cx, T::FgSubtle))
                                    .child(flags),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("st-weight-{i}")))
                                            .flex_1()
                                            .h(px(12.0))
                                            .flex()
                                            .gap(px(2.0))
                                            .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
                                            .child(segment(frac(st.pack_bytes), packed))
                                            .child(segment(frac(st.loose_bytes), loose))
                                            .child(segment(frac(other), color(cx, T::FgSubtle))),
                                    )
                                    .child(
                                        div()
                                            .w(px(80.0))
                                            .text_xs()
                                            .child(human_bytes(st.git_dir_bytes)),
                                    ),
                            )
                            .children(advice)
                            .children(blobs),
                    )
            });
        card.child(
            div()
                .flex()
                .gap_4()
                .text_xs()
                .text_color(color(cx, T::FgMuted))
                .child(legend(packed, "packs"))
                .child(legend(loose, "loose objects"))
                .child(legend(color(cx, T::FgSubtle), "other")),
        )
        .children(rows)
    }
}

impl Render for StatsPanel {
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match (&self.report, self.shown) {
            (Some(r), Some(i)) => r.repos.get(i).map_or_else(
                || "Statistics".to_owned(),
                |x| format!("Statistics · {}", x.name),
            ),
            (Some(r), None) => format!("Statistics · {} repositories", r.repos.len()),
            (None, _) => "Statistics".to_owned(),
        };
        let walked = self.insights().map(|s| {
            let mut branches: Vec<&str> = s.repos.iter().map(|(_, b)| b.as_str()).collect();
            branches.sort_unstable();
            branches.dedup();
            format!(
                "History of {} · read {}",
                branches.join(", "),
                exact_time(Timestamp::now())
            )
        });
        let shown = self.filtered_insights();
        let mut body = div()
            .id("st-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .flex()
            .flex_col()
            .gap_4()
            .pr_2();
        match shown {
            None => {
                let (done, total) = (
                    self.progress.done.load(Ordering::Relaxed),
                    self.progress.total.load(Ordering::Relaxed),
                );
                let frac = if total == 0 {
                    0.0
                } else {
                    #[allow(clippy::cast_precision_loss)]
                    let f = done as f32 / total as f32;
                    f
                };
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .pt_12()
                        .text_color(color(cx, T::FgMuted))
                        .child(if total == 0 {
                            "Reading the history…".to_owned()
                        } else {
                            format!("Reading the history… {done} of {total} repositories")
                        })
                        .child(div().w(px(320.0)).child(share_bar_plain(
                            frac,
                            color(cx, T::Accent),
                            cx,
                        ))),
                );
            }
            Some(s) if s.commits == 0 && s.merges == 0 => {
                body = body.child(hint("No commit to look at.", cx));
            }
            Some(s) => {
                body = body
                    .child(self.render_kpis(&s, cx))
                    .child(self.render_contribution(&s, cx))
                    .children(self.render_activity(&s, cx))
                    .child(self.render_languages(&s, cx))
                    .child(self.render_sizes(&s, cx))
                    .child(self.render_aging(&s, cx))
                    .child(self.render_lead(&s, cx))
                    .child(self.render_tags(&s, cx))
                    .child(self.render_weight(&s, cx));
            }
        }
        let chips = self.render_scope_chips(cx);
        let errors: Vec<Div> = self
            .report
            .iter()
            .flat_map(|r| &r.errors)
            .map(|e| {
                div()
                    .text_xs()
                    .text_color(color(cx, T::StatusError))
                    .child(e.clone())
            })
            .collect();
        div()
            .id("stats-panel")
            .occlude()
            .w(relative(0.92))
            .max_w(px(1180.0))
            .h(relative(0.9))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Border))
            .rounded(radius(cx) * 2.0)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .children(walked.map(|w| {
                                div().text_xs().text_color(color(cx, T::FgSubtle)).child(w)
                            })),
                    )
                    .child(
                        button("st-close", "Close", cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(StatsPanelEvent::Closed))),
                    ),
            )
            .children(chips)
            .children(errors)
            .child(body)
    }
}

// ---------------------------------------------------------------------------
// Chart pieces.

/// One bar of a column chart.
struct Bar {
    value: u64,
    color: Hsla,
    /// Label under the bar (empty: none).
    label: String,
    tip: Vec<String>,
}

/// One dot of a scatter plot, positions in 0..=1.
struct Dot {
    x: f32,
    y: f32,
    size: f32,
    color: Hsla,
    tip: Vec<String>,
}

/// A titled card.
fn card(title: &str, subtitle: &str, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(radius(cx) * 2.0)
        .bg(color(cx, T::BgPanel))
        .border_1()
        .border_color(color(cx, T::BorderSubtle))
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(subtitle.to_owned()),
                ),
        )
}

/// A headline number.
fn kpi(label: &str, value: String, hint: String, cx: &App) -> Div {
    div()
        .flex_1()
        .min_w(px(150.0))
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(radius(cx) * 2.0)
        .bg(color(cx, T::BgPanel))
        .border_1()
        .border_color(color(cx, T::BorderSubtle))
        .child(
            div()
                .text_xs()
                .text_color(color(cx, T::FgMuted))
                .child(label.to_uppercase()),
        )
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
        .child(
            div()
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .truncate()
                .child(hint),
        )
}

fn sub_title(text: &str, cx: &App) -> Div {
    div()
        .pb_1()
        .text_xs()
        .text_color(color(cx, T::FgSubtle))
        .child(text.to_uppercase())
}

fn hint(text: &str, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(color(cx, T::FgMuted))
        .child(text.to_owned())
}

/// A list line: a value in front, then a description.
fn list_line(value: String, text: String, cx: &App) -> Div {
    div()
        .flex()
        .gap_2()
        .py(px(2.0))
        .text_xs()
        .child(
            div()
                .w(px(170.0))
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(color(cx, T::FgMuted))
                .child(text),
        )
}

fn swatch(fill: Hsla) -> Div {
    div().size(px(10.0)).flex_none().rounded(px(3.0)).bg(fill)
}

fn legend(fill: Hsla, label: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(swatch(fill))
        .child(label)
}

/// A horizontal track filled to `frac`, its percentage beside it.
fn share_bar(frac: f32, fill: Hsla, cx: &App) -> Div {
    div()
        .w(px(150.0))
        .flex()
        .items_center()
        .gap_2()
        .child(div().flex_1().child(share_bar_plain(frac, fill, cx)))
        .child(
            div()
                .w(px(40.0))
                .text_xs()
                .text_color(color(cx, T::FgMuted))
                .child(percent(frac)),
        )
}

fn share_bar_plain(frac: f32, fill: Hsla, cx: &App) -> Div {
    div()
        .h(px(8.0))
        .w_full()
        .rounded_full()
        .bg(color(cx, T::BgHover))
        .child(
            div()
                .h_full()
                .w(relative(frac.clamp(0.0, 1.0)))
                .min_w(px(if frac > 0.0 { 3.0 } else { 0.0 }))
                .rounded_full()
                .bg(fill),
        )
}

/// A bar from `from` to `to` (fractions of the track) with a dot at `from`.
fn range_bar(from: f32, to: f32, fill: Hsla, cx: &App) -> Div {
    let (from, to) = (from.clamp(0.0, 1.0), to.clamp(0.0, 1.0));
    div()
        .flex_1()
        .h(px(14.0))
        .relative()
        .child(
            div()
                .absolute()
                .top(px(6.0))
                .left_0()
                .right_0()
                .h(px(2.0))
                .bg(color(cx, T::BorderSubtle)),
        )
        .child(
            div()
                .absolute()
                .top(px(4.0))
                .left(relative(from))
                .w(relative((to - from).max(0.0)))
                .h(px(6.0))
                .rounded_full()
                .bg(fill.opacity(0.5)),
        )
        .child(
            div()
                .absolute()
                .top(px(2.0))
                .left(relative(from))
                .ml(px(-5.0))
                .size(px(10.0))
                .rounded_full()
                .bg(fill)
                .border_2()
                .border_color(color(cx, T::BgPanel)),
        )
}

/// The [`TOP_LANGUAGES`] first languages, and the rest summed into one
/// share with an empty name.
fn fold_languages(languages: &[LanguageShare]) -> (&[LanguageShare], Option<LanguageShare>) {
    if languages.len() <= TOP_LANGUAGES {
        return (languages, None);
    }
    let (shown, rest) = languages.split_at(TOP_LANGUAGES);
    let other = LanguageShare {
        name: String::new(),
        color: [0; 3],
        lines: rest.iter().map(|l| l.lines).sum(),
        files: rest.iter().map(|l| l.files).sum(),
    };
    (shown, Some(other))
}

/// Tooltip of a language: its lines and files, and their shares.
#[allow(clippy::cast_precision_loss)] // shares
fn language_tip(l: &LanguageShare, lines: f32, files: f32) -> Vec<String> {
    vec![
        if l.name.is_empty() {
            "Other languages".to_owned()
        } else {
            l.name.clone()
        },
        format!(
            "{} lines ({})",
            thousands(l.lines),
            percent(l.lines as f32 / lines)
        ),
        format!(
            "{} files ({})",
            thousands(l.files),
            percent(l.files as f32 / files)
        ),
    ]
}

/// A language bar, as on a GitHub repo page: one segment per language,
/// as wide as its share of the lines, each with its tooltip.
#[allow(clippy::cast_precision_loss)] // shares
fn language_bar(
    id: &str,
    languages: &[LanguageShare],
    height: Pixels,
    other_fill: Hsla,
    cx: &App,
) -> Div {
    let (shown, other) = fold_languages(languages);
    let lines = languages.iter().map(|l| l.lines).sum::<u64>().max(1) as f32;
    let files = languages.iter().map(|l| l.files).sum::<u64>().max(1) as f32;
    let segments = shown
        .iter()
        .chain(other.as_ref())
        .enumerate()
        .map(|(i, l)| {
            let tip = language_tip(l, lines, files);
            let fill = if l.name.is_empty() {
                other_fill
            } else {
                rgb_color(l.color)
            };
            div()
                .id(SharedString::from(format!("{id}-{i}")))
                .h_full()
                .w(relative(l.lines as f32 / lines))
                .when(l.lines > 0, |d| d.min_w(px(2.0)))
                .bg(fill)
                .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
        });
    div()
        .w_full()
        .h(height)
        .flex()
        .overflow_hidden()
        .rounded_full()
        .bg(color(cx, T::BgHover))
        .children(segments)
}

/// A segment of a stacked bar, `frac` of the track wide.
fn segment(frac: f32, fill: Hsla) -> Div {
    div()
        .h_full()
        .w(relative(frac.clamp(0.0, 1.0)))
        .when(frac > 0.0, |d| d.min_w(px(2.0)))
        .rounded(px(3.0))
        .bg(fill)
}

/// A column chart: bars rising from a baseline, each with a tooltip.
#[allow(clippy::cast_precision_loss)] // heights
fn columns(id: &str, bars: Vec<Bar>, height: f32, gap: f32, cx: &App) -> Div {
    let max = bars.iter().map(|b| b.value).max().unwrap_or(0).max(1) as f32;
    let labelled = bars.iter().any(|b| !b.label.is_empty());
    let hover = color(cx, T::BgHover);
    let cols = bars.into_iter().enumerate().map(|(i, b)| {
        let h = if b.value == 0 {
            0.0
        } else {
            (b.value as f32 / max * height).max(2.0)
        };
        let tip = b.tip;
        div()
            .id(SharedString::from(format!("{id}-{i}")))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .child(
                div()
                    .w_full()
                    .h(px(height))
                    .flex()
                    .flex_col()
                    .justify_end()
                    .rounded_t(px(4.0))
                    .hover(move |s| s.bg(hover))
                    .child(div().w_full().h(px(h)).rounded_t(px(4.0)).bg(b.color)),
            )
            .when(labelled, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(color(cx, T::FgSubtle))
                        .truncate()
                        .child(b.label),
                )
            })
            .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
    });
    div()
        .flex()
        .flex_col()
        .child(div().flex().items_end().gap(px(gap)).children(cols))
        .child(div().h(px(1.0)).bg(color(cx, T::Border)))
}

/// A scatter plot of dots (positions in 0..=1), the x axis labelled at its
/// ends (right, left) and the y axis at its top.
fn scatter(
    id: &str,
    dots: Vec<Dot>,
    (x_end, x_start): (String, String),
    y_top: String,
    cx: &App,
) -> Div {
    const W: f32 = 380.0;
    const H: f32 = 180.0;
    let grid = color(cx, T::BorderSubtle);
    let ring = color(cx, T::BgPanel);
    let marks = dots.into_iter().enumerate().map(|(i, d)| {
        let tip = d.tip;
        div()
            .id(SharedString::from(format!("{id}-{i}")))
            .absolute()
            .left(px(d.x.clamp(0.0, 1.0) * W - d.size / 2.0))
            .top(px((1.0 - d.y.clamp(0.0, 1.0)) * H - d.size / 2.0))
            .size(px(d.size))
            .rounded_full()
            .bg(d.color.opacity(0.85))
            .border_1()
            .border_color(ring)
            .hover(|s| s.bg(d.color))
            .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
    });
    let lines = (0..=3).map(|k| {
        #[allow(clippy::cast_precision_loss)]
        let y = H * k as f32 / 3.0;
        div()
            .absolute()
            .left_0()
            .top(px(y))
            .w(px(W))
            .h(px(1.0))
            .bg(grid)
    });
    let axis = |text: String| {
        div()
            .text_xs()
            .text_color(color(cx, T::FgSubtle))
            .child(text)
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .flex_none()
        .child(axis(y_top))
        .child(
            div()
                .relative()
                .w(px(W))
                .h(px(H))
                .children(lines)
                .children(marks),
        )
        .child(
            div()
                .w(px(W))
                .flex()
                .justify_between()
                .child(axis(x_start))
                .child(axis(x_end)),
        )
}

/// A donut of `segments` (fractions summing to at most 1), `center` written
/// in its hole.
fn donut(segments: Vec<(f32, Hsla)>, size: f32, center: &str, cx: &App) -> Div {
    let ring = canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let c = point(
                bounds.origin.x + bounds.size.width / 2.0,
                bounds.origin.y + bounds.size.height / 2.0,
            );
            let outer = f32::from(bounds.size.width.min(bounds.size.height)) / 2.0;
            let inner = outer * 0.62;
            let at = |r: f32, a: f32| point(c.x + px(r * a.cos()), c.y + px(r * a.sin()));
            let mut start = -FRAC_PI_2;
            for (frac, fill) in &segments {
                let sweep = frac * TAU;
                if sweep <= 0.0 {
                    continue;
                }
                // A thin surface gap between segments.
                let pad = if segments.len() > 1 { 0.012 } else { 0.0 };
                let (a0, a1) = (start + pad, start + sweep - pad);
                start += sweep;
                if a1 <= a0 {
                    continue;
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let steps = ((a1 - a0) / 0.04).ceil().max(2.0) as usize;
                let mut pts = Vec::with_capacity(2 * steps + 2);
                for k in 0..=steps {
                    #[allow(clippy::cast_precision_loss)]
                    let a = a0 + (a1 - a0) * k as f32 / steps as f32;
                    pts.push(at(outer, a));
                }
                for k in (0..=steps).rev() {
                    #[allow(clippy::cast_precision_loss)]
                    let a = a0 + (a1 - a0) * k as f32 / steps as f32;
                    pts.push(at(inner, a));
                }
                let mut path = PathBuilder::fill();
                path.add_polygon(&pts, true);
                if let Ok(p) = path.build() {
                    window.paint_path(p, *fill);
                }
            }
        },
    )
    .size_full();
    let mut lines = center.lines();
    let big = lines.next().unwrap_or_default().to_owned();
    let small = lines.next().unwrap_or_default().to_owned();
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .child(div().absolute().size_full().child(ring))
        .child(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(big))
                .child(
                    div()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(small),
                ),
        )
}

// ---------------------------------------------------------------------------
// Formatting.

/// `value` on a log scale of 0..=`max` (both ≥ 1).
#[allow(clippy::cast_precision_loss)]
fn log_frac(value: u64, max: u64) -> f32 {
    let top = ((max.max(1) + 1) as f32).ln();
    ((value + 1) as f32).ln() / top
}

fn percent(frac: f32) -> String {
    let p = frac * 100.0;
    if p > 0.0 && p < 1.0 {
        "<1%".to_owned()
    } else {
        format!("{p:.0}%")
    }
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(ch);
    }
    out
}

/// A duration in the largest fitting unit ("45 min", "3.5 h", "12 d").
#[allow(clippy::cast_precision_loss)]
fn duration(secs: u64) -> String {
    let s = secs as f64;
    if s < 3_600.0 {
        format!("{:.0} min", (s / 60.0).max(1.0))
    } else if s < DAY {
        format!("{:.1} h", s / 3_600.0)
    } else if s < 60.0 * DAY {
        format!("{:.1} d", s / DAY)
    } else {
        format!("{:.0} d", s / DAY)
    }
}

/// The date part of [`exact_time`].
fn short_date(t: Timestamp) -> String {
    exact_time(t)
        .split(' ')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn first_name(name: &str) -> String {
    name.split_whitespace().next().unwrap_or(name).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(thousands(1_234_567), "1\u{202f}234\u{202f}567");
        assert_eq!(thousands(12), "12");
        assert_eq!(duration(90), "2 min");
        assert_eq!(duration(5_400), "1.5 h");
        assert_eq!(duration(3 * 86_400), "3.0 d");
        assert_eq!(percent(0.004), "<1%");
        assert_eq!(percent(0.5), "50%");
        assert!((log_frac(100, 100) - 1.0).abs() < 1e-6);
    }
}
