//! # archipel-ops
//!
//! Everything that *changes* repositories, and how it is run:
//!
//! * [`scheduler`]: per-repo serial write queue, bounded read and network
//!   pools, cancellation and progress events ([`Scheduler`], [`OpHandle`]).
//! * [`journal`]: records the "before" state of every mutating action
//!   ([`Journal`]) so it can be undone.
//! * [`undo`]: undo/redo planning and execution, refused with an explanation
//!   when it would lose work; compensating actions for pushes ([`UndoService`]).
//! * [`actions`]: single-repo actions wired to the journal ([`RepoActions`]).
//! * [`batch`]: batch operations over many repos with per-repo reports
//!   ([`BatchOps`], [`BatchReport`]) — never a silent all-or-nothing.
//! * [`commit`]: the multi-repo commit use case ([`MultiCommit`]).
//! * [`patch`]: builds patches for hunk / line staging.
//!
//! Dependencies: `archipel-model` traits only (plus `tokio` for scheduling).
//! Concrete Git backends are injected by the composition root.

pub mod actions;
pub mod batch;
pub mod commit;
pub mod error;
pub mod journal;
pub mod patch;
pub mod scheduler;
pub mod undo;

pub use actions::{GitPorts, RepoActions};
pub use batch::{BatchOps, BatchReport, Outcome};
pub use commit::{CommitOptions, CommitPlan, CommitTarget, MultiCommit};
pub use error::OpError;
pub use journal::{Journal, Recording};
pub use scheduler::{OpClass, OpContext, OpEvent, OpEventKind, OpHandle, OpId, OpSpec, Scheduler};
pub use undo::{Compensation, Direction, UndoPlan, UndoService};
