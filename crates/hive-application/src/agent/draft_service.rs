//! `AgentDraftRepository` (the command boundary) and `AgentDraftEditorService`, which
//! canonicalizes route identifiers before the repository applies visibility and current write
//! authority. Reads are not here: drafts, versions, the review and the version comparison are
//! generated entity reads with computed fields.

use crate::agent::draft::{AgentDraftMutationProblem, AgentDraftMutationResult};
use crate::RepositoryError;
use async_trait::async_trait;
use uuid::Uuid;

/// What a repository's commands answer with.
pub type CommandResult<R> = Result<
    AgentDraftMutationResult<
        <R as AgentDraftRepository>::Draft,
        <R as AgentDraftRepository>::Version,
    >,
    RepositoryError,
>;

/// Persistence boundary for atomic compare-and-set draft commands. `Draft` and `Version` are the
/// repository's own stored rows.
#[async_trait]
pub trait AgentDraftRepository: Send + Sync {
    type Draft: Send;
    type Version: Send;

    async fn update_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        expected_revision: i64,
        document: String,
    ) -> CommandResult<Self>;

    async fn validate_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        expected_revision: i64,
    ) -> CommandResult<Self>;

    async fn create_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        display_name: String,
        slug: Option<String>,
    ) -> CommandResult<Self>;

    async fn publish_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        expected_revision: i64,
        warnings_acknowledged: bool,
    ) -> CommandResult<Self>;
}

fn parsed(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

fn not_found<D, V>() -> AgentDraftMutationResult<D, V> {
    AgentDraftMutationResult::refused(AgentDraftMutationProblem::not_found())
}

pub struct AgentDraftEditorService<R: AgentDraftRepository> {
    repository: R,
}

impl<R: AgentDraftRepository> AgentDraftEditorService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn update_draft(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
        expected_revision: i64,
        document: String,
    ) -> CommandResult<R> {
        let (Some(project), Some(agent)) = (parsed(project_id), parsed(agent_id)) else {
            return Ok(not_found());
        };
        self.repository
            .update_draft(principal_id, project, agent, expected_revision, document)
            .await
    }

    pub async fn validate_draft(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
        expected_revision: i64,
    ) -> CommandResult<R> {
        let (Some(project), Some(agent)) = (parsed(project_id), parsed(agent_id)) else {
            return Ok(not_found());
        };
        self.repository
            .validate_draft(principal_id, project, agent, expected_revision)
            .await
    }

    pub async fn create_draft(
        &self,
        principal_id: Uuid,
        project_id: &str,
        display_name: String,
        slug: Option<String>,
    ) -> CommandResult<R> {
        let Some(project) = parsed(project_id) else {
            return Ok(not_found());
        };
        self.repository
            .create_draft(principal_id, project, display_name, slug)
            .await
    }

    pub async fn publish_draft(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
        expected_revision: i64,
        warnings_acknowledged: bool,
    ) -> CommandResult<R> {
        let (Some(project), Some(agent)) = (parsed(project_id), parsed(agent_id)) else {
            return Ok(not_found());
        };
        self.repository
            .publish_draft(
                principal_id,
                project,
                agent,
                expected_revision,
                warnings_acknowledged,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::draft::AgentDraftProblemKind;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingRepository {
        validate_calls: Mutex<Vec<(Uuid, Uuid, Uuid, i64)>>,
    }

    #[async_trait]
    impl AgentDraftRepository for RecordingRepository {
        type Draft = i64;
        type Version = ();

        async fn update_draft(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _a: Uuid,
            revision: i64,
            _d: String,
        ) -> CommandResult<Self> {
            Ok(AgentDraftMutationResult::success(revision + 1))
        }

        async fn validate_draft(
            &self,
            principal_id: Uuid,
            project_id: Uuid,
            agent_id: Uuid,
            revision: i64,
        ) -> CommandResult<Self> {
            self.validate_calls.lock().unwrap().push((
                principal_id,
                project_id,
                agent_id,
                revision,
            ));
            Ok(AgentDraftMutationResult::success(revision + 1))
        }

        async fn create_draft(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _d: String,
            _s: Option<String>,
        ) -> CommandResult<Self> {
            Ok(AgentDraftMutationResult::success(1))
        }

        async fn publish_draft(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _a: Uuid,
            revision: i64,
            _w: bool,
        ) -> CommandResult<Self> {
            Ok(AgentDraftMutationResult::published(revision, ()))
        }
    }

    #[tokio::test]
    async fn validate_draft_passes_the_parsed_canonical_identifiers_to_the_repository() {
        let service = AgentDraftEditorService::new(RecordingRepository::default());
        let principal = Uuid::new_v4();
        let project = Uuid::new_v4();
        let agent = Uuid::new_v4();
        let result = service
            .validate_draft(principal, &project.to_string(), &agent.to_string(), 4)
            .await
            .unwrap();
        assert_eq!(result.agent_draft, Some(5));
        let calls = service.repository.validate_calls.lock().unwrap();
        assert_eq!(calls.as_slice(), [(principal, project, agent, 4)]);
    }

    #[tokio::test]
    async fn update_draft_is_not_found_for_a_malformed_agent_id_rather_than_an_error() {
        let service = AgentDraftEditorService::new(RecordingRepository::default());
        let result = service
            .update_draft(
                Uuid::new_v4(),
                &Uuid::new_v4().to_string(),
                "not-a-uuid",
                1,
                "{}".to_string(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.problem.unwrap().kind,
            AgentDraftProblemKind::NotFound
        );
    }

    #[tokio::test]
    async fn create_and_publish_are_not_found_for_a_malformed_project_id() {
        let service = AgentDraftEditorService::new(RecordingRepository::default());
        let created = service
            .create_draft(Uuid::new_v4(), "not-a-uuid", "Name".to_string(), None)
            .await
            .unwrap();
        assert_eq!(
            created.problem.unwrap().kind,
            AgentDraftProblemKind::NotFound
        );
        let published = service
            .publish_draft(
                Uuid::new_v4(),
                "not-a-uuid",
                &Uuid::new_v4().to_string(),
                1,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            published.problem.unwrap().kind,
            AgentDraftProblemKind::NotFound
        );
    }
}
