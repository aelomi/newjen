//! Newjen's core: everything except the UI.
//!
//! - [`protocol`]: the Claude Code CLI's stream-json wire format.
//! - [`session`]: starts and talks to one `claude` process.
//! - [`transcript`]: turns session events into a conversation a UI can render.
//! - [`orphans`]: cleans up `claude` processes left behind by a crash.
//!
//! This crate must never depend on GPUI or Ely, so the UI layer could be replaced and the core
//! can be tested headlessly.

pub mod orphans;
pub mod protocol;
pub mod session;
pub mod transcript;

pub use protocol::{Incoming, PermissionAnswer, PermissionRequest, allows_for_session};
pub use session::{Session, SessionConfig, SessionEvent, find_claude};
pub use transcript::{Item, Status, ToolState, Transcript};
