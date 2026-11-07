//! # archipel-app
//!
//! The UI-agnostic application layer: everything the frontend needs, as
//! view-models and commands, with no UI toolkit dependency.
//!
//! * [`App`]: project sessions (paint from cached summaries, background
//!   refresh, file-watcher invalidations, unloading), dashboard state, the
//!   command bus ([`Command`]) and events ([`AppEvent`]).
//! * Extension points (ARCHITECTURE §5), all used by the built-ins:
//!   [`dashboard::DashboardColumn`] / [`dashboard::RepoFilter`],
//!   [`actions::Action`], [`forge::ForgeProvider`],
//!   [`message::TicketPrefixExtractor`] / [`message::MessageTemplate`],
//!   and the [`keymap::Keymap`].
//! * [`forge`]: forge integration (spec §8): the "Pull request" dashboard
//!   column, sign-in, pull requests across repos and clone from an account,
//!   through the forge ports of [`forge::ForgePorts`].
//! * [`deptree`]: the repo dependency tree view-model (spec §9), with the
//!   [`deptree::DependencySource`] extension point.
//! * [`groups`]: sidebar grouping of repos by category (spec §1).
//! * [`settings`]: loading the settings file.
//!
//! Dependencies: `archipel-model` and the feature crates (`ops`, `project`,
//! `graph`, `stats`, `theme`, `cache` LRUs). Concrete adapters are injected
//! by the composition roots through [`Services`].

// Trait impls returning string literals keep the trait's `&str` signature.
#![allow(clippy::unnecessary_literal_bound)]

pub mod actions;
mod app;
pub mod dashboard;
pub mod deptree;
pub mod forge;
pub mod fuzzy;
pub mod groups;
pub mod keymap;
pub mod message;
pub mod settings;

pub use actions::{Action, ActionContext, ActionRegistry, Command, CustomCommand};
pub use app::{
    App, AppError, AppEvent, CloneOutcome, CloneProtocol, DirectoryTarget, Extensions, ProjectIcon,
    ProjectIdentity, PullRequestOutcome, PullRequestPlan, PullRequestTarget, RepoPullRequests,
    Services, clone_dir_name, file_project_id, initials, split_clone_urls,
};
pub use archipel_stats::{BranchRow, BranchesView, DEFAULT_STALE_BRANCH_DAYS};
