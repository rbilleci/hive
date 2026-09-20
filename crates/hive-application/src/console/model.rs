use uuid::Uuid;

/// A project safe to include in the signed-in user's context chooser. Ports
/// `ConsoleProject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleProject {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

/// An organization and its visible projects for shared-console navigation. Ports
/// `ConsoleOrganization`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleOrganization {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub projects: Vec<ConsoleProject>,
}

/// One server-calculated capability hint for a visible console scope. Ports
/// `ConsoleCapability` (the SDL's `EffectiveCapability`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleCapability {
    pub code: String,
    pub scope_type: String,
    pub scope_id: Uuid,
}

/// Immutable context and default-deny capability projection for one verified
/// local principal. Ports `ConsoleContext`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleContext {
    pub principal_id: Uuid,
    pub display_name: String,
    pub organizations: Vec<ConsoleOrganization>,
    pub capabilities: Vec<ConsoleCapability>,
    pub revision: String,
}

/// Approved, non-secret display preferences owned by exactly one current
/// principal. Ports `UserDisplayPreferences`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDisplayPreferences {
    pub principal_id: Uuid,
    pub color_scheme: String,
    pub density: String,
    pub sidebar_state: String,
}

impl UserDisplayPreferences {
    pub fn defaults(principal_id: Uuid) -> Self {
        Self {
            principal_id,
            color_scheme: "LIGHT".to_string(),
            density: "COMFORTABLE".to_string(),
            sidebar_state: "EXPANDED".to_string(),
        }
    }
}

/// Typed outcome for a guarded, private display-preference write. Ports
/// `DisplayPreferencesProblem`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayPreferencesProblem {
    NotFound,
    InvalidPreferences,
}

/// A successful preference snapshot or one typed refusal; never both. Ports
/// `DisplayPreferencesMutationResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayPreferencesMutationResult {
    pub preferences: Option<UserDisplayPreferences>,
    pub problem: Option<DisplayPreferencesProblem>,
}

impl DisplayPreferencesMutationResult {
    pub fn success(preferences: UserDisplayPreferences) -> Self {
        Self {
            preferences: Some(preferences),
            problem: None,
        }
    }

    pub fn refused(problem: DisplayPreferencesProblem) -> Self {
        Self {
            preferences: None,
            problem: Some(problem),
        }
    }
}
