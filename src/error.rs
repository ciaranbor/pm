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

    #[error("Feature already exists: {0}")]
    FeatureAlreadyExists(String),

    #[error("Not inside a pm project")]
    NotInProject,

    #[error("Not in a feature worktree — provide a feature name explicitly")]
    NotInFeatureWorktree,

    #[error("Not in a feature or main worktree — run from a worktree directory")]
    NotInWorktree,

    #[error("Path already exists: {0}")]
    PathAlreadyExists(PathBuf),

    #[error("Not a git repository: {0}")]
    NotAGitRepo(PathBuf),

    #[error("Repo already registered as project \"{0}\"")]
    RepoAlreadyRegistered(String),

    #[error("Invalid feature name \"{0}\": must not contain '/'")]
    InvalidFeatureName(String),

    #[error(
        "Branch '{branch}' is already checked out in worktree '{worktree}' — use --from to replace it"
    )]
    WorktreeConflict { branch: String, worktree: PathBuf },

    #[error("Git error: {0}")]
    Git(String),

    #[error("tmux error: {0}")]
    Tmux(String),

    #[error("gh error: {0}")]
    Gh(String),

    #[error("Skill not found: {0}")]
    SkillNotFound(String),

    #[error("Agent definition not found: {0}")]
    AgentNotFound(String),

    #[error("Invalid agent name: {0}")]
    InvalidAgentName(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Could not determine home directory")]
    NoHomeDir,
}

pub type Result<T> = std::result::Result<T, PmError>;
