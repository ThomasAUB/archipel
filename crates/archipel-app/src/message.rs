//! Commit message helpers: ticket prefixes and templates (spec §4), and
//! the Conventional Commits helper of the staging panel (spec §5).
//!
//! Extension points: [`TicketPrefixExtractor`] and [`MessageTemplate`]; the
//! project settings choose the built-ins, a fork can add its own.

use std::collections::HashMap;
use std::fmt;

use archipel_model::project::{ConventionalCommits, MessageTemplateDef, ProjectSettings};
use regex::Regex;

/// Finds the ticket reference to prefix commit messages with.
pub trait TicketPrefixExtractor: Send + Sync {
    /// The ticket for a commit on `branch` (`None` when there is none).
    fn ticket(&self, branch: Option<&str>) -> Option<String>;
}

/// Always the same ticket (project setting `ticket_prefix`).
pub struct FixedTicket(pub String);

impl TicketPrefixExtractor for FixedTicket {
    fn ticket(&self, _branch: Option<&str>) -> Option<String> {
        Some(self.0.clone())
    }
}

/// The first match of a regex in the branch name (`ticket_from_branch`),
/// e.g. `[A-Z]+-\d+` turns `feature/JIRA-123-login` into `JIRA-123`.
pub struct BranchTicket(Regex);

impl BranchTicket {
    /// Compiles the pattern.
    pub fn new(pattern: &str) -> Result<Self, regex::Error> {
        Regex::new(pattern).map(Self)
    }
}

impl TicketPrefixExtractor for BranchTicket {
    fn ticket(&self, branch: Option<&str>) -> Option<String> {
        let branch = branch?;
        let m = self.0.captures(branch)?;
        // Prefer the first capture group when the pattern has one.
        m.get(1).or_else(|| m.get(0)).map(|g| g.as_str().to_owned())
    }
}

/// Builds the extractor configured in the project settings, if any. An
/// invalid regex is logged and ignored.
#[must_use]
pub fn extractor_for(settings: &ProjectSettings) -> Option<Box<dyn TicketPrefixExtractor>> {
    if let Some(pattern) = &settings.ticket_from_branch {
        match BranchTicket::new(pattern) {
            Ok(t) => return Some(Box::new(t)),
            Err(e) => tracing::warn!(error = %e, "invalid ticket_from_branch pattern"),
        }
    }
    settings
        .ticket_prefix
        .clone()
        .map(|p| Box::new(FixedTicket(p)) as Box<dyn TicketPrefixExtractor>)
}

/// Prefixes `message` with `ticket` unless it already starts with it. A
/// Conventional Commits header stays first: the ticket goes after it
/// (`feat(ui): JIRA-1 add chips`).
#[must_use]
pub fn with_ticket(message: &str, ticket: Option<&str>) -> String {
    let Some(t) = ticket.filter(|t| !t.is_empty()) else {
        return message.to_owned();
    };
    let message = message.trim_start();
    if let Some(h) = parse_header(message) {
        if h.description.starts_with(t) {
            return message.to_owned();
        }
        return format!("{}{t} {}", h.prefix(), &message[h.prefix_len..]);
    }
    if message.starts_with(t) {
        message.to_owned()
    } else {
        format!("{t} {message}")
    }
}

/// A commit message template.
pub trait MessageTemplate: Send + Sync {
    /// Template name shown in the picker.
    fn name(&self) -> &str;
    /// The message for a commit on `branch` with `ticket`.
    fn render(&self, branch: Option<&str>, ticket: Option<&str>) -> String;
}

/// A template from the project file: `{ticket}` and `{branch}` placeholders.
pub struct TextTemplate(pub MessageTemplateDef);

impl MessageTemplate for TextTemplate {
    fn name(&self) -> &str {
        &self.0.name
    }
    fn render(&self, branch: Option<&str>, ticket: Option<&str>) -> String {
        self.0
            .body
            .replace("{ticket}", ticket.unwrap_or_default())
            .replace("{branch}", branch.unwrap_or_default())
    }
}

