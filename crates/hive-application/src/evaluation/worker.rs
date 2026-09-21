//! Drives only the deterministic PostgreSQL-backed local prompt-case queue. Unlike the
//! deployment outbox worker, whose single `deliver_next` method hides claim, heartbeat and retry,
//! this worker inlines the whole claim -> decide -> commit -> delivered/failed cycle and calls
//! the work store's heartbeat methods itself, so every heartbeat happens in `run_once` rather
//! than in an outer poll loop.

use super::decider;
use super::fixture::EvaluationFixturePort;
use super::models::EvaluationExecutionDecision;
use super::repository::{EvaluationWorkStore, RepositoryError};

pub struct LocalEvaluationWorker<S: EvaluationWorkStore> {
    store: S,
    fixtures: Box<dyn EvaluationFixturePort>,
    worker_id: String,
}

impl<S: EvaluationWorkStore> LocalEvaluationWorker<S> {
    pub fn new(store: S, fixtures: Box<dyn EvaluationFixturePort>, worker_id: String) -> Self {
        Self {
            store,
            fixtures,
            worker_id,
        }
    }

    pub async fn run_once(&self) -> Result<bool, RepositoryError> {
        let claimed = self.store.claim_next(&self.worker_id).await;
        let work = match claimed {
            Ok(Some(work)) => work,
            Ok(None) => {
                self.store.idle(&self.worker_id).await?;
                return Ok(false);
            }
            Err(error) => {
                self.store.claim_failed(&self.worker_id).await?;
                return Err(error);
            }
        };
        let decision = match decider::decide(&work, self.fixtures.as_ref()) {
            Ok(decision) => decision,
            Err(error) => {
                self.store
                    .failed(
                        &self.worker_id,
                        &work,
                        EvaluationExecutionDecision::runner_failure("RUNNER_FAILED"),
                    )
                    .await?;
                return Err(RepositoryError::Other(error.into()));
            }
        };
        match self.store.commit(&self.worker_id, &work, decision).await {
            Ok(()) => {
                self.store.delivered(&self.worker_id).await?;
                Ok(true)
            }
            Err(error) => {
                self.store
                    .failed(
                        &self.worker_id,
                        &work,
                        EvaluationExecutionDecision::runner_failure("RUNNER_FAILED"),
                    )
                    .await?;
                Err(error)
            }
        }
    }

    pub async fn run_batch(&self, maximum: u32) -> Result<u32, RepositoryError> {
        assert!(
            maximum >= 1,
            "A worker batch requires a positive delivery bound."
        );
        let mut delivered = 0;
        while delivered < maximum && self.run_once().await? {
            delivered += 1;
        }
        Ok(delivered)
    }
}
