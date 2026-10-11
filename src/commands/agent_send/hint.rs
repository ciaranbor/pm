//! The hint appended to a send's error when its recipient is not an
//! active agent in the target scope, guessing at the sender's mistake.

use crate::state::scope;

/// Generate a helpful hint when a recipient agent is not found.
/// `orchestrator` is main's lead agent ([`scope::orchestrator`]).
pub(super) fn agent_not_found_hint(
    recipient: &str,
    sender_scope: &str,
    target_scope: &str,
    orchestrator: &str,
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

    // From a feature, a bare `main` or orchestrator name names an agent of
    // the sender's own scope, not main's.
    if (recipient == scope::MAIN || recipient == orchestrator) && sender_scope != scope::MAIN {
        return format!(
            "\n  Hint: the orchestrator is `{orchestrator}@main`, the agent `{orchestrator}` in \
             the main scope. Use `pm msg send {orchestrator}@main` (or `<agent>@main`), or \
             `pm msg reply`"
        );
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
        let hint = agent_not_found_hint("main", "login", "login", "main");
        assert!(hint.contains("pm msg send main@main"), "{hint}");
    }

    #[test]
    fn hint_names_a_renamed_orchestrator() {
        for recipient in ["main", "boss"] {
            let hint = agent_not_found_hint(recipient, "login", "login", "boss");
            assert!(hint.contains("pm msg send boss@main"), "{hint}");
        }
    }

    #[test]
    fn hint_main_from_main_no_scope_suggestion() {
        // From main scope, "main" as recipient is just a missing agent, not a scope confusion
        let hint = agent_not_found_hint("main", "main", "main", "main");
        assert!(!hint.contains("main@main"));
        assert!(hint.contains("different scope"));
    }

    #[test]
    fn hint_different_scope_suggests_project() {
        let hint = agent_not_found_hint("reviewer", "login", "other-feat", "main");
        assert!(hint.contains("--project"));
    }

    #[test]
    fn hint_same_scope_default() {
        let hint = agent_not_found_hint("reviewer", "login", "login", "main");
        assert!(hint.contains("different scope"));
        assert!(hint.contains("reviewer@<scope>"));
    }
}
