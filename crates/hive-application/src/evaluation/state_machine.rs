//! Every permitted run and case lifecycle
//! transition, independent of PostgreSQL mechanics.
//!
//! `evaluation_runs.lifecycle_status` and `evaluation_case_runs.lifecycle_status` admit the same
//! value set, so `EvaluationRunStatus` types both. Persistence parses each column once at its row
//! boundary; every transition predicate is an exhaustive `match`, so a new variant fails
//! compilation until each predicate classifies it.

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvaluationRunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unrecognized evaluation run status `{0}`")]
pub struct UnknownEvaluationRunStatus(pub String);

impl EvaluationRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "QUEUED",
            Self::Running => "RUNNING",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Canceled => "CANCELED",
        }
    }

    pub const fn may_start_run(self) -> bool {
        match self {
            Self::Queued => true,
            Self::Running | Self::Completed | Self::Failed | Self::Canceled => false,
        }
    }

    /// The run is `RUNNING`; callers pair this with the case's own status.
    const fn is_running(self) -> bool {
        match self {
            Self::Running => true,
            Self::Queued | Self::Completed | Self::Failed | Self::Canceled => false,
        }
    }

    pub const fn may_finalize(self, every_case_terminal: bool) -> bool {
        self.is_running() && every_case_terminal
    }

    pub const fn may_cancel(self) -> bool {
        match self {
            Self::Queued | Self::Running => true,
            Self::Completed | Self::Failed | Self::Canceled => false,
        }
    }

    pub const fn is_terminal(self) -> bool {
        match self {
            Self::Completed | Self::Failed | Self::Canceled => true,
            Self::Queued | Self::Running => false,
        }
    }
}

pub const fn may_start_case(run: EvaluationRunStatus, current: EvaluationRunStatus) -> bool {
    run.is_running() && current.may_start_run()
}

pub const fn may_complete_case(run: EvaluationRunStatus, current: EvaluationRunStatus) -> bool {
    run.is_running() && current.is_running()
}

impl FromStr for EvaluationRunStatus {
    type Err = UnknownEvaluationRunStatus;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "QUEUED" => Ok(Self::Queued),
            "RUNNING" => Ok(Self::Running),
            "COMPLETED" => Ok(Self::Completed),
            "FAILED" => Ok(Self::Failed),
            "CANCELED" => Ok(Self::Canceled),
            other => Err(UnknownEvaluationRunStatus(other.to_string())),
        }
    }
}

impl std::fmt::Display for EvaluationRunStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{may_complete_case, may_start_case, EvaluationRunStatus as Status};

    const ALL: [Status; 5] = [
        Status::Queued,
        Status::Running,
        Status::Completed,
        Status::Failed,
        Status::Canceled,
    ];

    fn named(predicate: impl Fn(Status) -> bool) -> Vec<&'static str> {
        ALL.into_iter()
            .filter(|status| predicate(*status))
            .map(Status::as_str)
            .collect()
    }

    #[test]
    fn every_variant_round_trips_through_its_column_value() {
        for status in ALL {
            assert_eq!(status.as_str().parse::<Status>(), Ok(status));
        }
        assert!("PAUSED".parse::<Status>().is_err());
    }

    #[test]
    fn predicates_match_the_literal_sets_they_replaced() {
        assert_eq!(named(Status::may_start_run), ["QUEUED"]);
        assert_eq!(named(Status::may_cancel), ["QUEUED", "RUNNING"]);
        assert_eq!(
            named(Status::is_terminal),
            ["COMPLETED", "FAILED", "CANCELED"]
        );
        assert_eq!(named(|status| status.may_finalize(true)), ["RUNNING"]);
        assert!(named(|status| status.may_finalize(false)).is_empty());
    }

    #[test]
    fn case_transitions_require_a_running_run() {
        for run in ALL {
            let running = run == Status::Running;
            assert_eq!(
                named(|case| may_start_case(run, case)),
                if running { vec!["QUEUED"] } else { vec![] }
            );
            assert_eq!(
                named(|case| may_complete_case(run, case)),
                if running { vec!["RUNNING"] } else { vec![] }
            );
        }
    }
}
