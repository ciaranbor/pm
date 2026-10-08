use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PmError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML serialization error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("TOML deserialization error: {0}")]
    TomlDeserialize(#[from] toml::de::Error),

    #[error("Project not found: {0}")]
    ProjectNotFound(String),

    #[error("Feature not found: {0}")]
    FeatureNotFound(String),

    #[error("Branch not found: {0}")]
    BranchNotFound(String),

    #[error(
        "base branch '{0}' is not checked out in this project (neither the main branch nor a feature's)"
    )]
    BaseNotCheckedOut(String),

    #[error("Feature already exists: {0}")]
    FeatureAlreadyExists(String),

    #[error("Not inside a pm project")]
    NotInProject,

    #[error("project '{name}' is not on this machine: run `pm restore {name}`")]
    NotHere { name: String },

    #[error(
        "{} holds pm state but no main checkout: `pm restore` or `pm delete` its project",
        .0.display()
    )]
    NotRestoredRoot(PathBuf),

    #[error("Not in a feature worktree — provide a feature name explicitly")]
    NotInFeatureWorktree,

    #[error("Not in a feature or main worktree — run from a worktree directory")]
    NotInWorktree,

    #[error("Cannot derive a project name from git URL \"{0}\": pass a PATH")]
    UnnamedGitUrl(String),

    #[error("Path already exists: {0}")]
    PathAlreadyExists(PathBuf),

    #[error("Not a git repository: {0}")]
    NotAGitRepo(PathBuf),

    #[error("HEAD is detached in {0}: check out the branch pm should record as main, then retry")]
    DetachedHead(PathBuf),

    #[error("Repo already registered as project \"{0}\"")]
    RepoAlreadyRegistered(String),

    #[error(
        "A project named \"{name}\" is already registered, at {}: \
         choose another name, or `pm delete {name}` first",
        .root.display()
    )]
    ProjectNameTaken { name: String, root: PathBuf },

    #[error(
        "Invalid project root \"{0}\": must be an absolute path or start with `~/` \
         (relative paths in the registry resolve against each caller's CWD, \
         which silently corrupts cross-project operations like messaging)"
    )]
    InvalidProjectRoot(String),

    #[error("Invalid feature name \"{0}\": must not contain '/'")]
    InvalidFeatureName(String),

    #[error(
        "Branch '{branch}' is already checked out in worktree '{worktree}' — use --from to replace it"
    )]
    WorktreeConflict { branch: String, worktree: PathBuf },

    #[error("Git error: {0}")]
    Git(String),

    #[error("{0}")]
    SafetyCheck(String),

    /// A refusal to lose work whose way out is a terminal command: `cli`
    /// words it for the CLI, `remote` for a device, which can't run it.
    #[error("{reason} {cli}")]
    Unsafe {
        reason: String,
        cli: String,
        remote: String,
    },

    /// A merge git refused, then aborted: nothing changed.
    #[error("{0}")]
    MergeAborted(String),

    #[error("tmux error: {0}")]
    Tmux(String),

    #[error("gh error: {0}")]
    Gh(String),

    #[error("Skill not found: {0}")]
    SkillNotFound(String),

    #[error("Baseline not found: {0}")]
    BaselineNotFound(String),

    #[error("harness '{value}' is not supported yet; supported: {supported}")]
    HarnessUnsupported { value: String, supported: String },

    #[error(
        "Workflow not found: {0}\n  Hint: run `pm workflow list` to see installed workflows, \
         or run `pm upgrade` to install the bundled ones."
    )]
    WorkflowNotFound(String),

    #[error(
        "{kind} '{name}' is bundled but disabled by `[bundled.disable] {key}` in the global pm config.\n  \
         Hint: add a project custom at {custom}, or remove '{name}' from that list and run \
         `pm upgrade`."
    )]
    BundledDisabled {
        kind: &'static str,
        key: &'static str,
        name: String,
        custom: String,
    },

    #[error(
        "Workflow '{workflow}' lists '{agent}' in its agent team, but no agent definition was found at \
         {}. Install the definition or fix the workflow.",
        display_paths(.searched)
    )]
    WorkflowAgentMissing {
        workflow: String,
        agent: String,
        searched: Vec<PathBuf>,
    },

    #[error(
        "Workflow '{workflow}' lists 'default', which is no longer the vanilla agent and has \
         no definition. Rename it to 'plain' in the workflow's config.toml."
    )]
    WorkflowNamesLegacyVanilla { workflow: String },

    #[error("Agent error: {0}")]
    Agent(String),

    #[error("Agent definition not found: {0}")]
    AgentNotFound(String),

    #[error(
        "No agent definition '{agent}' found at {}. \
         Pass --agent with an installed definition, or install the definition file.",
        display_paths(.searched)
    )]
    AgentDefinitionMissing {
        agent: String,
        searched: Vec<PathBuf>,
    },

    #[error(
        "No agent definition 'default': the vanilla agent is now 'plain'. Run `pm upgrade` to \
         migrate agents spawned as 'default', or spawn 'plain'."
    )]
    UnmigratedVanillaAgent,

    #[error("Invalid agent name: {0}")]
    InvalidAgentName(String),

    #[error("{0}")]
    Messaging(String),

    #[error("{0}")]
    Summary(String),

    #[error("Export/import error: {0}")]
    ExportImport(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Could not determine home directory")]
    NoHomeDir,

    #[error("{0}")]
    Serve(String),

    #[error("editor: {0}")]
    Editor(String),

    #[error("self-update: {0}")]
    SelfUpdate(String),

    #[error("Transcript unreadable: {0}")]
    Transcript(String),
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl PmError {
    /// The message without an [`PmError::Agent`]'s kind prefix, for a
    /// report that frames it itself.
    pub fn reason(self) -> String {
        match self {
            PmError::Agent(message) => message,
            e => e.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, PmError>;
