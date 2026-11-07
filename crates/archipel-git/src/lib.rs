//! # archipel-git
//!
//! Git adapters implementing the port traits of `archipel-model`:
//!
//! * [`GitCli`] runs the `git` executable. It implements **every** reader and
//!   writer trait; writes always go through it so hooks, credentials, signing
//!   and LFS behave exactly as in the user's terminal.
//! * [`GixReader`] reads in-process with `gix` (status, refs, history, stash).
//!   It is the default read backend (see `docs/adr/0002-read-backend.md`).
//! * [`parse`] holds the pure parsers of CLI outputs (porcelain v2, unified
//!   diffs, numstat, progress), unit-tested on recorded outputs.
//!
//! Dependencies: `archipel-model` only (plus `gix`). No `gix` type appears in
//! a public signature: everything is converted to model types at the boundary.

mod cli;
pub mod gitdir;
mod gix_backend;
pub(crate) mod parse;
mod version;

pub use cli::{CliConfig, GitCli};
pub use gix_backend::GixReader;
pub use version::{GitVersion, check_git_version};
