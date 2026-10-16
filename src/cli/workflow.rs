use clap::Subcommand;

#[derive(Subcommand)]
pub enum WorkflowCommands {
    /// Print the active workflow's routing prose (workflow.md)
    Show,
    /// List installed workflows (project tier first, then global) with
    /// their descriptions
    List,
}
