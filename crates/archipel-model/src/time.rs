//! A serializable wall-clock timestamp.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Seconds since the Unix epoch (UTC). Serializable, unlike `Instant`.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// The current time.
    #[must_use]
    pub fn now() -> Self {
        Self::from_system(SystemTime::now())
    }

    /// Converts from a `SystemTime` (times before the epoch become negative).
    #[must_use]
    pub fn from_system(time: SystemTime) -> Self {
        match time.duration_since(UNIX_EPOCH) {
            Ok(d) => Self(i64::try_from(d.as_secs()).unwrap_or(i64::MAX)),
            Err(e) => Self(-i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX)),
        }
    }

    /// Time elapsed from `self` to `later` (zero if `later` is earlier).
    #[must_use]
    pub fn elapsed_until(self, later: Self) -> Duration {
        Duration::from_secs(u64::try_from(later.0 - self.0).unwrap_or(0))
    }

    /// Index of the Monday-based week since the epoch.
    ///
    /// The epoch (1970-01-01) was a Thursday, so weeks are shifted by 3 days.
    #[must_use]
    pub fn week_index(self) -> i64 {
        (self.0 + 3 * 86_400).div_euclid(7 * 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn week_index_starts_on_monday() {
        // 1970-01-05 was a Monday; 1970-01-04 a Sunday.
        let sunday = Timestamp(3 * 86_400);
        let monday = Timestamp(4 * 86_400);
        assert_eq!(monday.week_index(), sunday.week_index() + 1);
    }

    #[test]
    fn elapsed_is_saturating() {
        assert_eq!(Timestamp(10).elapsed_until(Timestamp(5)), Duration::ZERO);
        assert_eq!(Timestamp(5).elapsed_until(Timestamp(10)).as_secs(), 5);
    }
}
