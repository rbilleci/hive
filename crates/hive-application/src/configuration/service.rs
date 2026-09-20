//! Ports `ConfigurationService`: command-shape validation in front of the
//! repository. PostgreSQL repeats every authority and lifecycle check inside
//! its own transaction; this layer only rejects shapes that could never be
//! valid regardless of who is asking.

use super::canonical::sorted;
use super::identity::{resource_identity, TypedReference};
use super::models::{ConfigurationMutationResult, ConfigurationProblem, McpServerConfiguration};
use super::repository::{ConfigurationRepository, RepositoryError};
use std::collections::HashSet;
use std::sync::LazyLock;
use uuid::Uuid;

const KINDS: [&str; 3] = ["PROMPT", "POLICY", "MODEL_PROFILE"];
const ENVIRONMENTS: [&str; 3] = ["DEVELOPMENT", "STAGING", "PRODUCTION"];
const TRANSPORTS: [&str; 2] = ["STDIO", "REMOTE"];
const LEGACY_LIFECYCLE_STATUSES: [&str; 3] = ["ACTIVE", "DISABLED", "ARCHIVED"];

static NAME_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z][A-Za-z0-9 _-]{1,80}$").unwrap());
static SERVER_ID_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[a-z][a-z0-9-]{0,62}$").unwrap());
static BINDING_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^redacted://[a-z0-9/_-]{3,180}$").unwrap());
static CAPABILITY_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z][A-Za-z0-9_.:/-]{0,119}$").unwrap());
static STDIO_COMMAND_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z0-9_./-]{1,512}$").unwrap());
static BEARER_BASIC_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)^(?:bearer|basic)\s+.+$").unwrap());
static SENSITIVE_NAME_PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)(?:password|passwd|secret|token|api[-_]?key|authorization|credential|bearer|basic|header|env(?:ironment)?)").unwrap()
});

