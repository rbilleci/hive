use uuid::Uuid;

/// Approved, non-secret display preferences owned by exactly one current
/// principal. Ports `UserDisplayPreferences`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDisplayPreferences {
    pub principal_id: Uuid,
    pub color_scheme: String,
    pub density: String,
    pub sidebar_state: String,
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
