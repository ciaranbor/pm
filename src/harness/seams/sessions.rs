//! Session seams: where an agent's conversation is read from, and moving
//! sessions between directories and machines.
//!
//! Session portability (`pm harness migrate|export|import`) is three seams
//! over each harness's own store. All of them preserve session ids, so a
//! registry entry stays valid across a move; none opens a harness's database.

use std::path::Path;

use crate::error::Result;
use crate::harness::session_store::Location;
use crate::harness::{
    AgentSession, Conversation, ExportJob, Harness, ImportOutcome, InUse, SessionStore,
    claude_code, codex, opencode,
};
use crate::state::paths;
use crate::state::project::HarnessConfig;
use crate::state::runtime::{self, SessionPath};

impl Harness {
    /// Where the conversation of `agent`'s current session is read from;
    /// `None` until it has one. The transcript path the session reported at
    /// its start is preferred to the one derived from its id.
    pub fn conversation(self, agent: &AgentSession<'_>) -> Option<Conversation> {
        let recorded = || {
            runtime::read_session_path(
                agent.project_root,
                agent.scope,
                agent.name,
                SessionPath::Transcript,
            )
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().contains(agent.session_id))
            })
        };
        let jsonl = |path, parse| Location::Jsonl { path, parse };
        let location = match self {
            Harness::ClaudeCode => {
                let config_dir = runtime::read_session_path(
                    agent.project_root,
                    agent.scope,
                    agent.name,
                    SessionPath::ConfigDir,
                );
                jsonl(
                    recorded().or_else(|| {
                        claude_code::chat::transcript_path(agent, config_dir.as_deref())
                    })?,
                    claude_code::chat::parse,
                )
            }
            Harness::Codex => jsonl(
                recorded().or_else(|| {
                    codex::sessions::find_rollout(&codex::home_dir(agent.home), agent.session_id)
                })?,
                codex::chat::parse,
            ),
            Harness::OpenCode => {
                let db = opencode::chat::db_path(agent.home, paths::data_home().as_deref());
                if agent.session_id.is_empty() || !db.is_file() {
                    return None;
                }
                Location::Session {
                    db,
                    id: agent.session_id.to_string(),
                }
            }
        };
        Some(Conversation {
            harness: self,
            location,
        })
    }

    /// The harness's name in an export's file and root directory names.
    pub fn export_tag(self) -> &'static str {
        match self {
            Harness::ClaudeCode => claude_code::EXPORT_TAG,
            Harness::Codex => codex::EXPORT_TAG,
            Harness::OpenCode => opencode::EXPORT_TAG,
        }
    }

    /// Why the harness's session store cannot be reached on this machine,
    /// for a caller that carries sessions as a side effect and must not fail
    /// over a harness that is configured but not installed. `None` for a
    /// store pm reads as files.
    pub fn sessions_unreachable(self, config: &HarnessConfig) -> Option<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => None,
            Harness::OpenCode => opencode::sessions::unreachable(&config.opencode),
        }
    }

    /// Whether the harness has any session recorded at `dir` that a
    /// migration could act on; never true for codex, which needs none.
    pub fn has_sessions(self, store: &SessionStore<'_>, dir: &Path) -> Result<bool> {
        match self {
            Harness::ClaudeCode => Ok(claude_code::sessions::has_sessions(
                &store.claude_base(),
                dir,
            )),
            Harness::Codex => Ok(false),
            Harness::OpenCode => opencode::sessions::has_sessions(&store.config.opencode, dir),
        }
    }

    /// Make the sessions recorded at `from` resumable at `to`, which must
    /// exist. Sessions named in `in_use` have an agent running on them and
    /// are left where they are, each reported with its agent.
    pub fn migrate_sessions(
        self,
        store: &SessionStore<'_>,
        from: &Path,
        to: &Path,
        in_use: &[InUse],
    ) -> Result<Vec<String>> {
        match self {
            Harness::ClaudeCode => {
                claude_code::sessions::migrate_sessions(&store.claude_base(), from, to, in_use)
            }
            Harness::Codex => Ok(vec![codex::sessions::MIGRATE_NOTE.to_string()]),
            Harness::OpenCode => {
                opencode::sessions::migrate(&store.config.opencode, from, to, in_use)
            }
        }
    }

    /// Write the sessions recorded at each job's `dir` into its `staging`,
    /// which is created when there are any. Returns, per job, what was
    /// written, for the report; `None` when its `dir` has no sessions.
    pub fn export_sessions(
        self,
        home: &Path,
        jobs: &[ExportJob<'_>],
    ) -> Result<Vec<Option<String>>> {
        match self {
            Harness::ClaudeCode => jobs
                .iter()
                .map(|job| {
                    claude_code::sessions::export(
                        &SessionStore {
                            home,
                            config: job.config,
                        }
                        .claude_base(),
                        job.dir,
                        &job.staging,
                    )
                })
                .collect(),
            Harness::Codex => {
                let targets: Vec<(&Path, &Path)> = jobs
                    .iter()
                    .map(|job| (job.dir, job.staging.as_path()))
                    .collect();
                codex::sessions::export(&codex::home_dir(home), &targets)
            }
            Harness::OpenCode => jobs
                .iter()
                .map(|job| opencode::sessions::export(&job.config.opencode, job.dir, &job.staging))
                .collect(),
        }
    }

    /// Install what [`export_sessions`](Self::export_sessions) wrote to
    /// `staging` for the directory `from` as sessions of `to`. Never
    /// replaces a session the store already has.
    pub fn import_sessions(
        self,
        store: &SessionStore<'_>,
        staging: &Path,
        from: &Path,
        to: &Path,
    ) -> Result<ImportOutcome> {
        match self {
            Harness::ClaudeCode => {
                claude_code::sessions::import(&store.claude_base(), staging, from, to)
            }
            Harness::Codex => codex::sessions::import(&codex::home_dir(store.home), staging),
            Harness::OpenCode => opencode::sessions::import(&store.config.opencode, staging, to),
        }
    }
}