/// The templates of a project.
#[must_use]
pub fn templates_for(settings: &ProjectSettings) -> Vec<Box<dyn MessageTemplate>> {
    settings
        .message_templates
        .iter()
        .cloned()
        .map(|t| Box::new(TextTemplate(t)) as Box<dyn MessageTemplate>)
        .collect()
}

/// The Conventional Commits types offered when the project names none.
pub const DEFAULT_TYPES: &[&str] = &[
    "feat", "fix", "refactor", "docs", "test", "chore", "perf", "build", "ci", "style", "revert",
];

/// Longest subject line the lint accepts, in characters.
pub const MAX_SUBJECT: usize = 72;

/// The Conventional Commits types of a project (`conventional_types`, else
/// [`DEFAULT_TYPES`]).
#[must_use]
pub fn conventional_types(settings: &ProjectSettings) -> Vec<String> {
    if settings.conventional_types.is_empty() {
        DEFAULT_TYPES.iter().map(|&t| t.to_owned()).collect()
    } else {
        settings.conventional_types.clone()
    }
}

/// Whether the Conventional Commits helper shows, for a repo whose recent
/// commits follow the convention (`detected`) or not.
#[must_use]
pub fn conventional_enabled(mode: ConventionalCommits, detected: bool) -> bool {
    match mode {
        ConventionalCommits::Auto => detected,
        ConventionalCommits::On => true,
        ConventionalCommits::Off => false,
    }
}

/// The `type(scope)!: description` header of a commit message's subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header<'a> {
    /// The type (`feat`).
    pub kind: &'a str,
    /// The scope, without its parentheses.
    pub scope: Option<&'a str>,
    /// The `!` marking a breaking change.
    pub breaking: bool,
    /// Whether a space follows the colon.
    pub spaced: bool,
    /// The rest of the subject line.
    pub description: &'a str,
    /// Byte length of `type(scope)!: `, the spaces after the colon included.
    pub prefix_len: usize,
}

impl Header<'_> {
    /// The header without its description (`feat(ui)!: `).
    #[must_use]
    pub fn prefix(&self) -> String {
        format_prefix(self.kind, self.scope, self.breaking)
    }
}

fn format_prefix(kind: &str, scope: Option<&str>, breaking: bool) -> String {
    let scope = scope.map(|s| format!("({s})")).unwrap_or_default();
    let bang = if breaking { "!" } else { "" };
    format!("{kind}{scope}{bang}: ")
}

/// Parses the Conventional Commits header of `message`'s subject line, as
/// typed so far: `feat:` already counts, the description may be empty.
#[must_use]
pub fn parse_header(message: &str) -> Option<Header<'_>> {
    let subject = message.lines().next().unwrap_or_default();
    let kind_len = subject
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(subject.len());
    let kind = &subject[..kind_len];
    if !kind.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut rest = &subject[kind_len..];
    let mut scope = None;
    if let Some(inner) = rest.strip_prefix('(') {
        let end = inner.find([')', '('])?;
        if !inner[end..].starts_with(')') {
            return None;
        }
        scope = Some(&inner[..end]);
        rest = &inner[end + 1..];
    }
    let breaking = rest.starts_with('!');
    if breaking {
        rest = &rest[1..];
    }
    let rest = rest.strip_prefix(':')?;
    let description = rest.trim_start_matches(' ');
    Some(Header {
        kind,
        scope,
        breaking,
        spaced: description.len() < rest.len(),
        description: description.trim_end(),
        prefix_len: subject.len() - description.len(),
    })
}

/// `message` with `header` replaced by `type(scope)!: ` built from these.
fn rebuild(
    message: &str,
    header: &Header,
    kind: &str,
    scope: Option<&str>,
    breaking: bool,
) -> String {
    format!(
        "{}{}",
        format_prefix(kind, scope, breaking),
        &message[header.prefix_len..]
    )
}

