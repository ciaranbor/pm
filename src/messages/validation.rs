use crate::error::{PmError, Result};

/// Validate that a name (agent, sender, feature) is safe for use as a path component.
/// Allows alphanumeric, dashes, and underscores only.
pub fn validate_name(name: &str, kind: &str) -> Result<()> {
    if name.is_empty() {
        return Err(PmError::InvalidAgentName(format!(
            "{kind} name cannot be empty"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(PmError::InvalidAgentName(format!(
            "{kind} name '{name}' contains invalid characters (only alphanumeric, dashes, and underscores allowed)"
        )));
    }
    Ok(())
}
