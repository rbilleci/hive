//! The evaluation domain: the definition lifecycle, `runEvaluation` driven through the outbox
//! worker in process, the generated run reads with their fact lists, and the grant those reads
//! require.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test evaluation -- --ignored

mod common;

use common::*;

async fn delete_evaluation_test_fixtures(
    db: &sea_orm::DatabaseConnection,
    definition_id: &str,
    agent_id: &str,
) {
    use hive_persistence::entity::{
        agent_versions, enums, evaluation_artifact_metadata, evaluation_audit_events,
        evaluation_case_runs, evaluation_command_receipts, evaluation_definition_drafts,
        evaluation_definition_versions, evaluation_definitions, evaluation_metric_results,
        evaluation_outbox_events, evaluation_results, evaluation_runs,
        evaluation_target_projections, evaluation_target_snapshots,
    };
    use sea_orm::sea_query::JoinType;
    use sea_orm::{
        ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect, QueryTrait, RelationTrait,
    };

    let definition = uuid(definition_id);
    // The versions of this definition, the subquery every assertion below narrows by.
    let versions_of_definition = || {
        evaluation_definition_versions::Entity::find()
            .select_only()
            .column(evaluation_definition_versions::Column::Id)
            .filter(evaluation_definition_versions::Column::DefinitionId.eq(definition))
            .into_query()
    };
    let run_ids: Vec<uuid::Uuid> = evaluation_runs::Entity::find()
        .select_only()
        .column(evaluation_runs::Column::Id)
        .join(
            JoinType::InnerJoin,
            evaluation_runs::Relation::EvaluationDefinitionVersions.def(),
        )
        .filter(evaluation_definition_versions::Column::DefinitionId.eq(definition))
        .into_tuple()
        .all(db)
        .await
        .expect("look up evaluation test runs");
    let expect = "clean up an evaluation test run table";
    for run_id in &run_ids {
        let run_id = *run_id;
        evaluation_case_runs::Entity::delete_many()
            .filter(evaluation_case_runs::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_metric_results::Entity::delete_many()
            .filter(evaluation_metric_results::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_artifact_metadata::Entity::delete_many()
            .filter(evaluation_artifact_metadata::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_results::Entity::delete_many()
            .filter(evaluation_results::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_audit_events::Entity::delete_many()
            .filter(evaluation_audit_events::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_outbox_events::Entity::delete_many()
            .filter(evaluation_outbox_events::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_target_snapshots::Entity::delete_many()
            .filter(evaluation_target_snapshots::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_command_receipts::Entity::delete_many()
            .filter(evaluation_command_receipts::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
    }
    evaluation_runs::Entity::delete_many()
        .filter(evaluation_runs::Column::DefinitionVersionId.in_subquery(versions_of_definition()))
        .exec(db)
        .await
        .expect("clean up evaluation test runs");
    evaluation_command_receipts::Entity::delete_many()
        .filter(
            Condition::any()
                .add(evaluation_command_receipts::Column::DefinitionId.eq(definition))
                .add(
                    evaluation_command_receipts::Column::DefinitionVersionId
                        .in_subquery(versions_of_definition()),
                ),
        )
        .exec(db)
        .await
        .expect("clean up evaluation test command receipts");
    evaluation_audit_events::Entity::delete_many()
        .filter(evaluation_audit_events::Column::DefinitionId.eq(definition))
        .exec(db)
        .await
        .expect("clean up evaluation test definition audit events");
    evaluation_definition_versions::Entity::delete_many()
        .filter(evaluation_definition_versions::Column::DefinitionId.eq(definition))
        .exec(db)
        .await
        .expect("clean up evaluation test definition versions");
    evaluation_definition_drafts::Entity::delete_many()
        .filter(evaluation_definition_drafts::Column::DefinitionId.eq(definition))
        .exec(db)
        .await
        .expect("clean up evaluation test definition draft");
    evaluation_definitions::Entity::delete_by_id(definition)
        .exec(db)
        .await
        .expect("clean up evaluation test definition");
    evaluation_target_projections::Entity::delete_many()
        .filter(
            evaluation_target_projections::Column::TargetKind
                .eq(enums::EvaluationTargetKind::AgentVersion),
        )
        .filter(
            evaluation_target_projections::Column::TargetId.in_subquery(
                agent_versions::Entity::find()
                    .select_only()
                    .column(agent_versions::Column::Id)
                    .filter(agent_versions::Column::AgentId.eq(uuid(agent_id)))
                    .into_query(),
            ),
        )
        .exec(db)
        .await
        .expect("clean up evaluation test target projections");
}

/// Covers the evaluation GraphQL surface end to end against a real published agent version:
/// create/validate/publish a definition, the project's computed `compatibleEvaluationTargets`,
/// `runEvaluation`, driving the local outbox worker in-process along the `evaluation-worker`
/// subcommand's own delivery path, then the generated `evaluationRuns` read with
/// its four fact lists, a lifecycle-conflict refusal, and rerun.
#[tokio::test]
#[ignore]
async fn evaluation_definition_and_run_round_trip() {
    let _guard = lock_project_agents().await;
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_agent_draft_test_agent_by_slug(
        &db,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-evaluation-agent",
    )
    .await;

    let membership_id = grant_project_role(
        &db,
        "50000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000001",
        hive_persistence::entity::enums::ProjectRoleCode::ProjectAdmin,
    )
    .await;

    let create_agent_body = graphql_as(
        &router,
        &cookie,
        "mutation { createAgentDraft(input: { projectId: \"50000000-0000-0000-0000-000000000001\", displayName: \"HTTP Integration Evaluation Agent\" }) \
            { agentDraft { agentId } problems { code } } }",
    )
    .await;
    let agent_id = create_agent_body["data"]["createAgentDraft"]["agentDraft"]["agentId"]
        .as_str()
        .unwrap()
        .to_string();
    let update_query = format!(
        "mutation {{ updateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 1, \
            document: {{ general: {{ displayName: \"HTTP Integration Evaluation Agent\" }}, instructions: {{ source: \"Do the thing.\" }}, limits: {{ maxTokens: 4096 }} }} }}) \
            {{ agentDraft {{ revision }} problems {{ code }} }} }}"
    );
    graphql_as(&router, &cookie, &update_query).await;
    let validate_agent_query = format!(
        "mutation {{ validateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 2 }}) \
            {{ agentDraft {{ revision }} problems {{ code }} }} }}"
    );
    let validate_agent_body = graphql_as(&router, &cookie, &validate_agent_query).await;
    let agent_revision = validate_agent_body["data"]["validateAgentDraft"]["agentDraft"]
        ["revision"]
        .as_i64()
        .unwrap();
    let publish_agent_query = format!(
        "mutation {{ publishAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: {agent_revision}, warningsAcknowledged: true }}) \
            {{ agentVersion {{ id }} problems {{ code }} }} }}"
    );
    let publish_agent_body = graphql_as(&router, &cookie, &publish_agent_query).await;
    let agent_version_id = publish_agent_body["data"]["publishAgentDraft"]["agentVersion"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createEvaluationDefinition(input: { projectId: \"50000000-0000-0000-0000-000000000001\", slug: \"http-integration-eval\", \
            idempotencyKey: \"http-integration-eval-create\" }) { definition { id draft { revision } } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_body["data"]["createEvaluationDefinition"]["problems"],
        serde_json::json!([])
    );
    let definition_id = create_body["data"]["createEvaluationDefinition"]["definition"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let validate_query = format!(
        "mutation {{ validateEvaluationDefinitionDraft(input: {{ definitionId: \"{definition_id}\", expectedRevision: 1, idempotencyKey: \"http-integration-eval-validate\" }}) \
            {{ definition {{ draft {{ revision validationStatus }} }} problems {{ code }} }} }}"
    );
    let validate_body = graphql_as(&router, &cookie, &validate_query).await;
    assert_eq!(
        validate_body["data"]["validateEvaluationDefinitionDraft"]["definition"]["draft"]
            ["validationStatus"],
        "VALID"
    );
    let validated_revision = validate_body["data"]["validateEvaluationDefinitionDraft"]
        ["definition"]["draft"]["revision"]
        .as_i64()
        .unwrap();

    let publish_query = format!(
        "mutation {{ publishEvaluationDefinitionDraft(input: {{ definitionId: \"{definition_id}\", expectedRevision: {validated_revision}, idempotencyKey: \"http-integration-eval-publish\" }}) \
            {{ version {{ id versionNumber }} problems {{ code }} }} }}"
    );
    let publish_body = graphql_as(&router, &cookie, &publish_query).await;
    assert_eq!(
        publish_body["data"]["publishEvaluationDefinitionDraft"]["problems"],
        serde_json::json!([])
    );
    let version_id = publish_body["data"]["publishEvaluationDefinitionDraft"]["version"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The candidate targets of a published version are the computed field on the project, over
    // the generated `evaluationTargetProjections` rows.
    let targets_query = format!(
        "query {{ projects(filters: {{ id: {{ eq: \"50000000-0000-0000-0000-000000000001\" }} }}) \
            {{ nodes {{ compatibleEvaluationTargets(definitionVersionId: \"{version_id}\") {{ targetKind targetId }} }} }} }}"
    );
    let targets_body = graphql_as(&router, &cookie, &targets_query).await;
    let targets = targets_body["data"]["projects"]["nodes"][0]["compatibleEvaluationTargets"]
        .as_array()
        .unwrap();
    assert!(targets
        .iter()
        .any(|target| target["targetKind"] == "AGENT_VERSION"
            && target["targetId"] == agent_version_id));

    let environment_id = "e1300000-0000-0000-0000-000000000001";
    let run_query = format!(
        "mutation {{ runEvaluation(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", definitionVersionId: \"{version_id}\", \
            targetKind: AGENT_VERSION, targetId: \"{agent_version_id}\", environmentDefinitionVersionId: \"{environment_id}\", \
            idempotencyKey: \"http-integration-eval-run\" }}) {{ run {{ id lifecycleStatus generation }} problems {{ code message }} }} }}"
    );
    let run_body = graphql_as(&router, &cookie, &run_query).await;
    assert_eq!(
        run_body["data"]["runEvaluation"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        run_body["data"]["runEvaluation"]["run"]["lifecycleStatus"],
        "QUEUED"
    );
    let run_id = run_body["data"]["runEvaluation"]["run"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Drives the same claim -> decide -> commit -> delivered cycle the `evaluation-worker`
    // subcommand's outer loop calls, in-process: no integration test here spawns a second process
    // for worker-delivered state.
    let worker_db = sea_orm::Database::connect(test_database_url())
        .await
        .expect("connect the evaluation worker to the test database");
    let worker = hive_application::evaluation::LocalEvaluationWorker::new(
        hive_persistence::evaluation::PgEvaluationWorkStore::new(worker_db),
        Box::new(hive_application::evaluation::LocalPromptCaseFixtureAdapter),
        "http-integration-evaluation-worker".to_string(),
    );
    worker
        .run_batch(10)
        .await
        .expect("the evaluation worker batch completes");

    // The run and its four fact lists, every one a generated entity read ordered by its own key.
    let run_detail_query = format!(
        "query {{ evaluationRuns(filters: {{ id: {{ eq: \"{run_id}\" }} }}) \
            {{ nodes {{ id lifecycleStatus outcomeCategory generation durationMillis deploymentEvidenceDisposition \
                target {{ agentContentDigest }} }} }} \
          evaluationCaseRuns(filters: {{ runId: {{ eq: \"{run_id}\" }} }}, orderBy: {{ ordinal: ASC, id: ASC }}) {{ nodes {{ caseKey passed }} }} \
          evaluationMetricResults(filters: {{ runId: {{ eq: \"{run_id}\" }} }}, orderBy: {{ metricCode: ASC, id: ASC }}) {{ nodes {{ metricCode value passed }} }} \
          evaluationArtifactMetadata(filters: {{ runId: {{ eq: \"{run_id}\" }} }}, orderBy: {{ artifactKind: ASC, id: ASC }}) {{ nodes {{ artifactKind }} }} \
          evaluationAuditEvents(filters: {{ runId: {{ eq: \"{run_id}\" }} }}, orderBy: {{ occurredAt: DESC, id: DESC }}) {{ nodes {{ action summary }} }} }}"
    );
    let run_detail_body = graphql_as(&router, &cookie, &run_detail_query).await;
    let data = &run_detail_body["data"];
    let run_detail = &data["evaluationRuns"]["nodes"][0];
    assert_eq!(run_detail["lifecycleStatus"], "COMPLETED");
    assert_eq!(run_detail["outcomeCategory"], "PASSED");
    assert_eq!(
        run_detail["deploymentEvidenceDisposition"],
        "NOT_A_DEPLOYMENT"
    );
    assert!(run_detail["target"]["agentContentDigest"].is_string());
    assert_eq!(data["evaluationCaseRuns"]["nodes"][0]["passed"], true);
    assert_eq!(data["evaluationMetricResults"]["nodes"][0]["passed"], true);
    assert_eq!(
        data["evaluationArtifactMetadata"]["nodes"][0]["artifactKind"],
        "LOCAL_SUMMARY"
    );
    assert!(!data["evaluationAuditEvents"]["nodes"]
        .as_array()
        .unwrap()
        .is_empty());
    let completed_generation = run_detail["generation"].as_i64().unwrap();

    // The run is terminal (COMPLETED), so cancel is refused as a lifecycle conflict regardless of
    // whether the supplied generation happens to still be current.
    let cancel_query = format!(
        "mutation {{ cancelEvaluation(input: {{ runId: \"{run_id}\", expectedGeneration: {completed_generation}, idempotencyKey: \"http-integration-eval-cancel\" }}) \
            {{ run {{ id }} problems {{ code }} }} }}"
    );
    let cancel_body = graphql_as(&router, &cookie, &cancel_query).await;
    assert_eq!(
        cancel_body["data"]["cancelEvaluation"]["run"],
        serde_json::Value::Null
    );
    assert_eq!(
        cancel_body["data"]["cancelEvaluation"]["problems"][0]["code"],
        "LIFECYCLE_CONFLICT"
    );

    let rerun_query = format!(
        "mutation {{ rerunEvaluation(input: {{ runId: \"{run_id}\", idempotencyKey: \"http-integration-eval-rerun\" }}) \
            {{ run {{ id sourceRunId lifecycleStatus }} problems {{ code }} }} }}"
    );
    let rerun_body = graphql_as(&router, &cookie, &rerun_query).await;
    assert_eq!(
        rerun_body["data"]["rerunEvaluation"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        rerun_body["data"]["rerunEvaluation"]["run"]["sourceRunId"],
        run_id
    );

    {
        use sea_orm::EntityTrait;
        hive_persistence::entity::evaluation_worker_heartbeats::Entity::delete_by_id(
            "http-integration-evaluation-worker".to_string(),
        )
        .exec(&db)
        .await
        .expect("clean up the in-process evaluation worker's heartbeat row");
    }
    revoke_project_membership(&db, membership_id).await;
    delete_evaluation_test_fixtures(&db, &definition_id, &agent_id).await;
    delete_deployment_test_fixtures(&db, &agent_id).await;
    delete_agent_draft_test_agent(&db, &agent_id).await;
}

/// The stored evaluation document columns and an evaluation audit event's raw material are not
/// part of the generated API: no principal can select, filter or order on them. `canonicalDocument`
/// and `diagnostics` exist only as the computed fields `EVALUATION_DEFINITION.AUTHOR` gates, and
/// `summary` as the one fact a run's audit list ever showed.
#[tokio::test]
#[ignore]
async fn evaluation_reads_do_not_expose_the_raw_document_or_fact_columns() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    for query in [
        "query { evaluationDefinitionDrafts(filters: { canonicalDocument: { eq: \"{}\" } }) { nodes { revision } } }",
        "query { evaluationDefinitionDrafts(orderBy: { canonicalDocument: ASC }) { nodes { revision } } }",
        "query { evaluationDefinitionVersions(orderBy: { canonicalDocument: ASC }) { nodes { id } } }",
        "query { evaluationAuditEvents { nodes { facts } } }",
        "query { evaluationAuditEvents { nodes { sourceIp } } }",
        "query { evaluationAuditEvents { nodes { userAgent } } }",
    ] {
        let body = graphql_as(&router, &cookie, query).await;
        assert!(
            body["errors"][0]["message"].is_string(),
            "{query} must be refused: {body}"
        );
        assert!(body.get("data").is_none_or(serde_json::Value::is_null));
    }
}

/// A principal with no evaluation grant reads no definition, run or fact row, and is offered no
/// candidate target. Bea (00000000-...-0002) holds no role on organization 10000000-...-0001.
#[tokio::test]
#[ignore]
async fn evaluation_reads_are_empty_for_a_principal_without_an_evaluation_grant() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;
    for field in [
        "evaluationDefinitions",
        "evaluationDefinitionDrafts",
        "evaluationDefinitionVersions",
        "evaluationRuns",
        "evaluationCaseRuns",
        "evaluationMetricResults",
        "evaluationArtifactMetadata",
        "evaluationAuditEvents",
        "evaluationTargetSnapshots",
        "evaluationTargetProjections",
    ] {
        let body = graphql_as(
            &router,
            &cookie,
            &format!("query {{ {field}(pagination: {{ page: {{ limit: 10, page: 0 }} }}) {{ nodes {{ __typename }} }} }}"),
        )
        .await;
        assert!(body.get("errors").is_none(), "{body}");
        assert_eq!(
            body["data"][field]["nodes"],
            serde_json::json!([]),
            "{body}"
        );
    }
    let targets = graphql_as(
        &router,
        &cookie,
        "query { projects(filters: { id: { eq: \"50000000-0000-0000-0000-000000000001\" } }) \
            { nodes { compatibleEvaluationTargets(definitionVersionId: \"00000000-0000-0000-0000-000000000000\") { targetId } } } }",
    )
    .await;
    assert!(targets.get("errors").is_none(), "{targets}");
    assert_eq!(
        targets["data"]["projects"]["nodes"],
        serde_json::json!([]),
        "{targets}"
    );
}
