//! Event-handler modules carved out of `main.rs::run`.
//!
//! Each submodule owns a focused slice of the input dispatch:
//! - `tasks`: thread-spawn helpers for AI summary / JSONL regeneration.
//! - (future phases) `keyboard`, `mouse`, `paste` for popup-state-specific handlers.

pub(crate) mod keyboard;
pub(crate) mod mcp_popup;
pub(crate) mod mouse;
pub(crate) mod pane;
pub(crate) mod tasks;
