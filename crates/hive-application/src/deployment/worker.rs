//! The deterministic worker loop for local outbox delivery. No I/O of its own — every delivery goes
//! through `DeploymentOutboxDelivery`.

use super::repository::{DeploymentOutboxDelivery, RepositoryError};

pub struct LocalDeploymentOutboxWorker<D: DeploymentOutboxDelivery> {
    delivery: D,
    worker_id: String,
}

impl<D: DeploymentOutboxDelivery> LocalDeploymentOutboxWorker<D> {
    pub fn new(delivery: D, worker_id: String) -> Self {
        Self {
            delivery,
            worker_id,
        }
    }

    pub async fn run_once(&self) -> Result<bool, RepositoryError> {
        self.delivery.deliver_next(&self.worker_id).await
    }

    pub async fn run_batch(&self, maximum_deliveries: u32) -> Result<u32, RepositoryError> {
        assert!(
            maximum_deliveries >= 1,
            "A worker batch requires a positive delivery bound."
        );
        let mut count = 0;
        while count < maximum_deliveries && self.run_once().await? {
            count += 1;
        }
        Ok(count)
    }
}
