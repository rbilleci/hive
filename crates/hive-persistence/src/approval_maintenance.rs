//! Shared health state for the approval-reconciliation maintenance loop. Ports the
//! two `AtomicReference<ApprovalMaintenanceHealth>` fields `PostgresDeploymentRepository`
//! owns, including their pre-first-tick default values, so `GET /health` reports the
//! same "not started yet" state a freshly booted Java service reports before its
//! first `@Scheduled` run. `deployment::approval`'s reconciliation passes replace the defaults
//! through `set_maintenance`/`set_upgrade`.

use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalMaintenanceHealth {
    pub healthy: bool,
    pub failure_code: Option<String>,
    pub attempted: i64,
    pub reconciled: i64,
    pub failed: i64,
}

impl ApprovalMaintenanceHealth {
    pub fn not_completed() -> Self {
        Self {
            healthy: false,
            failure_code: Some("MAINTENANCE_NOT_COMPLETED".into()),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        }
    }

    pub fn compatibility_backfill_pending() -> Self {
        Self {
            healthy: false,
            failure_code: Some("COMPATIBILITY_BACKFILL_PENDING".into()),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        }
    }

    pub fn in_progress() -> Self {
        Self {
            healthy: false,
            failure_code: Some("MAINTENANCE_IN_PROGRESS".into()),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        }
    }
}

#[derive(Clone)]
pub struct ApprovalMaintenanceState {
    maintenance: Arc<RwLock<ApprovalMaintenanceHealth>>,
    upgrade: Arc<RwLock<ApprovalMaintenanceHealth>>,
}

impl Default for ApprovalMaintenanceState {
    fn default() -> Self {
        Self {
            maintenance: Arc::new(RwLock::new(ApprovalMaintenanceHealth::not_completed())),
            upgrade: Arc::new(RwLock::new(
                ApprovalMaintenanceHealth::compatibility_backfill_pending(),
            )),
        }
    }
}

impl ApprovalMaintenanceState {
    pub fn maintenance(&self) -> ApprovalMaintenanceHealth {
        self.maintenance.read().expect("lock not poisoned").clone()
    }

    pub fn upgrade(&self) -> ApprovalMaintenanceHealth {
        self.upgrade.read().expect("lock not poisoned").clone()
    }

    pub fn set_maintenance(&self, health: ApprovalMaintenanceHealth) {
        *self.maintenance.write().expect("lock not poisoned") = health;
    }

    pub fn set_upgrade(&self, health: ApprovalMaintenanceHealth) {
        *self.upgrade.write().expect("lock not poisoned") = health;
    }

    /// Ports `PostgresDeploymentRepository.approvalMaintenanceStatus()`: when archive
    /// reconciliation (`upgrade`) is unhealthy, it masks the expiry health entirely — `/health`
    /// reports the upgrade failure under the `status`/`approvalMaintenance*` fields regardless of
    /// what expiry reconciliation itself last reported.
    pub fn status(&self) -> ApprovalMaintenanceHealth {
        let upgrade = self.upgrade();
        if upgrade.healthy {
            self.maintenance()
        } else {
            upgrade
        }
    }
}