/// Sets the type of `message`'s header to `kind`, adding a `kind: ` header
/// when it has none. The scope, `!` and the rest of the message are kept.
#[must_use]
pub fn set_type(message: &str, kind: &str) -> String {
    match parse_header(message) {
        Some(h) => rebuild(message, &h, kind, h.scope, h.breaking),
        None => format!("{kind}: {}", message.trim_start()),
    }
}

/// Removes `message`'s header (type, scope and `!`), keeping the rest.
#[must_use]
pub fn clear_type(message: &str) -> String {
    match parse_header(message) {
        Some(h) => message[h.prefix_len..].to_owned(),
        None => message.to_owned(),
    }
}

/// Sets (or, with `None`, removes) the scope of `message`'s header; a
/// message without header is returned as is.
#[must_use]
pub fn set_scope(message: &str, scope: Option<&str>) -> String {
    match parse_header(message) {
        Some(h) => rebuild(
            message,
            &h,
            h.kind,
            scope.filter(|s| !s.is_empty()),
            h.breaking,
        ),
        None => message.to_owned(),
    }
}

/// Adds or removes the `!` of a breaking change in `message`'s header; a
/// message without header is returned as is.
#[must_use]
pub fn toggle_breaking(message: &str) -> String {
    match parse_header(message) {
        Some(h) => rebuild(message, &h, h.kind, h.scope, !h.breaking),
        None => message.to_owned(),
    }
}

/// A Conventional Commits rule a message breaks. A warning only: it never
/// prevents the commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lint {
    /// The subject is not `type(scope)!: description`.
    NoHeader,
    /// The type is not one of the project's.
    UnknownType(String),
    /// Nothing after `type: `.
    NoDescription,
    /// The subject is longer than [`MAX_SUBJECT`] characters (its length).
    LongSubject(usize),
    /// The body starts right after the subject.
    NoBlankLine,
    /// The description starts with a capital letter.
    Capitalized,
    /// The description ends with a period.
    TrailingPeriod,
}

impl fmt::Display for Lint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHeader => f.write_str("subject should be `type(scope): description`"),
            Self::UnknownType(t) => write!(f, "unknown type `{t}`"),
            Self::NoDescription => f.write_str("add a description after the type"),
            Self::LongSubject(n) => write!(f, "subject is {n} characters (max {MAX_SUBJECT})"),
            Self::NoBlankLine => f.write_str("leave a blank line after the subject"),
            Self::Capitalized => f.write_str("start the description in lowercase"),
            Self::TrailingPeriod => f.write_str("no period at the end of the subject"),
        }
    }
}

/// The Conventional Commits rules `message` breaks, given the project's
/// `types`. An empty message breaks none. A description starting with an
/// all-caps word (`JIRA-1`, `API`) is not counted as capitalized.
#[must_use]
pub fn lint(message: &str, types: &[String]) -> Vec<Lint> {
    let mut out = Vec::new();
    if message.trim().is_empty() {
        return out;
    }
    let mut lines = message.lines();
    let subject = lines.next().unwrap_or_default().trim_end();
    match parse_header(message) {
        Some(h) if h.spaced || h.description.is_empty() => {
            if !types.iter().any(|t| t == h.kind) {
                out.push(Lint::UnknownType(h.kind.to_owned()));
            }
            let d = h.description;
            if d.is_empty() {
                out.push(Lint::NoDescription);
            } else {
                let first = d.split_whitespace().next().unwrap_or_default();
                if d.starts_with(char::is_uppercase) && first.chars().any(char::is_lowercase) {
                    out.push(Lint::Capitalized);
                }
                if d.ends_with('.') && !d.ends_with("..") {
                    out.push(Lint::TrailingPeriod);
                }
            }
        }
        _ => out.push(Lint::NoHeader),
    }
    let len = subject.chars().count();
    if len > MAX_SUBJECT {
        out.push(Lint::LongSubject(len));
    }
    if lines.next().is_some_and(|l| !l.trim().is_empty()) {
        out.push(Lint::NoBlankLine);
    }
    out
}

