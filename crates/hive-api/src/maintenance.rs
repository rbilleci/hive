//! The `serve`-subcommand's scheduled approval-reconciliation task.

use hive_persistence::{deployment, ApprovalMaintenanceState};
use sea_orm::DatabaseConnection;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

/// Ticks every second, calling expiry reconciliation then archive reconciliation in sequence, and
/// publishing each result into the shared `/health` state. The loop body is awaited to completion
/// before the next `tick()` call, so a pass that overruns 1 second cannot overlap the next;
/// `MissedTickBehavior::Skip` then realigns to the following second boundary rather than bursting
/// through the missed ticks.
///
/// `shutdown` ends the loop at a tick boundary: a reconciliation pass that has already begun runs
/// to completion, so no transaction is abandoned part-way. `biased` makes the shutdown arm win
/// when the signal and a tick are ready together, rather than `select!`'s random choice.
pub(crate) async fn run(
    db: DatabaseConnection,
    state: ApprovalMaintenanceState,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            _ = interval.tick() => {}
        }
        deployment::reconcile_approval_expiry(&db, &state).await;
        deployment::reconcile_approval_upgrade(&db, &state).await;
    }
}
