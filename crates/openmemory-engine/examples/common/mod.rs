//! Shared helpers for the engine benchmark examples (`stress`,
//! `readpath`).

use std::time::Duration;

/// NATO words for synthetic-but-searchable content, matching the
//  vocabulary used by openmemory-bench.
pub const WORDS: [&str; 16] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliett",
    "kilo", "lima", "mike", "november", "oscar", "papa",
];

/// Percentile over a sorted nanosecond vector.
pub fn pctl(sorted: &[u64], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    Duration::from_nanos(sorted[idx])
}
