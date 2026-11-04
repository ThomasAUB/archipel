//! Startup milestones (spec: start < 1 s).
//!
//! Every milestone is logged; with `ARCHIPEL_STARTUP_TRACE=1` they are also
//! printed on stderr, which is what the startup benchmark reads.

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// Records the process start (call first thing in `main`).
pub fn begin() {
    let _ = START.set(Instant::now());
    if std::env::var_os("ARCHIPEL_STARTUP_TRACE").is_some() {
        // Wall clock, so a launcher can compare with the OS process start time.
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        eprintln!("startup main-wall-ms {wall}");
    }
}

/// Milliseconds since [`begin`].
pub fn elapsed_ms() -> f64 {
    START.get().map_or(0.0, |s| s.elapsed().as_secs_f64() * 1e3)
}

/// Records a milestone.
pub fn mark(name: &str) {
    let ms = elapsed_ms();
    tracing::info!(milestone = name, ms, "startup");
    if std::env::var_os("ARCHIPEL_STARTUP_TRACE").is_some() {
        eprintln!("startup {name} {ms:.1}");
    }
}
