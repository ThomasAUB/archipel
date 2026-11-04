//! Port traits: the small, role-based interfaces between layers.
//!
//! Consumers depend on the narrowest trait they need; adapters implement as
//! many traits as they can. Every trait here has a fake in `testing`.

pub mod git;
pub mod project;
pub mod store;
pub mod watch;

pub use git::*;
pub use project::*;
pub use store::*;
pub use watch::*;
