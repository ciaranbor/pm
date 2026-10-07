use pm::commands;
use pm::error::{PmError, Result};

use super::scope::{optional_project_root, resolve_scope};
use crate::cli::WorkflowCommands;

pub(super) fn run(cmd: WorkflowCommands) -> Result<()> {
    // `install`/`uninstall`/`list` act on the global tier, so they
    // work outside a project too; `show` needs the feature's scope.
    let project_root = optional_project_root()?;
    match cmd {
        WorkflowCommands::Show => {
            let project_root = project_root.ok_or(PmError::NotInProject)?;
            let scope = resolve_scope(&project_root)?;
            match commands::workflow::show(&project_root, &scope)? {
                Some(body) => {
                    // Use print! (not println!) to avoid adding a
                    // trailing blank line — workflow.md already
                    // ends in a newline.
                    print!("{body}");
                    if !body.ends_with('\n') {
                        println!();
                    }
                }
                None => {
                    println!("No workflow active for this feature.");
                }
            }
            Ok(())
        }
        WorkflowCommands::List => {
            let out = commands::workflow::list_rows(project_root.as_deref())?;
            // Print rows to stdout (the normal listing).
            if out.rows.is_empty() {
                println!("No workflows installed. Run `pm upgrade` to install bundled workflows.");
            } else {
                for line in &out.rows {
                    println!("{line}");
                }
            }
            // Print warnings to stderr so they don't pollute pipe
            // consumers but still surface broken workflows.
            for w in &out.warnings {
                eprintln!("{w}");
            }
            Ok(())
        }
    }
}
