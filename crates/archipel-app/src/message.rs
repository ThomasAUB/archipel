//! Commit message helpers (spec §4): ticket prefixes and templates.
//!
//! Extension points: [`TicketPrefixExtractor`] and [`MessageTemplate`]; the
//! project settings choose the built-ins, a fork can add its own.

use archipel_model::project::{MessageTemplateDef, ProjectSettings};
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

/// Prefixes `message` with `ticket` unless it already starts with it.
#[must_use]
pub fn with_ticket(message: &str, ticket: Option<&str>) -> String {
    match ticket {
        Some(t) if !t.is_empty() && !message.trim_start().starts_with(t) => {
            format!("{t} {}", message.trim_start())
        }
        _ => message.to_owned(),
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
    fn templates_render_placeholders() {
        let t = TextTemplate(MessageTemplateDef {
            name: "feat".into(),
            body: "{ticket}: feat on {branch}".into(),
        });
        assert_eq!(t.render(Some("dev"), Some("T-1")), "T-1: feat on dev");
    }
}
