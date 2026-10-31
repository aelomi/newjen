//! Newjen's core: everything except the UI.
//!
//! - [`protocol`]: the Claude Code CLI's stream-json wire format.
//! - [`session`]: starts and talks to one `claude` process.
//! - [`transcript`]: turns session events into a conversation a UI can render.
//! - [`thread`]: one conversation with its process, transcript, grants and saving.
//! - [`store`]: the SQLite database that keeps threads across relaunches.
//! - [`orphans`]: cleans up `claude` processes left behind by a crash.
//! - [`rift`]: makes and removes the Rift copies threads can work in.
//!
//! This crate must never depend on GPUI or GPUI Kit, so the UI layer could be replaced and
//! the core can be tested headlessly.

pub mod orphans;
pub mod protocol;
pub mod rift;
pub mod session;
pub mod store;
pub mod thread;
pub mod transcript;

pub use protocol::{
    Incoming, OnceReason, PermissionAnswer, PermissionRequest, SessionRule, SessionScope,
    allows_for_session, session_rules, session_scope,
};
pub use session::{Conversation, Session, SessionConfig, SessionEvent, find_claude};
pub use store::{Grant, Project, Store, ThreadRow, WriteError};
pub use thread::{Thread, ThreadEvent};
pub use transcript::{Item, Status, ToolState, Transcript};
