//! Newjen's core: everything except the UI.
//!
//! - [`env`]: login-shell PATH and installed CLI lookup.
//! - [`providers`]: usage-free sign-in, account, model and limit detection.
//! - [`protocol`]: the Claude Code CLI's stream-json wire format.
//! - [`session`]: starts and talks to one `claude` process.
//! - [`transcript`]: turns session events into a conversation a UI can render.
//! - [`thread`]: one conversation with its process, transcript, grants and saving.
//! - [`store`]: the SQLite database that keeps threads across relaunches.
//! - [`orphans`]: cleans up `claude` processes left behind by a crash.
//! - [`worktree`]: makes and removes the git worktrees threads can work in.
//! - [`usage`]: the latest plan and usage-window report each CLI gave.
//!
//! This crate must never depend on GPUI or GPUI Kit, so the UI layer could be replaced and
//! the core can be tested headlessly.

pub mod agent;
mod approval_text;
pub mod changes;
pub mod commit_message;
pub mod edits;
pub mod env;
pub mod instance;
pub mod orphans;
mod probe;
pub mod protocol;
pub mod providers;
pub mod session;
pub mod steps;
pub mod store;
pub mod thread;
pub mod transcript;
pub mod usage;
pub mod vcs;
pub mod worktree;

pub use agent::{
    ApprovalIntent, ApprovalKey, ApprovalKind, ApprovalRequest, ApprovalRisk, ApprovalView,
    GrantId, GrantLifetime, GrantView, OnceReason, RememberScope, RuleMeaning, ToolDisposition,
};
pub use env::{Cli, find_cli, resolved_path};
pub use session::{Conversation, Session, SessionConfig, SessionEvent};
pub use store::{Grant, Project, Store, ThreadRow, WriteError};
pub use thread::{Thread, ThreadEvent};
pub use transcript::{Item, Status, ToolState, Transcript};

pub use providers::claude::presentation::tool_disposition;

pub use agent::{AgentConfig, AgentPresentation, ConversationState, EffortSelection, Speed};

pub mod picker;
