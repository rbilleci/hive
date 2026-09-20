//! Ports `AgentDraftRepository` (the interface) and `AgentDraftEditorService`:
//! canonicalizes route identifiers before the repository applies visibility
//! and current write authority.

use crate::agent::draft::{
    AgentDraft, AgentDraftMutationProblem, AgentDraftMutationResult, AgentDraftReview,
    AgentVersion, AgentVersionComparison,
};
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Persistence boundary for member-scoped draft reads and atomic
/// compare-and-set commands. Ports `AgentDraftRepository`.
#[async_trait]
pub trait AgentDraftRepository: Send + Sync {
    async fn find_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Option<AgentDraft>, RepositoryError>;

    async fn update_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        expected_revision: i64,
        document: String,
    ) -> Result<AgentDraftMutationResult, RepositoryError>;

    async fn validate_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        expected_revision: i64,
    ) -> Result<AgentDraftMutationResult, RepositoryError>;

    async fn create_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        display_name: String,
        slug: Option<String>,
    ) -> Result<AgentDraftMutationResult, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn publish_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        expected_revision: i64,
        warnings_acknowledged: bool,
    ) -> Result<AgentDraftMutationResult, RepositoryError>;

    async fn review_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Option<AgentDraftReview>, RepositoryError>;

    async fn versions(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Option<Vec<AgentVersion>>, RepositoryError>;

    async fn version(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<AgentVersion>, RepositoryError>;

    async fn compare_versions(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        from_version_id: Uuid,
        to_version_id: Uuid,
    ) -> Result<Option<AgentVersionComparison>, RepositoryError>;
}

fn parsed(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

/// Ports `AgentDraftEditorService`.
pub struct AgentDraftEditorService<R: AgentDraftRepository> {
    repository: R,
}

impl<R: AgentDraftRepository> AgentDraftEditorService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn find_draft(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
    ) -> Result<Option<AgentDraft>, RepositoryError> {
        let (Some(project), Some(agent)) = (parsed(project_id), parsed(agent_id)) else {
            return Ok(None);
        };
        self.repository
            .find_draft(principal_id, project, agent)
            .await
    }

    pub async fn update_draft(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
        expected_revision: i64,
        document: String,
    ) -> Result<AgentDraftMutationResult, RepositoryError> {
        let (Some(project), Some(agent)) = (parsed(project_id), parsed(agent_id)) else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::not_found(),
            ));
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
    ) -> Result<AgentDraftMutationResult, RepositoryError> {
        let (Some(project), Some(agent)) = (parsed(project_id), parsed(agent_id)) else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::not_found(),
            ));
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
    ) -> Result<AgentDraftMutationResult, RepositoryError> {
        let Some(project) = parsed(project_id) else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::not_found(),
            ));
        };
        self.repository
            .create_draft(principal_id, project, display_name, slug)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn publish_draft(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
        expected_revision: i64,
        warnings_acknowledged: bool,
    ) -> Result<AgentDraftMutationResult, RepositoryError> {
        let (Some(project), Some(agent)) = (parsed(project_id), parsed(agent_id)) else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::not_found(),
            ));
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

    pub async fn review_draft(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
    ) -> Result<Option<AgentDraftReview>, RepositoryError> {
        let (Some(project), Some(agent)) = (parsed(project_id), parsed(agent_id)) else {
            return Ok(None);
        };
        self.repository
            .review_draft(principal_id, project, agent)
            .await
    }

    pub async fn versions(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
    ) -> Result<Option<Vec<AgentVersion>>, RepositoryError> {
        let (Some(project), Some(agent)) = (parsed(project_id), parsed(agent_id)) else {
            return Ok(None);
        };
        self.repository.versions(principal_id, project, agent).await
    }

    pub async fn version(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
        version_id: &str,
    ) -> Result<Option<AgentVersion>, RepositoryError> {
        let (Some(project), Some(agent), Some(version)) =
            (parsed(project_id), parsed(agent_id), parsed(version_id))
        else {
            return Ok(None);
        };
        self.repository
            .version(principal_id, project, agent, version)
            .await
    }

    pub async fn compare_versions(
        &self,
        principal_id: Uuid,
        project_id: &str,
        agent_id: &str,
        from_version_id: &str,
        to_version_id: &str,
    ) -> Result<Option<AgentVersionComparison>, RepositoryError> {
        let (Some(project), Some(agent), Some(from), Some(to)) = (
            parsed(project_id),
            parsed(agent_id),
            parsed(from_version_id),
            parsed(to_version_id),
        ) else {
            return Ok(None);
        };
        self.repository
            .compare_versions(principal_id, project, agent, from, to)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingRepository {
        find_draft_calls: Mutex<Vec<(Uuid, Uuid, Uuid)>>,
    }

    #[async_trait]
    impl AgentDraftRepository for RecordingRepository {
        async fn find_draft(
            &self,
            principal_id: Uuid,
            project_id: Uuid,
            agent_id: Uuid,
        ) -> Result<Option<AgentDraft>, RepositoryError> {
            self.find_draft_calls
                .lock()
                .unwrap()
                .push((principal_id, project_id, agent_id));
            Ok(None)
        }

        async fn update_draft(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _a: Uuid,
            _r: i64,
            _d: String,
        ) -> Result<AgentDraftMutationResult, RepositoryError> {
            Ok(AgentDraftMutationResult::default())
        }

        async fn validate_draft(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _a: Uuid,
            _r: i64,
        ) -> Result<AgentDraftMutationResult, RepositoryError> {
            Ok(AgentDraftMutationResult::default())
        }

        async fn create_draft(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _d: String,
            _s: Option<String>,
        ) -> Result<AgentDraftMutationResult, RepositoryError> {
            Ok(AgentDraftMutationResult::default())
        }

        async fn publish_draft(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _a: Uuid,
            _r: i64,
            _w: bool,
        ) -> Result<AgentDraftMutationResult, RepositoryError> {
            Ok(AgentDraftMutationResult::default())
        }

        async fn review_draft(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _a: Uuid,
        ) -> Result<Option<AgentDraftReview>, RepositoryError> {
            Ok(None)
        }

        async fn versions(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _a: Uuid,
        ) -> Result<Option<Vec<AgentVersion>>, RepositoryError> {
            Ok(None)
        }

        async fn version(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _a: Uuid,
            _v: Uuid,
        ) -> Result<Option<AgentVersion>, RepositoryError> {
            Ok(None)
        }

        async fn compare_versions(
            &self,
            _p: Uuid,
            _pr: Uuid,
            _a: Uuid,
            _f: Uuid,
            _t: Uuid,
        ) -> Result<Option<AgentVersionComparison>, RepositoryError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn find_draft_passes_the_parsed_canonical_identifiers_to_the_repository() {
        let repository = RecordingRepository::default();
        let service = AgentDraftEditorService::new(repository);
        let principal = Uuid::new_v4();
        let project = Uuid::new_v4();
        let agent = Uuid::new_v4();
        service
            .find_draft(principal, &project.to_string(), &agent.to_string())
            .await
            .unwrap();
        let calls = service.repository.find_draft_calls.lock().unwrap();
        assert_eq!(calls.as_slice(), [(principal, project, agent)]);
    }

    #[tokio::test]
    async fn find_draft_is_none_for_a_malformed_project_id_rather_than_an_error() {
        let service = AgentDraftEditorService::new(RecordingRepository::default());
        let result = service
            .find_draft(Uuid::new_v4(), "not-a-uuid", &Uuid::new_v4().to_string())
            .await
            .unwrap();
        assert!(result.is_none());
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
            super::super::draft::AgentDraftProblemKind::NotFound
        );
    }

    #[tokio::test]
    async fn compare_versions_is_none_for_a_malformed_version_id() {
        let service = AgentDraftEditorService::new(RecordingRepository::default());
        let result = service
            .compare_versions(
                Uuid::new_v4(),
                &Uuid::new_v4().to_string(),
                &Uuid::new_v4().to_string(),
                "not-a-uuid",
                &Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