fn parsed(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

/// Ports `ConfigurationService.references`: all-or-nothing parse, duplicates
/// rejected, sorted by canonical `value()` order. `None` on any failure — the
/// caller's `values == null` (never reached from GraphQL's `[String!]!`
/// inputs) already maps to an empty `Vec`, not this `None` path.
fn references(values: &[String]) -> Option<Vec<TypedReference>> {
    let parsed: Vec<TypedReference> = values
        .iter()
        .filter_map(|value| TypedReference::parse(value))
        .collect();
    if parsed.len() != values.len() {
        return None;
    }
    let unique: HashSet<&TypedReference> = parsed.iter().collect();
    if unique.len() != parsed.len() {
        return None;
    }
    Some(sorted(parsed))
}

fn valid_resource(
    kind: &str,
    name: &str,
    content: &str,
    refs: Option<&Vec<TypedReference>>,
    identity: Option<&str>,
) -> bool {
    KINDS.contains(&kind)
        && NAME_PATTERN.is_match(name)
        && content.chars().count() <= 16000
        && refs.is_some()
        && identity.is_some()
}

fn valid_list(values: &[String], limit: usize, width: usize) -> bool {
    if values.len() > limit {
        return false;
    }
    let unique: HashSet<&String> = values.iter().collect();
    unique.len() == values.len()
        && values
            .iter()
            .all(|value| !value.trim().is_empty() && value.chars().count() <= width)
}

fn valid_capabilities(values: &[String]) -> bool {
    valid_list(values, 128, 120)
        && values
            .iter()
            .all(|value| CAPABILITY_PATTERN.is_match(value))
}

fn sensitive_argument(value: &str) -> bool {
    let stripped = value.trim();
    if BEARER_BASIC_PATTERN.is_match(stripped) {
        return true;
    }
    let without_dashes = stripped.trim_start_matches('-');
    let name = without_dashes.split(['=', ':']).next().unwrap_or("");
    SENSITIVE_NAME_PATTERN.is_match(name)
}

fn contains_sensitive_literal(arguments: &[String]) -> bool {
    arguments.iter().any(|value| sensitive_argument(value))
}

fn decode_www_form_component(value: &str) -> String {
    percent_encoding::percent_decode_str(&value.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

/// Ports `ConfigurationService.validRemoteUrl`. `[&;]`-splits the query
/// string (an older convention `url::form_urlencoded::parse`, which only
/// splits on `&`, does not follow) so a sensitive parameter hidden after a
/// `;` is still caught.
fn valid_remote_url(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 500 {
        return false;
    }
    let Ok(url) = url::Url::parse(trimmed) else {
        return false;
    };
    if !url.scheme().eq_ignore_ascii_case("https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(query) = url.query() else {
        return true;
    };
    for segment in query.split(['&', ';']) {
        let encoded_name = segment.split('=').next().unwrap_or("");
        let name = decode_www_form_component(encoded_name);
        if SENSITIVE_NAME_PATTERN.is_match(&name) {
            return false;
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn valid_mcp_server(
    server_id: &str,
    name: &str,
    definition: Option<&TypedReference>,
    environment: &str,
    transport: &str,
    command: Option<&str>,
    arguments: &[String],
    remote_url: Option<&str>,
    bindings: &[String],
    tools: &[String],
    resources: &[String],
    prompts: &[String],
) -> bool {
    let shape_ok = SERVER_ID_PATTERN.is_match(server_id)
        && NAME_PATTERN.is_match(name)
        && definition.is_some_and(|value| value.kind == "tool")
        && ENVIRONMENTS.contains(&environment)
        && TRANSPORTS.contains(&transport)
        && valid_list(arguments, 64, 512)
        && valid_list(bindings, 32, 190)
        && bindings.iter().all(|value| BINDING_PATTERN.is_match(value))
        && valid_capabilities(tools)
        && valid_capabilities(resources)
        && valid_capabilities(prompts);
    if !shape_ok || contains_sensitive_literal(arguments) {
        return false;
    }
    if transport == "STDIO" {
        command.is_some_and(|value| STDIO_COMMAND_PATTERN.is_match(value))
            && remote_url.map(str::trim).unwrap_or("").is_empty()
    } else {
        valid_remote_url(remote_url)
            && command.map(str::trim).unwrap_or("").is_empty()
            && arguments.is_empty()
    }
}

fn valid_legacy_tool(
    revision: i64,
    name: &str,
    definition: Option<&TypedReference>,
    environment: &str,
    reference: &str,
    lifecycle: &str,
    rotation: &str,
) -> bool {
    revision >= 0
        && NAME_PATTERN.is_match(name)
        && definition.is_some_and(|value| value.kind == "tool")
        && ENVIRONMENTS.contains(&environment)
        && BINDING_PATTERN.is_match(reference)
        && LEGACY_LIFECYCLE_STATUSES.contains(&lifecycle)
        && rotation.chars().count() <= 240
}

fn without_secret_material(value: McpServerConfiguration) -> McpServerConfiguration {
    let unsafe_arguments = contains_sensitive_literal(&value.arguments);
    let unsafe_remote = value
        .remote_url
        .as_deref()
        .is_some_and(|url| !valid_remote_url(Some(url)));
    if !unsafe_arguments && !unsafe_remote {
        return value;
    }
    McpServerConfiguration {
        arguments: if unsafe_arguments {
            Vec::new()
        } else {
            value.arguments
        },
        remote_url: if unsafe_remote {
            None
        } else {
            value.remote_url
        },
        status: "INCOMPLETE".to_string(),
        ..value
    }
}

fn safe_list(values: Vec<String>) -> Vec<String> {
    let mut values: Vec<String> = values
        .iter()
        .map(|value| value.trim().to_string())
        .collect();
    values.sort();
    values
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Ports `ConfigurationService`.
pub struct ConfigurationService<R: ConfigurationRepository> {
    repository: R,
}

impl<R: ConfigurationRepository> ConfigurationService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn catalog(
        &self,
        principal: Uuid,
        organization_id: &str,
    ) -> Result<Option<super::models::CatalogRelease>, RepositoryError> {
        let Some(id) = parsed(organization_id) else {
            return Ok(None);
        };
        self.repository.catalog(principal, id).await
    }

    pub async fn resources(
        &self,
        principal: Uuid,
        project_id: &str,
        kind: Option<&str>,
    ) -> Result<Option<Vec<super::models::ReusableResource>>, RepositoryError> {
        let Some(id) = parsed(project_id) else {
            return Ok(None);
        };
        self.repository
            .resources(principal, id, kind.map(str::to_string))
            .await
    }

    pub async fn resource(
        &self,
        principal: Uuid,
        project_id: &str,
        resource_id: &str,
    ) -> Result<Option<super::models::ReusableResource>, RepositoryError> {
        let (Some(project), Some(resource)) = (parsed(project_id), parsed(resource_id)) else {
            return Ok(None);
        };
        self.repository.resource(principal, project, resource).await
    }

    pub async fn mcp_servers(
        &self,
        principal: Uuid,
        project_id: &str,
    ) -> Result<Option<Vec<McpServerConfiguration>>, RepositoryError> {
        let Some(id) = parsed(project_id) else {
            return Ok(None);
        };
        let servers = self.repository.mcp_servers(principal, id).await?;
        Ok(servers.map(|values| values.into_iter().map(without_secret_material).collect()))
    }

    pub async fn create(
        &self,
        actor: Uuid,
        project_id: &str,
        kind: &str,
        name: &str,
        content: &str,
        refs: Vec<String>,
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        let dependencies = references(&refs);
        let identity = resource_identity::from_display_name(name);
        let Some(project) = parsed(project_id) else {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::unavailable(),
            ));
        };
        if !valid_resource(
            kind,
            name,
            content,
            dependencies.as_ref(),
            identity.as_deref(),
        ) {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        self.repository
            .create_resource(
                actor,
                project,
                kind.to_string(),
                name.trim().to_string(),
                identity.unwrap(),
                content.to_string(),
                dependencies.unwrap(),
            )
            .await
    }

    pub async fn update(
        &self,
        actor: Uuid,
        project_id: &str,
        resource_id: &str,
        expected_revision: i64,
        content: &str,
        refs: Vec<String>,
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        let (Some(project), Some(resource)) = (parsed(project_id), parsed(resource_id)) else {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::unavailable(),
            ));
        };
        let dependencies = references(&refs);
        if expected_revision < 1 || content.chars().count() > 16000 || dependencies.is_none() {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        self.repository
            .update_draft(
                actor,
                project,
                resource,
                expected_revision,
                content.to_string(),
                dependencies.unwrap(),
            )
            .await
    }

    pub async fn validate(
        &self,
        actor: Uuid,
        project_id: &str,
        resource_id: &str,
        expected_revision: i64,
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        let (Some(project), Some(resource)) = (parsed(project_id), parsed(resource_id)) else {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::unavailable(),
            ));
        };
        if expected_revision < 1 {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        self.repository
            .validate(actor, project, resource, expected_revision)
            .await
    }

    pub async fn publish(
        &self,
        actor: Uuid,
        project_id: &str,
        resource_id: &str,
        expected_revision: i64,
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        let (Some(project), Some(resource)) = (parsed(project_id), parsed(resource_id)) else {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::unavailable(),
            ));
        };
        if expected_revision < 1 {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        self.repository
            .publish(actor, project, resource, expected_revision)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_mcp_server(
        &self,
        actor: Uuid,
        project_id: &str,
        server_id: &str,
        name: &str,
        definition: &str,
        environment: &str,
        enabled: bool,
        transport_type: &str,
        command: Option<&str>,
        arguments: Vec<String>,
        remote_url: Option<&str>,
        bindings: Vec<String>,
        tools: Vec<String>,
        resources: Vec<String>,
        prompts: Vec<String>,
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        let typed = TypedReference::parse(definition);
        let Some(project) = parsed(project_id) else {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::unavailable(),
            ));
        };
        if !valid_mcp_server(
            server_id,
            name,
            typed.as_ref(),
            environment,
            transport_type,
            command,
            &arguments,
            remote_url,
            &bindings,
            &tools,
            &resources,
            &prompts,
        ) {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        self.repository
            .create_mcp_server(
                actor,
                project,
                server_id.to_string(),
                name.trim().to_string(),
                typed.unwrap(),
                environment.to_string(),
                enabled,
                transport_type.to_string(),
                clean(command),
                safe_list(arguments),
                clean(remote_url),
                safe_list(bindings),
                safe_list(tools),
                safe_list(resources),
                safe_list(prompts),
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_mcp_server(
        &self,
        actor: Uuid,
        project_id: &str,
        server_id: &str,
        expected_revision: i64,
        name: &str,
        definition: &str,
        environment: &str,
        enabled: bool,
        transport_type: &str,
        command: Option<&str>,
        arguments: Vec<String>,
        remote_url: Option<&str>,
        bindings: Vec<String>,
        tools: Vec<String>,
        resources: Vec<String>,
        prompts: Vec<String>,
        lifecycle_status: &str,
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        let typed = TypedReference::parse(definition);
        let (Some(project), Some(server)) = (parsed(project_id), parsed(server_id)) else {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::unavailable(),
            ));
        };
        if expected_revision < 1
            || !["ACTIVE", "ARCHIVED"].contains(&lifecycle_status)
            || !valid_mcp_server(
                "existing",
                name,
                typed.as_ref(),
                environment,
                transport_type,
                command,
                &arguments,
                remote_url,
                &bindings,
                &tools,
                &resources,
                &prompts,
            )
        {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        self.repository
            .update_mcp_server(
                actor,
                project,
                server,
                expected_revision,
                name.trim().to_string(),
                typed.unwrap(),
                environment.to_string(),
                enabled,
                transport_type.to_string(),
                clean(command),
                safe_list(arguments),
                clean(remote_url),
                safe_list(bindings),
                safe_list(tools),
                safe_list(resources),
                safe_list(prompts),
                lifecycle_status.to_string(),
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn save_legacy_tool(
        &self,
        actor: Uuid,
        project_id: &str,
        tool_id: Option<&str>,
        expected_revision: i64,
        name: &str,
        definition: &str,
        environment: &str,
        redacted_secret_reference: &str,
        lifecycle: &str,
        rotation: &str,
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        let typed = TypedReference::parse(definition);
        let tool_id = tool_id.filter(|value| !value.trim().is_empty());
        let tool = match tool_id {
            None => None,
            Some(value) => match parsed(value) {
                Some(id) => Some(id),
                None => {
                    return Ok(ConfigurationMutationResult::refused(
                        ConfigurationProblem::unavailable(),
                    ))
                }
            },
        };
        let Some(project) = parsed(project_id) else {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::unavailable(),
            ));
        };
        if !valid_legacy_tool(
            expected_revision,
            name,
            typed.as_ref(),
            environment,
            redacted_secret_reference,
            lifecycle,
            rotation,
        ) {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        self.repository
            .save_legacy_tool(
                actor,
                project,
                tool,
                expected_revision,
                name.trim().to_string(),
                typed.unwrap(),
                environment.to_string(),
                redacted_secret_reference.trim().to_string(),
                lifecycle.to_string(),
                rotation.trim().to_string(),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_rejects_a_duplicate() {
        let values = vec!["model:a@v1".to_string(), "model:a@v1".to_string()];
        assert!(references(&values).is_none());
    }

    #[test]
    fn references_rejects_an_unparseable_entry() {
        let values = vec!["not-a-reference".to_string()];
        assert!(references(&values).is_none());
    }

    #[test]
    fn references_sorts_by_canonical_value() {
        let values = vec!["model:z@v1".to_string(), "model:a@v1".to_string()];
        let sorted = references(&values).unwrap();
        assert_eq!(
            sorted.iter().map(TypedReference::value).collect::<Vec<_>>(),
            vec!["model:a@v1".to_string(), "model:z@v1".to_string()]
        );
    }

    #[test]
    fn valid_remote_url_rejects_a_sensitive_query_parameter_after_a_semicolon() {
        assert!(!valid_remote_url(Some(
            "https://example.com/a?x=1;api_key=secret"
        )));
    }

    #[test]
    fn valid_remote_url_rejects_userinfo_and_fragment() {
        assert!(!valid_remote_url(Some("https://user:pass@example.com/")));
        assert!(!valid_remote_url(Some("https://example.com/#fragment")));
    }

    #[test]
    fn valid_remote_url_accepts_a_plain_https_url() {
        assert!(valid_remote_url(Some("https://example.com/mcp")));
    }

    #[test]
    fn sensitive_argument_detects_a_bearer_token() {
        assert!(sensitive_argument("Bearer abc123"));
        assert!(sensitive_argument("--api-key=xyz"));
        assert!(!sensitive_argument("--verbose"));
    }
}
