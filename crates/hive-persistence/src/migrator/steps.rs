/// One migration or seed file, plus how the ledger gates it. The order in `STEPS` is the order
/// the migrator runs: `V000` through `V014` run on every start with no ledger check; the three
/// seeds and `V015` onward are gated by `hive_schema_migrations`; `V016_1` runs after `V016`; the
/// evaluation schema repair replays `V039`'s content unconditionally between `V039` and `V040`.
pub struct Step {
    pub label: &'static str,
    pub sql: &'static str,
    pub gate: Gate,
}

pub enum Gate {
    /// V000 through V014: applied every startup, never recorded in the ledger.
    Unconditional,
    /// Recorded in `hive_schema_migrations` under this exact literal version string.
    Ledgered(&'static str),
}

macro_rules! migration {
    ($file:literal) => {
        include_str!(concat!("../../../../db/migration/", $file))
    };
}

macro_rules! seed {
    ($file:literal) => {
        include_str!(concat!("../../../../db/seed/", $file))
    };
}

pub fn steps() -> Vec<Step> {
    use Gate::*;
    vec![
        Step {
            label: "V000",
            sql: migration!("V000__us_english_contract_cutover.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V001",
            sql: migration!("V001__organization_directory.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V002",
            sql: migration!("V002__organization_overview_projects.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V003",
            sql: migration!("V003__organization_project_directory.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V004",
            sql: migration!("V004__project_dashboard.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V005",
            sql: migration!("V005__project_agent_directory.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V006",
            sql: migration!("V006__agent_operational_overview.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V007",
            sql: migration!("V007__agent_draft_editor.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V008",
            sql: migration!("V008__console_context_and_preferences.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V009",
            sql: migration!("V009__organization_project_administration.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V010",
            sql: migration!("V010__mutable_display_preferences.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V011",
            sql: migration!("V011__local_catalog_reusable_configuration.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V012",
            sql: migration!("V012__agent_authoring_immutable_versions.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V013",
            sql: migration!("V013__inert_mcp_server_configuration.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V014",
            sql: migration!("V014__deployment_request_and_local_execution.sql"),
            gate: Unconditional,
        },
        Step {
            label: "S001",
            sql: seed!("organization-directory.sql"),
            gate: Ledgered("S001__organization_directory"),
        },
        Step {
            label: "S009",
            sql: seed!("organization-project-administration.sql"),
            gate: Ledgered("S009__organization_project_administration"),
        },
        Step {
            label: "S011",
            sql: seed!("local-catalog-configuration.sql"),
            gate: Ledgered("S011__local_catalog_configuration"),
        },
        Step {
            label: "V015",
            sql: migration!("V015__deployment_contract_alignment.sql"),
            gate: Ledgered("V015__deployment_contract_alignment"),
        },
        Step {
            label: "V016",
            sql: migration!("V016__deployment_predecessor_compatibility.sql"),
            gate: Ledgered("V016__deployment_predecessor_compatibility"),
        },
        Step {
            label: "V016_1",
            sql: migration!("V016_1__deployment_projection_revision_preflight.sql"),
            gate: Ledgered("V016_1__deployment_projection_revision_preflight"),
        },
        Step {
            label: "V017",
            sql: migration!("V017__deployment_approval_requirements.sql"),
            gate: Ledgered("V017__deployment_approval_requirements"),
        },
        Step {
            label: "V018",
            sql: migration!("V018__deployment_approval_upgrade.sql"),
            gate: Ledgered("V018__deployment_approval_upgrade"),
        },
        Step {
            label: "V019",
            sql: migration!("V019__deployment_approval_operational_hardening.sql"),
            gate: Ledgered("V019__deployment_approval_operational_hardening"),
        },
        Step {
            label: "V020",
            sql: migration!("V020__deployment_approval_execution_fences.sql"),
            gate: Ledgered("V020__deployment_approval_execution_fences"),
        },
        Step {
            label: "V021",
            sql: migration!("V021__deployment_approval_review_and_readiness.sql"),
            gate: Ledgered("V021__deployment_approval_review_and_readiness"),
        },
        Step {
            label: "V022",
            sql: migration!("V022__deployment_approval_archive_and_handoff_contract.sql"),
            gate: Ledgered("V022__deployment_approval_archive_and_handoff_contract"),
        },
        Step {
            label: "V023",
            sql: migration!("V023__deployment_approval_project_archive_outbox.sql"),
            gate: Ledgered("V023__deployment_approval_project_archive_outbox"),
        },
        Step {
            label: "V024",
            sql: migration!("V024__deployment_approval_compatibility_completion.sql"),
            gate: Ledgered("V024__deployment_approval_compatibility_completion"),
        },
        Step {
            label: "V025",
            sql: migration!("V025__deployment_approval_audit_and_lifecycle_hardening.sql"),
            gate: Ledgered("V025__deployment_approval_audit_and_lifecycle_hardening"),
        },
        Step {
            label: "V026",
            sql: migration!("V026__deployment_approval_readiness_and_archive_history.sql"),
            gate: Ledgered("V026__deployment_approval_readiness_and_archive_history"),
        },
        Step {
            label: "V027",
            sql: migration!("V027__deployment_approval_archive_revision_boundary.sql"),
            gate: Ledgered("V027__deployment_approval_archive_revision_boundary"),
        },
        Step {
            label: "V028",
            sql: migration!("V028__deployment_approval_archive_reconciliation_index.sql"),
            gate: Ledgered("V028__deployment_approval_archive_reconciliation_index"),
        },
        Step {
            label: "V029",
            sql: migration!("V029__deployment_approval_replay_audit.sql"),
            gate: Ledgered("V029__deployment_approval_replay_audit"),
        },
        Step {
            label: "V030",
            sql: migration!("V030__deployment_approval_replay_receipts.sql"),
            gate: Ledgered("V030__deployment_approval_replay_receipts"),
        },
        Step {
            label: "V031",
            sql: migration!("V031__deployment_approval_bounded_recovery_and_idempotency.sql"),
            gate: Ledgered("V031__deployment_approval_bounded_recovery_and_idempotency"),
        },
        Step {
            label: "V032",
            sql: migration!("V032__deployment_approval_archive_attribution_and_role_grants.sql"),
            gate: Ledgered("V032__deployment_approval_archive_attribution_and_role_grants"),
        },
        Step {
            label: "V033",
            sql: migration!("V033__deployment_approval_participant_and_delivery_integrity.sql"),
            gate: Ledgered("V033__deployment_approval_participant_and_delivery_integrity"),
        },
        Step {
            label: "V034",
            sql: migration!("V034__deployment_approval_policy_and_recovery_repair.sql"),
            gate: Ledgered("V034__deployment_approval_policy_and_recovery_repair"),
        },
        Step {
            label: "V035",
            sql: migration!("V035__deployment_approval_archive_history_lookup.sql"),
            gate: Ledgered("V035__deployment_approval_archive_history_lookup"),
        },
        Step {
            label: "V036",
            sql: migration!("V036__deployment_approval_correlation_backfill_repair.sql"),
            gate: Ledgered("V036__deployment_approval_correlation_backfill_repair"),
        },
        Step {
            label: "V037",
            sql: migration!("V037__deployment_recovery_promotion_rollback.sql"),
            gate: Ledgered("V037__deployment_recovery_promotion_rollback"),
        },
        Step {
            label: "V038",
            sql: migration!("V038__deployment_active_rollback_predecessor_index.sql"),
            gate: Ledgered("V038__deployment_active_rollback_predecessor_index"),
        },
        Step {
            label: "V039",
            sql: migration!("V039__evaluation_definitions_runs_and_evidence.sql"),
            gate: Ledgered("V039__evaluation_definitions_runs_and_evidence"),
        },
        // The evaluation-schema repair: unconditionally replays V039's own content again on every
        // startup, even when V039's ledger row already exists, matching
        // `DatabaseMigrator.repairAppliedEvaluationSchema()`.
        Step {
            label: "V039-repair",
            sql: migration!("V039__evaluation_definitions_runs_and_evidence.sql"),
            gate: Unconditional,
        },
        Step {
            label: "V040",
            sql: migration!("V040__audit_history_projection.sql"),
            gate: Ledgered("V040__audit_history_projection"),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_has_nonempty_sql() {
        for step in steps() {
            assert!(!step.sql.trim().is_empty(), "{} is empty", step.label);
        }
    }

    #[test]
    fn step_count_matches_the_migration_and_seed_file_count() {
        // 15 unconditional (V000-V014) + 3 seeds + 24 ledgered V015-V038 + V016_1 + V039 + V040
        // + 1 unconditional V039 repair = 46.
        assert_eq!(steps().len(), 46);
    }
}
