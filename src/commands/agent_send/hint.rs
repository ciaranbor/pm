//! The hint appended to a send's error when its recipient is not an
//! active agent in the target scope, guessing at the sender's mistake.

/// Generate a helpful hint when a recipient agent is not found.
pub(super) fn agent_not_found_hint(
    recipient: &str,
    sender_scope: &str,
    target_scope: &str,
) -> String {
    // If the name contains '@', the user may have used shorthand syntax
    // directly instead of letting dispatch parse it.
    if let Some(pos) = recipient.find('@') {
        let name = &recipient[..pos];
        let scope = &recipient[pos + 1..];
        if !name.is_empty() && !scope.is_empty() {
            return format!(
                "\n  Hint: Did you mean `pm msg send {name}@{scope}` (the @ shorthand is parsed by the CLI, not passed as the agent name)?"
            );
        }
    }

    // Common mistake: sending to "main" agent from a feature scope when they
    // meant to send to an agent in the main scope.
    if recipient == "main" && sender_scope != "main" {
        return "\n  Hint: 'main' is a scope, not an agent. To send to an agent in the main scope, \
             use `pm msg send <agent>@main` or `pm msg reply`"
            .to_string();
    }

    // If sender and target are in different scopes, remind about --project
    if sender_scope != target_scope {
        return format!(
            "\n  Hint: Agent '{recipient}' was not found in scope '{target_scope}'. \
             For cross-project messages, add `--project <name>`"
        );
    }

    // Default: suggest cross-scope syntax
    format!(
        "\n  Hint: The agent may exist in a different scope. \
         Try `pm msg send {recipient}@<scope>` or `--project <name>` for cross-project"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_main_as_recipient_from_feature() {
        let hint = agent_not_found_hint("main", "login", "login");
        assert!(hint.contains("'main' is a scope, not an agent"));
        assert!(hint.contains("pm msg send <agent>@main"));
    }

    #[test]
    fn hint_main_from_main_no_scope_suggestion() {
        // From main scope, "main" as recipient is just a missing agent, not a scope confusion
        let hint = agent_not_found_hint("main", "main", "main");
        assert!(!hint.contains("'main' is a scope"));
        assert!(hint.contains("different scope"));
    }

    #[test]
    fn hint_different_scope_suggests_project() {
        let hint = agent_not_found_hint("reviewer", "login", "other-feat");
        assert!(hint.contains("--project"));
    }

    #[test]
    fn hint_same_scope_default() {
        let hint = agent_not_found_hint("reviewer", "login", "login");
        assert!(hint.contains("different scope"));
        assert!(hint.contains("reviewer@<scope>"));
    }
}
