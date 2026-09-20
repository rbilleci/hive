use std::sync::atomic::{AtomicU64, Ordering};

/// Ports `GraphqlExecutor.Telemetry`: in-process counters `GET /health` reports.
/// Starts at zero, matching a freshly booted Java service before its first
/// `/graphql` request.
#[derive(Default)]
pub struct GraphqlTelemetry {
    completed: AtomicU64,
    failed: AtomicU64,
    unavailable: AtomicU64,
    last_duration_nanos: AtomicU64,
    max_duration_nanos: AtomicU64,
}

pub enum Outcome {
    Completed,
    Failed,
    Unavailable,
}

impl GraphqlTelemetry {
    pub fn record(&self, outcome: Outcome, duration_nanos: u64) {
        match outcome {
            Outcome::Completed => self.completed.fetch_add(1, Ordering::Relaxed),
            Outcome::Failed => self.failed.fetch_add(1, Ordering::Relaxed),
            Outcome::Unavailable => self.unavailable.fetch_add(1, Ordering::Relaxed),
        };
        self.last_duration_nanos
            .store(duration_nanos, Ordering::Relaxed);
        self.max_duration_nanos
            .fetch_max(duration_nanos, Ordering::Relaxed);
    }

    pub fn completed(&self) -> u64 {
        self.completed.load(Ordering::Relaxed)
    }

    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn unavailable(&self) -> u64 {
        self.unavailable.load(Ordering::Relaxed)
    }

    pub fn last_duration_nanos(&self) -> u64 {
        self.last_duration_nanos.load(Ordering::Relaxed)
    }

    pub fn max_duration_nanos(&self) -> u64 {
        self.max_duration_nanos.load(Ordering::Relaxed)
    }
}