/// Subjects git writes itself, left out when judging the convention.
fn is_generated(subject: &str) -> bool {
    ["Merge ", "Revert \"", "fixup! ", "squash! ", "amend! "]
        .iter()
        .any(|p| subject.starts_with(p))
}

/// Whether most of these recent commit `subjects` (newest first) follow
/// the convention with one of `types`. Merges and other subjects git
/// writes are left out; fewer than 3 commits left are not enough to tell.
#[must_use]
pub fn follows_convention<S: AsRef<str>>(subjects: &[S], types: &[String]) -> bool {
    let (mut total, mut conventional) = (0usize, 0usize);
    for s in subjects
        .iter()
        .map(AsRef::as_ref)
        .filter(|s| !is_generated(s))
    {
        total += 1;
        if parse_header(s).is_some_and(|h| h.spaced && types.iter().any(|t| t == h.kind)) {
            conventional += 1;
        }
    }
    total >= 3 && conventional * 2 > total
}

/// The scopes used in these commit `subjects` (newest first), the most
/// used first, then the most recent.
#[must_use]
pub fn history_scopes<S: AsRef<str>>(subjects: &[S]) -> Vec<String> {
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for (i, s) in subjects.iter().enumerate() {
        if let Some(scope) = parse_header(s.as_ref()).and_then(|h| h.scope) {
            let scope = scope.trim();
            if !scope.is_empty() {
                counts.entry(scope).or_insert((0, i)).0 += 1;
            }
        }
    }
    let mut scopes: Vec<_> = counts.into_iter().collect();
    scopes.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)));
    scopes.into_iter().map(|(s, _)| s.to_owned()).collect()
}

/// Folders that group modules rather than name one.
const CONTAINERS: &[&str] = &[
    "crates",
    "packages",
    "libs",
    "lib",
    "src",
    "modules",
    "pkg",
    "internal",
    "cmd",
    "apps",
    "components",
    "source",
    "sources",
];

/// The scope a changed file suggests: a scope of `known` (used in the
/// history) naming one of its folders or its file name, else its first
/// folder that is not a mere container (`crates/`, `src/`, …), without the
/// repo name prefix (`crates/archipel-ui/src/x.rs` in `archipel`: `ui`). A
/// file of `.github/` suggests `ci`; a file at the root, nothing.
fn path_scope(path: &str, repo_name: &str, known: &[String]) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let (file, dirs) = parts.split_last()?;
    let stem = file.split('.').next().unwrap_or(file);
    if let Some(k) = dirs
        .iter()
        .chain(std::iter::once(&stem))
        .find_map(|p| known.iter().find(|k| k.eq_ignore_ascii_case(p)))
    {
        return Some(k.clone());
    }
    if matches!(dirs.first(), Some(&(".github" | ".gitlab" | ".circleci"))) {
        return Some("ci".to_owned());
    }
    let dir = dirs
        .iter()
        .find(|d| !d.starts_with('.') && !CONTAINERS.contains(&d.to_ascii_lowercase().as_str()))?;
    let lower = dir.to_ascii_lowercase();
    let repo = repo_name.to_ascii_lowercase();
    let scope = lower
        .strip_prefix(&repo)
        .and_then(|r| r.strip_prefix(['-', '_', '.']))
        .filter(|r| !r.is_empty())
        .unwrap_or(&lower);
    Some(scope.to_owned())
}

