//! Which run of an async load is the latest.
//!
//! A load can be started again before the previous run finishes (a file
//! watcher event right after an operation). Its runs finish in any order,
//! so each one takes a ticket when it starts and only applies its result
//! while that ticket is still the latest: an older run never replaces the
//! result of a newer one.

/// The runs of one load (see the [module docs](self)).
#[derive(Debug, Default)]
pub struct Latest(u64);

/// A run of a load, from [`Latest::begin`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket(u64);

impl Latest {
    /// Starts a run: every earlier run is now out of date.
    pub fn begin(&mut self) -> Ticket {
        self.0 += 1;
        Ticket(self.0)
    }

    /// Whether `ticket` is the latest run, whose result may be applied.
    pub fn is_current(&self, ticket: Ticket) -> bool {
        self.0 == ticket.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_run_is_current() {
        let mut runs = Latest::default();
        let first = runs.begin();
        assert!(runs.is_current(first));
        let second = runs.begin();
        assert!(!runs.is_current(first));
        assert!(runs.is_current(second));
    }
}
