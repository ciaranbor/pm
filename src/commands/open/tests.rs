//! `open` end to end, against a real tmux server.

mod hook;
mod launch;
mod open_all;
mod respawn;
mod sessions;

use super::*;
use crate::commands::{feat_new, init};
use crate::git;
use crate::state::agent::{AgentRegistry, AgentType};
use crate::testing::TestServer;
use tempfile::tempdir;