/// Up to `limit` scopes to offer for a commit of the `staged` paths in
/// the repo `repo_name`: those of the staged files (most files first),
/// then those of the history (`known`, as [`history_scopes`] lists them).
#[must_use]
pub fn suggest_scopes(
    staged: &[String],
    repo_name: &str,
    known: &[String],
    limit: usize,
) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for p in staged {
        if let Some(s) = path_scope(p, repo_name, known) {
            match counts.iter_mut().find(|(c, _)| *c == s) {
                Some((_, n)) => *n += 1,
                None => counts.push((s, 1)),
            }
        }
    }
    counts.sort_by_key(|a| std::cmp::Reverse(a.1)); // stable: first seen first on ties
    let mut out: Vec<String> = counts.into_iter().map(|(s, _)| s).collect();
    for k in known {
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_from_branch_and_fixed() {
        let t = BranchTicket::new(r"[A-Z]+-\d+").unwrap();
        assert_eq!(
            t.ticket(Some("feature/JIRA-123-login")).as_deref(),
            Some("JIRA-123")
        );
        assert_eq!(t.ticket(Some("main")), None);
        let g = BranchTicket::new(r"^\w+/(\d+)").unwrap();
        assert_eq!(g.ticket(Some("fix/42-crash")).as_deref(), Some("42"));
        assert_eq!(
            FixedTicket("OPS-1".into()).ticket(None).as_deref(),
            Some("OPS-1")
        );
    }

    #[test]
    fn settings_pick_extractor() {
        let mut s = ProjectSettings {
            ticket_prefix: Some("X-1".into()),
            ..Default::default()
        };
        assert_eq!(
            extractor_for(&s).unwrap().ticket(Some("a")).as_deref(),
            Some("X-1")
        );
        s.ticket_from_branch = Some(r"[A-Z]+-\d+".into());
        assert_eq!(
            extractor_for(&s).unwrap().ticket(Some("AB-7")).as_deref(),
            Some("AB-7")
        );
        s.ticket_from_branch = Some("(".into());
        assert_eq!(
            extractor_for(&s).unwrap().ticket(Some("AB-7")).as_deref(),
            Some("X-1")
        );
    }

    #[test]
    fn prefixing_is_idempotent() {
        assert_eq!(with_ticket("fix crash", Some("JIRA-1")), "JIRA-1 fix crash");
        assert_eq!(
            with_ticket("JIRA-1 fix crash", Some("JIRA-1")),
            "JIRA-1 fix crash"
        );
        assert_eq!(with_ticket("fix", None), "fix");
    }

    #[test]
    fn the_ticket_goes_after_a_conventional_header() {
        assert_eq!(
            with_ticket("feat(ui): add chips", Some("JIRA-1")),
            "feat(ui): JIRA-1 add chips"
        );
        assert_eq!(
            with_ticket("feat(ui): JIRA-1 add chips", Some("JIRA-1")),
            "feat(ui): JIRA-1 add chips"
        );
        assert_eq!(with_ticket("fix!:crash", Some("T-2")), "fix!: T-2 crash");
    }

    fn types() -> Vec<String> {
        DEFAULT_TYPES.iter().map(|&t| t.to_owned()).collect()
    }

    #[test]
    fn headers_parse() {
        let h = parse_header("feat(ui)!: add chips\n\nbody").unwrap();
        assert_eq!(
            (h.kind, h.scope, h.breaking, h.spaced, h.description),
            ("feat", Some("ui"), true, true, "add chips")
        );
        assert_eq!(h.prefix_len, "feat(ui)!: ".len());
        assert_eq!(parse_header("fix:").unwrap().description, "");
        assert!(!parse_header("fix:x").unwrap().spaced);
        assert_eq!(parse_header("Merge branch 'a': x"), None);
        assert_eq!(parse_header("add chips"), None);
        assert_eq!(parse_header("feat(ui: x"), None);
        assert_eq!(parse_header("(ui): x"), None);
        assert_eq!(parse_header(""), None);
    }

    #[test]
    fn chips_set_the_type_and_keep_the_rest() {
        assert_eq!(set_type("add chips", "feat"), "feat: add chips");
        assert_eq!(set_type("", "feat"), "feat: ");
        assert_eq!(
            set_type("fix(ui)!: add chips\n\nbody", "feat"),
            "feat(ui)!: add chips\n\nbody"
        );
        assert_eq!(
            set_type("JIRA-1 add chips", "feat"),
            "feat: JIRA-1 add chips"
        );
        assert_eq!(clear_type("feat(ui): add chips\nx"), "add chips\nx");
        assert_eq!(clear_type("add chips"), "add chips");
    }

    #[test]
    fn scope_and_breaking_edit_the_header() {
        assert_eq!(set_scope("feat: x", Some("ui")), "feat(ui): x");
        assert_eq!(set_scope("feat(app)!: x", Some("ui")), "feat(ui)!: x");
        assert_eq!(set_scope("feat(ui): x", None), "feat: x");
        assert_eq!(set_scope("add x", Some("ui")), "add x");
        assert_eq!(toggle_breaking("feat(ui): x"), "feat(ui)!: x");
        assert_eq!(toggle_breaking("feat!: x"), "feat: x");
        assert_eq!(toggle_breaking("add x"), "add x");
    }

    #[test]
    fn lint_warns() {
        let t = types();
        assert!(lint("", &t).is_empty());
        assert!(lint("feat(ui): add chips\n\nbody", &t).is_empty());
        assert_eq!(lint("add chips", &t), vec![Lint::NoHeader]);
        assert_eq!(lint("feat:add", &t), vec![Lint::NoHeader]);
        assert_eq!(lint("wip: x", &t), vec![Lint::UnknownType("wip".into())]);
        assert_eq!(lint("feat: ", &t), vec![Lint::NoDescription]);
        assert_eq!(
            lint("feat: Add chips.\nbody", &t),
            vec![Lint::Capitalized, Lint::TrailingPeriod, Lint::NoBlankLine]
        );
        assert!(lint("feat: JIRA-1 add chips", &t).is_empty());
        assert!(lint("feat: API keys...", &t).is_empty());
        let long = format!("feat: {}", "a".repeat(70));
        assert_eq!(lint(&long, &t), vec![Lint::LongSubject(76)]);
        let custom = vec!["deps".to_owned()];
        assert_eq!(lint("deps: bump", &custom), vec![]);
    }

    #[test]
    fn the_convention_is_detected_from_most_commits() {
        let t = types();
        let yes = [
            "feat: a",
            "Merge branch 'x'",
            "fix(ui): b",
            "add c",
            "docs: d",
        ];
        assert!(follows_convention(&yes, &t));
        let no = ["feat: a", "add b", "Fix c", "docs: d"];
        assert!(!follows_convention(&no, &t));
        assert!(!follows_convention(&["feat: a", "fix: b"], &t));
    }

    #[test]
    fn scopes_come_from_the_history_and_the_staged_paths() {
        let known = history_scopes(&["feat(ui): a", "fix(app): b", "fix(ui): c", "docs: d"]);
        assert_eq!(known, vec!["ui", "app"]);
        let staged = vec![
            "crates/archipel-graph/src/a.rs".to_owned(),
            "crates/archipel-graph/src/b.rs".to_owned(),
            "crates/archipel-ui/src/staging.rs".to_owned(),
            ".github/workflows/ci.yml".to_owned(),
            "README.md".to_owned(),
            "docs/SPEC.md".to_owned(),
        ];
        assert_eq!(
            suggest_scopes(&staged, "archipel", &known, 6),
            vec!["graph", "ui", "ci", "docs", "app"]
        );
        assert_eq!(
            suggest_scopes(&staged, "archipel", &known, 2),
            vec!["graph", "ui"]
        );
    }

    #[test]
    fn settings_pick_the_types() {
        let mut s = ProjectSettings::default();
        assert_eq!(conventional_types(&s).len(), DEFAULT_TYPES.len());
        s.conventional_types = vec!["deps".into()];
        assert_eq!(conventional_types(&s), vec!["deps"]);
        assert!(conventional_enabled(ConventionalCommits::Auto, true));
        assert!(!conventional_enabled(ConventionalCommits::Auto, false));
        assert!(conventional_enabled(ConventionalCommits::On, false));
        assert!(!conventional_enabled(ConventionalCommits::Off, true));
    }

    #[test]
    fn templates_render_placeholders() {
        let t = TextTemplate(MessageTemplateDef {
            name: "feat".into(),
            body: "{ticket}: feat on {branch}".into(),
        });
        assert_eq!(t.render(Some("dev"), Some("T-1")), "T-1: feat on dev");
    }
}
