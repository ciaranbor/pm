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

    #[error("Path already exists: {0}")]
    PathAlreadyExists(PathBuf),

    #[error("Not a git repository: {0}")]
    NotAGitRepo(PathBuf),

    #[error("Repo already registered as project \"{0}\"")]
    RepoAlreadyRegistered(String),

    #[error("Invalid feature name \"{0}\": must not contain '/'")]
    InvalidFeatureName(String),

    #[error("Git error: {0}")]
    Git(String),

    #[error("tmux error: {0}")]
    Tmux(String),

    #[error("gh error: {0}")]
    Gh(String),
}

pub type Result<T> = std::result::Result<T, PmError>;
